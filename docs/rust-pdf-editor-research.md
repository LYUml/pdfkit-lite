# Pure-Rust PDF reader/editor — research and architecture decisions

Date: 2026-11-01
Environment used for all measurements: macOS 27.0.1 (26A434), arm64e, Rust 1.99.0

Every claim marked "measured" below was run on this machine. Nothing here is
copied from documentation without verification.

---

## 1. Summary

**Pure Rust is viable. No C or C++ dependency, no bundled PDFium.**

| Capability | Decision | Measured evidence |
| --- | --- | --- |
| Rendering | hayro | Layout matches PDFium glyph for glyph on 5 real documents |
| Document I/O | lopdf | 3 annotations written in 2.6 ms, round trip intact |
| UI | eframe/egui | Window runs; 20.5 MB release binary |
| Typesetting | Typst | Chinese PDF in ~30 ms, embedded as a library |
| Document conversion | Pandoc (external process) | Markdown/LaTeX to .docx with Chinese intact |
| Agent control | rmcp | Compiles; server feature available |
| Cross-platform | one codebase | Typechecks for macOS, Windows (msvc), Linux (gnu) |

The hard part is **not** rendering. It is **writing Chinese text into a PDF in a
way that every reader displays correctly**. That drove most of the design.

---

## 2. Why pure Rust rather than PDFium

The two requirements "lightweight, like Preview" and "works on macOS, Windows and
Linux" are exactly where PDFium hurts.

| Dimension | Pure Rust (hayro + lopdf) | PDFium |
| --- | --- | --- |
| Dependencies | Zero C, `cargo build` and done | Must ship or compile a C++ library per platform |
| Size | Core 2.6 MB; 38.5 MB with GUI (LTO + strip) | libpdfium alone is 8 MB+ |
| Static linking | Possible, single file | Difficult |
| Cross-platform CI | One cross-compile (verified) | Three toolchains, prebuilt artifacts |
| Rendering maturity | Young (0.8); no encryption or blend modes | Industrial, powers Chrome |
| Text/annotation APIs | Must build ourselves | FPDFText_*, FPDFAnnot_* |

The PDFium route was built and measured too (`pdfium-render` with the
bblanchon prebuilt binary). It works and renders more broadly. But for this
product, buying one more renderer at the cost of an 8 MB+ C dependency, static
linking, and platform build complexity is not a good trade: hayro already
reproduces real documents exactly.

### Corroboration

`storytold/pdfcraft` (now `EquityBot/PlutarchPages`), 560k lines of Rust aiming
at Acrobat Pro parity, uses hayro for rendering:

```toml
# crates/render/Cargo.toml
hayro = { workspace = true }
hayro-syntax = { workspace = true }
```

---

## 3. Stack

| Layer | Choice | Version | Notes |
| --- | --- | --- | --- |
| Render | `hayro` | 0.8 | Bundles the 14 standard fonts and built-in CMaps |
| Read/write | `lopdf` | 0.46 | Round trip verified; `get_page_content` extracts page streams |
| Font parsing | `ttf-parser` | 0.25 | Handles `.ttc` collections |
| Font subsetting | `subsetter` | 0.2 | Only if we go back to hand-rolled embedding |
| Typesetting | `typst` + `typst-pdf` | 0.15 | Replaces hand-rolled embedding; see section 4 |
| GUI | `eframe`/`egui` | 0.37 | See section 3.1 for API caveats |

### 3.1 GUI trade-offs

egui is immediate mode. Upside: one API for desktop and web, fast compiles, no
native widget dependency, trivial texture upload — and a PDF page is just a
texture. Downside: it does not look native by default; a Preview-like feel means
drawing our own chrome.

Measured: `eframe 0.37` changed the `App` trait to
`fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame)`, panels are now
`egui::Panel::top(..)` (there is no `TopBottomPanel`), and `Ui::id` was renamed
`Ui::scope_id`. Most tutorials online still use the old shape and will not
compile.

Alternatives considered: Slint (more native feel, heavier tooling) and iced
(Elm architecture, more boilerplate for texture interaction). Plan: start with
egui; the document and render cores do not depend on the GUI, so a later swap is
contained.

### 3.2 What we deliberately do not build

- No embedded PDFium.
- No hand-written PDF parser (lopdf).
- No hand-written font engine (Typst covers it).
- No body-text reflow; annotations and overlays only.

