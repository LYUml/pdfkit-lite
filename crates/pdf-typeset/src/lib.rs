//! Typesetting for annotation text, backed by the embedded Typst engine.
//!
//! Why Typst instead of hand-rolled font embedding: Chinese text in a PDF annotation
//! needs a font with CJK coverage, a subset of that font, a `/ToUnicode` map, correct
//! line breaking and punctuation rules. Typst already does all of it, in pure Rust.
//!
//! The pipeline is: text -> Typst source -> one-page PDF -> a Form XObject that an
//! annotation can reference as its appearance (`/AP /N`).


mod merge;
pub use merge::{MergeError, MergedAppearance, merge_first_page};

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

#[derive(Debug, thiserror::Error)]
pub enum TypesetError {
    #[error("no usable font found from the candidate list")]
    NoFont,
    #[error("typesetting failed: {0}")]
    Compile(String),
    #[error("PDF export failed: {0}")]
    Export(String),
    #[error("the produced page could not be extracted: {0}")]
    Extract(String),
}

/// Fonts the typesetter can use, loaded once and reused.
///
/// Order matters: the first face that covers a glyph wins. Embeddable open-source faces
/// should come before system faces, because system fonts cannot be redistributed inside
/// the PDFs we produce.
#[derive(Debug)]
pub struct FontSet {
    fonts: Vec<Font>,
    book: LazyHash<FontBook>,
}

impl FontSet {
    /// Load every face of the given files, skipping anything that fails to parse.
    pub fn from_files(paths: &[std::path::PathBuf]) -> Self {
        let mut fonts = Vec::new();
        for path in paths {
            let Ok(data) = std::fs::read(path) else {
                continue;
            };
            let bytes = Bytes::new(data);
            // A `.ttc` holds several faces; stop at the first index that fails.
            for index in 0.. {
                match Font::new(bytes.clone(), index) {
                    Some(font) => fonts.push(font),
                    None => break,
                }
            }
        }
        let book = LazyHash::new(FontBook::from_fonts(fonts.iter()));
        Self { fonts, book }
    }

    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    /// Family names, for logging and for letting the user pick a face.
    pub fn families(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .fonts
            .iter()
            .map(|f| f.info().family.to_string())
            .collect();
        out.dedup();
        out
    }

    fn preferred_family(&self) -> Option<String> {
        self.families().into_iter().next()
    }
}

/// Fonts to try, in priority order: bundled open-source faces first, then the system.
///
/// Bundled faces are redistributable inside the generated PDFs; system fonts usually are
/// not, so they are a convenience fallback rather than the default.
pub fn system_font_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    // 1. Fonts shipped next to the executable or in a `fonts/` directory.
    for dir in [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("fonts"))),
        Some(std::path::PathBuf::from("fonts")),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut found: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|e| e.to_str()),
                        Some("ttf" | "otf" | "ttc" | "otc")
                    )
                })
                .collect();
            found.sort();
            out.extend(found);
        }
    }
    // 2. Platform fallbacks.
    #[cfg(target_os = "macos")]
    {
        out.push("/System/Library/Fonts/STHeiti Light.ttc".into());
        out.push("/System/Library/Fonts/Hiragino Sans GB.ttc".into());
        out.push("/System/Library/Fonts/Supplemental/Arial Unicode.ttf".into());
    }
    #[cfg(target_os = "windows")]
    {
        out.push("C:\\Windows\\Fonts\\msyh.ttc".into());
        out.push("C:\\Windows\\Fonts\\simhei.ttf".into());
        out.push("C:\\Windows\\Fonts\\arial.ttf".into());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        out.push("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into());
        out.push("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into());
    }
    out
}

/// A single-page document produced by the typesetter, ready to become a form XObject.
#[derive(Debug)]
pub struct TypesetPage {
    /// Width in PDF points.
    pub width: f32,
    /// Height in PDF points.
    pub height: f32,
    /// The complete one-page PDF, uncompressed enough to extract from.
    pub pdf: Vec<u8>,
}

/// Minimal `World` that serves one in-memory source and a fixed font set.
struct TextWorld<'a> {
    library: LazyHash<Library>,
    book: &'a LazyHash<FontBook>,
    fonts: &'a [Font],
    source: Source,
}

impl World for TextWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.book
    }

    fn main(&self) -> FileId {
        self.source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(FileError::NotFound("missing".into()))
        }
    }

    fn file(&self, _id: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound("missing".into()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        None
    }
}

