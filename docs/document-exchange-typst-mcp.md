# Document exchange, embedded typesetting, and agent control

Date: 2026-11-01
Companion to [rust-pdf-editor-research.md](rust-pdf-editor-research.md)

---

## 1. Summary

All three ideas are feasible, and cheaper than expected:

| Idea | Verdict | Key measurement |
| --- | --- | --- |
| LaTeX/Word interop without LibreOffice | Feasible, LibreOffice not needed | Pandoc converts Markdown and LaTeX to `.docx` with Chinese intact |
| Embedded PDF typesetting | Feasible, pure Rust | Typst 0.15 embedded as a library, Chinese PDF in ~30 ms |
| Built-in MCP for agents | Feasible | `rmcp` 3.8 compiles; an official precedent exists but differs |

One correction to the earlier plan: the fix for Chinese annotations is not
hand-rolled font embedding but **typesetting with Typst and embedding the
result as the appearance**.

---

## 2. LaTeX and Word interop: no LibreOffice

### 2.1 Measurements

| Direction | Command | Result |
| --- | --- | --- |
| Markdown to Word | `pandoc t.md -o t.docx` | 11 KB docx, Chinese intact |
| LaTeX to Word | `pandoc l.tex -o l.docx` | Chinese headings, bold and italics preserved |
| Word to Markdown | `pandoc t.docx -t markdown` | Tables, lists and emphasis survive |
| Word to plain text | `pandoc t.docx -t plain` | Chinese intact |

**LibreOffice is not needed.** Pandoc reads and writes docx/odt directly.

Installed here: `pandoc 3.9.0.2` with `+server +lua`.

### 2.2 Boundary

Pandoc handles **content**, not **layout**: paragraphs, headings, lists, tables,
footnotes, equations, images, citations. It does not reproduce Word page
margins, columns, floating image positions, complex headers, or tracked changes.

For a lightweight app that is the right split: Pandoc is the import/export
boundary, not a layout renderer.

### 2.3 Distribution

| Option | Upside | Downside |
| --- | --- | --- |
| User installs it (default) | No size, no licence risk | User must install |
| Bundle `pandoc.wasm` | One build for three platforms | Roughly +40 MB, needs a wasm runtime |
| Bundle native binaries | Best experience | +100 MB across three platforms |

Default: show the import/export UI only when pandoc is detected; otherwise show
a one-line install hint. This matches the lightweight goal.

---

## 3. Embedded typesetting: Typst

### 3.1 Why Typst rather than LaTeX

| Dimension | Typst | LaTeX |
| --- | --- | --- |
| Embeddable as a Rust library | Yes, pure Rust | No, external binary |
| Compile time | 30 ms measured | Seconds |
| Same on three platforms | Yes | Needs a TeX distribution |
| Size | Library, bounded | TeX Live is gigabytes |
| Chinese | Works, measured | Needs xeCJK configuration |

### 3.2 Pandoc and Typst compose

Pandoc can emit Typst source (`-t typst`):

```
Markdown or LaTeX
  -> pandoc -t typst
  -> Typst source (headings, bold, italics, tables, code all preserved)
  -> embedded Typst engine
  -> PDF
```

Verified: Typst source produced by Pandoc, compiled by the embedded engine,
rendered Chinese headings, bold, italics, tables and lists correctly.

So Pandoc does semantic conversion and Typst does layout, with no heavy
external runtime.

---

## 4. Built-in MCP

### 4.1 Measurement

`rmcp` 3.8 (the Rust MCP SDK) compiles with the server and stdio transport
features.

```toml
rmcp = { version = "3.8", features = ["server", "transport-io"], default-features = false }
```

### 4.2 An existing precedent

Anthropic ships a PDF MCP server: `@modelcontextprotocol/server-pdf` (MIT), in
the [`modelcontextprotocol/ext-apps`](https://github.com/modelcontextprotocol/ext-apps)
repository under `examples/pdf-server`. It is registered by the official
`pdf-viewer` plugin and provides tools including "Display PDF", "Interact with
PDF", "Poll PDF Commands" and "Save PDF". It is built on pdf.js, pdf-lib and
Express, and runs under Node.

| Dimension | Anthropic `server-pdf` | This project |
| --- | --- | --- |
| Shape | Separate Node server plus a pdf.js web viewer | Built into a native app |
| Front end | HTML/JS | Native Rust (egui) |
| Annotations | Viewer overlay, not standard PDF annotations | Standard PDF annotations, still editable elsewhere |
| Install | `npx`, needs Node | Shipped with the app |
| Dependencies | pdf.js, pdf-lib, Express | Pure Rust |

The difference is real: native, standard annotations, and no external runtime.

### 4.3 Design

- Off by default; a single switch in settings.
- When on, listen on `127.0.0.1` only, or serve over stdio so a client can
  launch the app as a subprocess.
- Suggested tools: `open_document`, `get_page_text`, `list_annotations`,
  `add_checkbox`, `add_text`, `save_copy`.
- **Writes require user confirmation.** An agent silently editing a document is a
  real risk: propose in the agent, confirm in the native UI, then write.

---

## 5. Prior art

**No existing project combines pure Rust, native UI, standard PDF annotations
and a built-in MCP server.** Every component has a mature precedent, though, and
none of them should be re-implemented.

| # | Precedent | Licence | What it does | What to borrow |
| --- | --- | --- | --- | --- |
| 1 | Anthropic `server-pdf` | MIT | PDF MCP server with viewer | Tool semantics, including asynchronous polling |
| 2 | PlutarchPages / PdfCraft | MIT / Apache-2.0 | 560k lines Rust targeting Acrobat Pro | Appearance rules, font policy, crate split |
| 3 | pdf-editor-offline | MIT | 145k lines Rust, hayro + lopdf | Workspace constraints |
| 4 | Typst | Apache-2.0 | Typesetting engine | Use as a library |
| 5 | Pandoc | GPL-2.0+ | Format conversion | External process only |
| 6 | franken_markdown | See section 6.3 of the companion document | Markdown to PDF in pure Rust | **Avoid**, see licence note |

---

## 6. Revised architecture

```
pdfkit-lite/
  crates/
    pdf-doc/       open, page geometry, save (lopdf)
    pdf-render/    page to RGBA (hayro) plus a background worker
    pdf-typeset/   Typst engine wrapper (annotation text, Markdown to PDF)
    pdf-convert/   Pandoc boundary (external process, optional)
    pdf-annot/     annotation builders and appearance streams
    pdf-mcp/       built-in MCP server (rmcp, off by default)
  apps/desktop/
```

Dependency rules:

- `pdf-typeset` depends on Typst only, never on the GUI.
- `pdf-convert` only spawns a process; degrades gracefully when pandoc is absent.
- `pdf-mcp` calls the other crates' public APIs, never UI internals.

---

## 7. Risks

| Risk | Severity | Mitigation |
| --- | --- | --- |
| Typst dependency tree compile time | Medium | About 6 minutes cold, fast incrementally; cache in CI |
| Typst layout does not match users' Word expectations | Medium | Say clearly that Typst is an internal template |
| Pandoc missing or version drift | Low | Detect version, hide the entry point when missing |
| Built-in MCP called by hostile software | High | Loopback only, confirmation on writes, off by default |
| Licence traps like franken_markdown | Medium | Maintain a dependency licence checklist; review anything that is not plain MIT |

---

## 8. Next steps

1. Finish wiring `pdf-typeset` into `FreeText` annotations.
2. Verify Chinese annotations in Preview, Acrobat and PDFium.
3. Pandoc boundary: export first, import later.
4. MCP after the annotation work; define tool semantics before the confirmation flow.
