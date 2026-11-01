use pdf_typeset::{FontSet, typeset};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<std::path::PathBuf> = std::env::args().skip(1).map(std::path::PathBuf::from).collect();
    let fonts = FontSet::from_files(&paths);
    println!("fonts loaded: {} faces, families: {:?}", fonts.len(), fonts.families());
    // Deliberately mixed CJK + Latin: the typesetter must handle both, and these
    // characters exercise glyphs that a Latin-only face cannot cover.
    let text = std::env::var("PROBE_TEXT").unwrap_or_else(|_| "Sample text 2026-11-01".to_owned());
    let page = typeset(&fonts, &text, 260.0, 11.0, [0.1, 0.1, 0.1])?;
    println!("typeset -> {}x{:.1}pt, pdf {} bytes", page.width, page.height, page.pdf.len());
    let ex = page.first_page_streams()?;
    println!("content {} bytes, resources {} bytes", ex.content.len(), ex.resources.len());
    std::fs::write("/tmp/typeset_probe.pdf", &page.pdf)?;
    Ok(())
}