/// Escape a string for use inside Typst markup.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '#' | '$' | '*' | '_' | '`' | '<' | '>' | '@' | '[' | ']' | '=' | '+' | '-'
            | '/' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Typeset `text` into a single-page PDF sized to `width_pt`, with wrapping.
///
/// The page is rendered with a transparent background so that when it is embedded as an
/// annotation appearance the page underneath stays visible.
pub fn typeset(
    fonts: &FontSet,
    text: &str,
    width_pt: f32,
    font_size: f32,
    color: [f32; 3],
) -> Result<TypesetPage, TypesetError> {
    if fonts.is_empty() {
        return Err(TypesetError::NoFont);
    }
    let family = fonts
        .preferred_family()
        .ok_or(TypesetError::NoFont)?;
    // `height: auto` lets the page grow to fit the text; width is fixed by the caller.
    let src = format!(
        concat!(
            "#set page(width: {w}pt, height: auto, margin: 0pt, fill: none, background: none)\n",
            "#set text(font: \"{family}\", size: {size}pt, fill: rgb({r}, {g}, {b}))\n",
            "#set par(leading: 0.6em, justify: false)\n",
            "{body}\n",
        ),
        w = width_pt,
        family = family.replace('"', ""),
        size = font_size,
        // Typst's rgb() takes integers 0-255 or ratios, never floats: convert here.
        r = (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        g = (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        b = (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        body = escape_markup(text),
    );

    let main = FileId::new(RootedPath::new(
        VirtualRoot::Project,
        VirtualPath::new("text.typ").map_err(|e| TypesetError::Extract(e.to_string()))?,
    ));
    let source = Source::new(main, src);
    let world = TextWorld {
        library: LazyHash::new(Library::default()),
        book: &fonts.book,
        fonts: &fonts.fonts,
        source,
    };

    let warned = typst::compile::<PagedDocument>(&world);
    let document = warned
        .output
        .map_err(|errors| TypesetError::Compile(format!("{errors:?}")))?;

    let options = typst_pdf::PdfOptions::default();
    let pdf = typst_pdf::pdf(&document, &options)
        .map_err(|errors| TypesetError::Export(format!("{errors:?}")))?;

    let first = document
        .pages()
        .first()
        .ok_or_else(|| TypesetError::Extract("no page produced".to_owned()))?;
    let size = first.frame.size();
    Ok(TypesetPage {
        width: size.x.to_pt() as f32,
        height: size.y.to_pt() as f32,
        pdf,
    })
}

impl TypesetPage {
    /// Extract `(content_stream, resource_dict_value)` for the first page of the produced PDF.
    ///
    /// This is deliberately narrow: we only read pages Typst itself wrote, so a small
    /// hand-rolled reader is enough and keeps the dependency surface tiny.
    pub fn first_page_streams(&self) -> Result<ExtractedPage, TypesetError> {
        extract_first_page(&self.pdf)
    }
}

/// What we need from the interim PDF to build a form XObject.
#[derive(Debug, Clone)]
pub struct ExtractedPage {
    /// Decoded page content stream operators.
    pub content: Vec<u8>,
    /// Resource dictionary contents serialized back to PDF syntax (`/Font<<...>>` ...).
    pub resources: String,
}

fn extract_first_page(pdf: &[u8]) -> Result<ExtractedPage, TypesetError> {
    let doc = lopdf::Document::load_mem(pdf)
        .map_err(|e| TypesetError::Extract(format!("interim PDF unreadable: {e}")))?;
    let pages = doc.get_pages();
    let (&_number, &page_id) = pages
        .iter()
        .next()
        .ok_or_else(|| TypesetError::Extract("no page in interim PDF".to_owned()))?;

    let content = doc.get_page_content(page_id);

    // Copy the page's /Resources so embedded fonts come along with the form XObject.
    let resources = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get_deref(b"Resources", &doc).ok().cloned())
        .map(|obj| match obj {
            lopdf::Object::Reference(id) => doc
                .get_dictionary(id)
                .map(|d| format!("{d:?}"))
                .unwrap_or_default(),
            lopdf::Object::Dictionary(d) => format!("{d:?}"),
            _ => String::new(),
        })
        .unwrap_or_default();

    Ok(ExtractedPage { content, resources })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup() {
        assert_eq!(escape_markup("a*b#c"), "a\\*b\\#c");
    }

}
