//! PDF document model and page geometry.
//!
//! This crate owns the file: opening it, reporting page boxes, and saving an edited copy.
//! It deliberately does not render and does not know about UI state.

use std::path::{Path, PathBuf};

pub use lopdf;
use lopdf::Document;

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("could not read or write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the PDF could not be written back: {0}")]
    Save(String),
    #[error("the PDF could not be parsed: {0}")]
    Parse(#[from] lopdf::Error),
}

/// Page geometry in PDF user space (points, origin bottom-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PageBox {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// Convert a point in "visible page coordinates" (top-left origin, y down) to PDF
    /// user space (bottom-left origin, y up). Used when writing annotations, which always
    /// live in user space.
    pub fn visible_to_user(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x0 + x, self.y1 - y)
    }

    /// Inverse of [`PageBox::visible_to_user`].
    pub fn user_to_visible(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.x0, self.y1 - y)
    }
}

/// An opened PDF, mutable so annotations (and later page operations) can be applied.
pub struct PdfDocument {
    doc: Document,
    path: Option<PathBuf>,
    /// Page index (0-based) -> object id, captured at open time.
    page_ids: Vec<lopdf::ObjectId>,
}

impl std::fmt::Debug for PdfDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfDocument")
            .field("path", &self.path)
            .field("pages", &self.page_ids.len())
            .finish()
    }
}

impl PdfDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DocError> {
        let path = path.as_ref().to_path_buf();
        let doc = Document::load(&path).map_err(|source| match source {
            lopdf::Error::IO(e) => DocError::Io {
                path: path.clone(),
                source: e,
            },
            other => DocError::Save(other.to_string()),
        })?;
        let page_ids = ordered_page_ids(&doc);
        Ok(Self {
            doc,
            path: Some(path),
            page_ids,
        })
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, DocError> {
        let doc = Document::load_mem(&bytes)?;
        let page_ids = ordered_page_ids(&doc);
        Ok(Self {
            doc,
            path: None,
            page_ids,
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn page_count(&self) -> usize {
        self.page_ids.len()
    }

    /// The page's media box in user space.
    pub fn page_box(&self, index: usize) -> Option<PageBox> {
        let id = *self.page_ids.get(index)?;
        let page = self.doc.get_dictionary(id).ok()?;
        for key in [b"MediaBox".as_slice(), b"CropBox"] {
            if let Ok(arr) = page.get_deref(key, &self.doc).and_then(|o| o.as_array()) {
                if arr.len() == 4 {
                    let v: Vec<f32> = arr
                        .iter()
                        .filter_map(|o| o.as_f32().ok().or_else(|| o.as_i64().ok().map(|i| i as f32)))
                        .collect();
                    if v.len() == 4 {
                        return Some(PageBox {
                            x0: v[0],
                            y0: v[1],
                            x1: v[2],
                            y1: v[3],
                        });
                    }
                }
            }
        }
        // US Letter fallback, matching what most viewers assume when the box is missing.
        Some(PageBox {
            x0: 0.0,
            y0: 0.0,
            x1: 612.0,
            y1: 792.0,
        })
    }

    pub fn page_id(&self, index: usize) -> Option<lopdf::ObjectId> {
        self.page_ids.get(index).copied()
    }

    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.doc
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// Save to `path`, compressing object streams.
    pub fn save_to(&mut self, path: impl AsRef<Path>) -> Result<(), DocError> {
        let path = path.as_ref().to_path_buf();
        self.doc.compress();
        self.doc
            .save(&path)
            .map(|_| ())
            .map_err(|source| DocError::Io { path, source })
    }

    /// Bytes of the current document, for handing straight to the renderer.
    pub fn to_bytes(&mut self) -> Result<Vec<u8>, DocError> {
        self.doc.compress();
        let mut out = Vec::new();
        self.doc
            .save_to(&mut out)
            .map_err(|source| DocError::Save(source.to_string()))?;
        Ok(out)
    }
}

/// Page objects in reading order. `Document::get_pages` returns a `BTreeMap<u32, ObjectId>`,
/// which already sorts by page number.
fn ordered_page_ids(doc: &Document) -> Vec<lopdf::ObjectId> {
    doc.get_pages().into_values().collect()
}