---

## 4. The real problem: Chinese text in annotations

### 4.1 What we hit

A `FreeText` annotation was written with the standard 14 font Helvetica
(`/DA (0 g /Helv 11 Tf)`). Rendering with PDFium:

- The box and border drew correctly.
- The Chinese text rendered as empty boxes.

Three separate causes:

1. `/Helv` was not defined in the page resources; lopdf writes the annotation
   dictionary but does not fix up resources.
2. Standard 14 fonts are single-byte (WinAnsi) and cannot carry CJK.
3. Even if defined, Helvetica has no CJK glyphs.

This matches an upstream report: [pdf.js issue #20117](https://github.com/mozilla/pdf.js/issues/20117)
describes FreeText annotations with Chinese characters failing to display in
PDFium-based viewers for the same reason: missing appearance streams and
encoding problems.

### 4.2 Solution: let Typst do it

Correct PDF for Chinese text needs an embedded font, a glyph subset, a
`/ToUnicode` map, and correct line breaking and punctuation. Typst already does
all of it, in pure Rust, in about 30 ms.

```
user text
  -> Typst source (escaped)
  -> one-page PDF (fonts embedded and subset by Typst)
  -> extract page content stream and resources with lopdf
  -> Form XObject referenced by the annotation's /AP /N
```

Measured with the embedded engine: a one-page Chinese document compiles in
30 ms and produces a 17 KB PDF; the page stream and the font resources are
recovered cleanly with lopdf.

This replaces the earlier plan of hand-rolling font embedding with `subsetter`.
That approach was fast enough (a 55 MB system font subset to 7.8 KB in 0.3 ms),
but we would still have had to build the CID font dictionary, `/W` widths and
`/ToUnicode` by hand. Typst removes that entire class of bugs.

### 4.3 Font licensing

Coverage differs between faces. U+6A11 is missing from at least one common
Chinese face, which is exactly the "missing glyph" scenario.

| Face | Covers U+6A11 | Note |
| --- | --- | --- |
| STHeiti Light | Yes | System font, not redistributable |
| Hiragino Sans GB | Yes | System font, not redistributable |
| Songti | No | Missing glyph |

PlutarchPages solves this by loading optional external font directories
(`CRAFT_FONTS_DIR`, e.g. LXGW WenKai) and never falling back to system fonts —
its comment is literally "never system-interface fallbacks".

Plan: bundle one open-source Chinese face (LXGW WenKai or Source Han, both SIL
OFL, redistributable) as the default, and treat system fonts as an optional,
never-redistributed extra.

---

## 5. Appearance streams: rules that cost real time

`/AP /N` must be an **indirect reference**:

```pdf
% ignored by PDFium and others
/AP <</N <</Type/XObject/Subtype/Form ...>> stream ... endstream>>
% works
/AP <</N 54 0 R>>
```

The appearance stream was syntactically valid and present; the viewers simply
never looked at it. Moving the form to a top-level object and referencing it
fixed the checkbox immediately (checked box: 2250 non-white pixels versus 1392
for unchecked).

Further rules, each found by debugging:

- **Do not set `/Matrix`.** Viewers already map `BBox` onto `Rect`; adding the
  mapping again scales the artwork twice.
- **Normalize `/Rect`** so `x0 <= x1` and `y0 <= y1`.
- **Draw filled paths.** A stroked polyline depends on line cap and join state
  and rendered differently between our two probes.
- **Watch the coordinate system.** Appearance space has its origin at the
  bottom-left with y pointing up; the UI uses top-left with y down. The tick was
  drawn upside down on the first attempt.

These are encoded as comments at the top of `crates/pdf-annot/src/lib.rs` so the
next person does not rediscover them.

---

## 5.1 Interaction rules that only show up in real use

Two more rules, found by driving the running app rather than by reading code:

- **A double-click is two clicks.** Placing a new annotation on `Response::clicked()`
  means the second click of a double-click drops a duplicate. Guard placement with
  `!response.double_clicked()`, and treat a click that lands inside an existing annotation
  as belonging to that annotation.
- **Switch back to the select tool after placing.** Otherwise the next click, meant to
  inspect what was just added, adds another one.

Neither is visible in a unit test; both were obvious within seconds of clicking around.

## 6. Projects worth borrowing from

| Project | Licence | Size | What to reuse | Distance to us |
| --- | --- | --- | --- | --- |
| [PlutarchPages](https://github.com/EquityBot/PlutarchPages) (fork of [PdfCraft](https://github.com/storytold/pdfcraft)) | MIT / Apache-2.0 | 560k lines Rust | Crate split, appearance generation rules, font embedding policy | Aims at Acrobat Pro; heavy |
| [pdf-editor-offline](https://github.com/OthmaneBlial/pdf-editor-offline) | MIT | 145k lines Rust | Workspace constraints (`unsafe_code = "forbid"`), pinned vendored deps | Desktop still in progress, CLI first |
| [Typst](https://crates.io/crates/typst) | Apache-2.0 | — | Typesetting engine | Direct dependency |
| [Pandoc](https://pandoc.org) | GPL-2.0+ | — | Format conversion | External process only |

### 6.1 Specific code worth studying

**PlutarchPages `crates/annot/src/appearance.rs`** states the right defensive
posture:

> It returns `None` for anything it cannot draw faithfully (cloudy borders,
> unknown line endings, indirect geometry), so callers never replace an
> appearance with a worse one.

That is: if you cannot draw it well, do not draw it at all — never overwrite an
existing appearance with a worse one.

**PlutarchPages `crates/fonts/src/embedding.rs`** only embeds explicitly
supplied, embeddable TrueType faces and never falls back to system fonts. That
solves the licensing question above.

### 6.2 Reuse policy

All three are permissive, so borrowing is legal. Still:

- Record the exact commit and licence.
- Keep copyright notices and licence text.
- Do not read "MIT" as "no attribution needed".

Policy: **do not fork a whole project** (PlutarchPages at 560k lines contradicts
"lightweight"). Port individual modules with attribution.

### 6.3 A licence trap to avoid

`franken_markdown` advertises "MIT License (with OpenAI/Anthropic Rider)". The
LICENSE body defines "Restricted Parties" to include OpenAI, Anthropic, their
affiliates, "and any person or entity acting directly or indirectly on behalf
of, for the benefit of, or under the direction of any of the foregoing", then
states "No rights are granted to any Restricted Party."

That is not standard MIT: the boundary of "acting on behalf of" is vague and
the risk is real. **Do not depend on or borrow from it.** Typst plus Pandoc
already covers the same ground.

This is a concrete instance of the warning that "free to use" and "safe to
integrate and redistribute" are different questions.

### 6.4 Pandoc and the GPL

Pandoc is GPL-2.0+. Calling it as a separate process (`Command::new("pandoc")`)
without linking or redistributing its code keeps the GPL from reaching our own
code. Do not bundle the pandoc binary in installers unless willing to meet GPL
distribution terms.

---

## 6.5 Binary size

The Typst dependency tree pushes an unstripped release build to 62.8 MB, which is not
"lightweight" by any measure. Release settings close most of the gap:

```toml
[profile.release]
lto = "thin"
codegen-units = 1
strip = "symbols"
panic = "abort"
```

| Build | Binary |
| --- | --- |
| Default release | 62.8 MB |
| `strip` only | 50.3 MB |
| LTO + `codegen-units = 1` + `strip` + `panic = "abort"` | 38.5 MB |

Behaviour is unchanged: tests pass and the same document renders identically under
Apple's renderer after the switch. If size matters more than startup time later, the next
lever is compiling Typst features selectively rather than all of them.

## 7. Risks and mitigations

| Risk | Severity | Mitigation |
| --- | --- | --- |
| Chinese annotations do not display in some viewers | High (reproduced) | Embed font + write `/AP` as an indirect reference; verify in Preview, Acrobat and PDFium |
| hayro does not support encrypted PDFs | Medium | Detect and say so; add decryption later |
| hayro deviations on complex documents | Medium | Build a regression corpus of 20+ samples |
| egui does not feel native | Low | Core is GUI-independent |
| PDF modified by another reader while open | Medium | Write copies first; write-back needs change detection |
| Typst compilation cost per text annotation | Low | Cache compiled results; only re-typeset on edit |

---

## 8. Next steps

1. Finish the Typst typesetting path and wire it into `FreeText` annotations.
2. Verify the result in Preview, Acrobat and a PDFium-based viewer.
3. Image and ink signatures.
4. Text editing UI (currently a fixed placeholder).
5. Regression corpus and cross-viewer screenshot comparison.
