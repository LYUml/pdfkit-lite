# pdfkit-lite

A lightweight, Preview-like PDF reader and annotator in pure Rust.

Target: macOS first, Windows and Linux alongside. No C dependencies, no bundled
PDFium, no document upload — everything runs locally.

## Status

Early skeleton. The render-and-annotate loop works end to end; see
[Limitations](#limitations) for what is not there yet.

## What works today

- Open a PDF from the command line or the system file picker
- Render pages off the UI thread, so the window stays responsive
- Place checkboxes, toggle them, and save them as standard PDF `Square` annotations
- Place text boxes as standard PDF `FreeText` annotations; text is typeset by
  Typst, so Chinese, Japanese and Korean display correctly with embedded fonts
- Draw handwritten signatures as standard PDF `Ink` annotations
- Save a **copy** (`<name>-edited.pdf`) and never overwrite the original
- Hand the result to the system PDF reader (Preview on macOS)
- `--selftest` mode for headless verification

Every annotation we write is plain ISO 32000, so Preview, Acrobat and browser
viewers all show it, and the file stays editable after it leaves this app.

## Stack

| Concern | Choice | Why |
| --- | --- | --- |
| Rendering | [hayro](https://crates.io/crates/hayro) 0.8 | Pure Rust, output matches PDFium glyph for glyph on real documents |
| Document I/O | [lopdf](https://crates.io/crates/lopdf) 0.46 | Solid read/write, no C |
| UI | [eframe](https://crates.io/crates/eframe) / egui 0.37 | One API for desktop, cheap texture upload for page bitmaps |
| Typesetting | [Typst](https://crates.io/crates/typst) 0.15 | Embedded as a library; handles CJK fonts, subsets and line breaking for us |

## Build and run

```bash
cargo build --release

# GUI
./target/release/pdfkit-lite path/to/file.pdf

# Headless check: writes demo annotations and exits
./target/release/pdfkit-lite --selftest in.pdf out.pdf
```

Requires Rust 1.92 or newer.

## Layout

```
crates/
  pdf-doc/       open documents, page geometry, coordinate conversion
  pdf-render/    hayro rasterization plus a background render worker
  pdf-annot/     annotation builders and appearance streams
  pdf-typeset/   Typst-backed typesetting for annotation text
  pdf-font/      reserved for font discovery
apps/
  desktop/       the egui application
```

The core crates know nothing about the UI, so a CLI or a different toolkit can
reuse them.

## Notes on appearance streams

Writing annotation appearances has a few traps that cost real debugging time.
They are documented at the top of `crates/pdf-annot/src/lib.rs`; the short
version:

1. `/AP /N` must be an indirect reference. An inline stream inside the
   annotation dictionary is silently ignored by PDFium.
2. Do not set `/Matrix` on the form; viewers already map `BBox` onto `Rect`.
3. Normalize `/Rect` so `x0 <= x1` and `y0 <= y1`.
4. Draw shapes as filled paths; stroked polylines render inconsistently.
5. Appearance space has its origin at the bottom-left with y pointing up, while
   most UIs use top-left with y down.

## Verified across renderers

The same document renders identically under PDFium and under Apple's own PDF
renderer (the one Preview uses), checked through Quick Look: checkboxes, CJK
text and ink signatures all display.

## Limitations

- Text boxes carry a fixed placeholder; there is no text input UI yet
- Signed as a *visual* signature, not a cryptographic one. See the note below
- Encrypted PDFs are not supported (a hayro limitation)
- No undo, no dragging annotations, no deletion
- No thumbnail sidebar and no continuous scrolling; one page at a time
- `pdf-font` is a placeholder crate

## On signatures

What we write is a visual signature: an `Ink` annotation that looks like a
signature. It is **not** a cryptographic digital signature and carries no
tamper evidence. For legally binding e-signatures, use PAdES / CAdES with a
certificate instead.

## License

MIT. See [LICENSE](LICENSE).
