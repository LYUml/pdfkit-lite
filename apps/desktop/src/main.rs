//! A lightweight, Preview-like PDF reader and annotator.
//!
//! Milestone 1 scope: open a PDF, render it, place checkboxes and text boxes, save a
//! separate copy, and hand that copy back to the system PDF reader.

use eframe::egui;
use pdf_doc::{PageBox, PdfDocument};
use pdf_typeset::{FontSet, system_font_candidates};
use pdf_render::RenderedPage;
use pdf_render::worker::{self, RenderRequest, RenderResult};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

/// Colour of drawn ink on screen, matching what is written into the PDF.
fn ink_color() -> egui::Color32 {
    egui::Color32::from_rgb(
        (INK[0] * 255.0) as u8,
        (INK[1] * 255.0) as u8,
        (INK[2] * 255.0) as u8,
    )
}

/// Ink used for annotation artwork and text.
const INK: [f32; 3] = [0.10, 0.10, 0.10];

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // Headless self-test: `pdfkit-lite --selftest <in.pdf> <out.pdf>` writes the demo
    // annotations and exits. Used to verify the write path without a GUI.
    if args.get(1).map(String::as_str) == Some("--selftest") {
        let src = PathBuf::from(&args[2]);
        let out = PathBuf::from(&args[3]);
        let bytes = std::fs::read(&src).expect("read input");
        let mut doc = PdfDocument::from_bytes(bytes).expect("parse input");
        let placed = demo_annotations(&doc);
        let fonts = FontSet::from_files(&system_font_candidates());
        write_annotations(&mut doc, &placed, &fonts).expect("write annotations");
        doc.save_to(&out).expect("save");
        println!("selftest ok -> {} ({} annotations)", out.display(), placed.len());
        return Ok(());
    }
    let initial = args.get(1).map(PathBuf::from);
    eframe::run_native(
        "pdfkit-lite",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 900.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            install_ui_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(initial)))
        }),
    )
}

/// egui ships no CJK glyphs. Load a system CJK face so the UI can show Chinese.
fn install_ui_fonts(ctx: &egui::Context) {
    let candidates = [
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "cjk".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.push("cjk".to_owned());
                }
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Checkbox,
    Text,
    Sign,
}

/// A placed annotation, stored in *visible* page coordinates (top-left origin, points).
#[derive(Debug, Clone)]
struct Placed {
    page: usize,
    rect: egui::Rect,
    kind: PlacedKind,
}

#[derive(Debug, Clone)]
enum PlacedKind {
    Checkbox { checked: bool },
    Text { text: String },
    /// A handwritten signature. Points are in the annotation's local space, origin at the
    /// bottom-left, matching PDF appearance space.
    Signature { strokes: Vec<Vec<(f32, f32)>> },
}

struct App {
    doc: Option<PdfDocument>,
    page_sizes: Vec<(f32, f32)>,
    page_boxes: Vec<PageBox>,
    page: usize,
    zoom: f32,
    tool: Tool,
    placed: Vec<Placed>,
    render_tx: Option<Sender<RenderRequest>>,
    render_rx: Option<Receiver<RenderResult>>,
    token: u64,
    pending: Option<u64>,
    texture: Option<egui::TextureHandle>,
    displayed: Option<(usize, RenderedPage)>,
    status: String,
    path: Option<PathBuf>,
    fonts: FontSet,
    /// The stroke being drawn with the Sign tool, in visible page coordinates.
    current_stroke: Option<Vec<egui::Pos2>>,
    /// Index of the text annotation currently being edited inline.
    editing_text: Option<usize>,
}

impl App {
    fn new(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            doc: None,
            page_sizes: Vec::new(),
            page_boxes: Vec::new(),
            page: 0,
            zoom: 1.4,
            tool: Tool::Select,
            placed: Vec::new(),
            render_tx: None,
            render_rx: None,
            token: 0,
            pending: None,
            texture: None,
            displayed: None,
            status: "Open a PDF to begin".to_owned(),
            path: None,
            // Loading fonts is slow (tens of MB), so do it once at startup.
            fonts: FontSet::from_files(&system_font_candidates()),
            current_stroke: None,
            editing_text: None,
        };
        if let Some(p) = path {
            app.open(p);
        }
        app
    }

    fn open(&mut self, path: PathBuf) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.status = format!("Could not read file: {e}");
                return;
            }
        };
        match PdfDocument::from_bytes(bytes.clone()) {
            Ok(doc) => {
                let boxes: Vec<PageBox> = (0..doc.page_count())
                    .filter_map(|i| doc.page_box(i))
                    .collect();
                self.page_sizes = boxes.iter().map(|b| (b.width(), b.height())).collect();
                self.page_boxes = boxes;
                let (tx, rx) = worker::spawn(bytes);
                self.render_tx = Some(tx);
                self.render_rx = Some(rx);
                self.doc = Some(doc);
                self.page = 0;
                self.placed.clear();
                self.texture = None;
                self.displayed = None;
                self.path = Some(path.clone());
                self.status = format!(
                    "Opened {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                self.request_render();
            }
            Err(e) => self.status = format!("Could not parse PDF: {e}"),
        }
    }

    fn request_render(&mut self) {
        let Some(tx) = &self.render_tx else { return };
        self.token += 1;
        let req = RenderRequest {
            token: self.token,
            page: self.page,
            scale_milli: (self.zoom * 1000.0) as u32,
        };
        self.pending = Some(req.token);
        if tx.send(req).is_err() {
            self.status = "Render worker exited".to_owned();
        }
    }

    fn poll_render(&mut self) {
        let mut latest: Option<RenderResult> = None;
        if let Some(rx) = &self.render_rx {
            while let Ok(r) = rx.try_recv() {
                latest = Some(r);
            }
        }
        if let Some(r) = latest {
            if Some(r.token) == self.pending {
                self.pending = None;
                match r.result {
                    Ok(page) => self.displayed = Some((r.page, page)),
                    Err(e) => self.status = format!("Render failed: {e}"),
                }
            }
        }
    }

/// Convert a screen-space polyline into a placed signature annotation.
///
/// The stroke is stored in the annotation's local space with the origin at the bottom-left
/// so it matches PDF appearance coordinates directly.
fn finish_stroke(&mut self, page: usize, points: Vec<egui::Pos2>, page_origin: egui::Pos2) {
    if points.len() < 2 {
        return;
    }
    let xs: Vec<f32> = points.iter().map(|p| p.x).collect();
    let ys: Vec<f32> = points.iter().map(|p| p.y).collect();
    let (min_x, max_x) = (xs.iter().cloned().fold(f32::MAX, f32::min), xs.iter().cloned().fold(f32::MIN, f32::max));
    let (min_y, max_y) = (ys.iter().cloned().fold(f32::MAX, f32::min), ys.iter().cloned().fold(f32::MIN, f32::max));
    // A little padding so the stroke is not clipped by the annotation box.
    let pad = 3.0;
    let rect = egui::Rect::from_min_max(
        egui::pos2(min_x - pad, min_y - pad),
        egui::pos2(max_x + pad, max_y + pad),
    );
    let local: Vec<(f32, f32)> = points
        .iter()
        .map(|p| (p.x - rect.min.x, rect.max.y - p.y))
        .collect();
    let _ = page_origin;
    self.placed.push(Placed {
        page,
        rect,
        kind: PlacedKind::Signature { strokes: vec![local] },
    });
}

    fn save_copy(&mut self) {
        let Some(doc) = &mut self.doc else {
            self.status = "No document is open".to_owned();
            return;
        };
        let src = self
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("document.pdf"));
        let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let out = src.with_file_name(format!("{stem}-edited.pdf"));

        if let Err(e) = write_annotations(doc, &self.placed, &self.fonts) {
            self.status = format!("Could not write annotations: {e}");
            return;
        }
        match doc.save_to(&out) {
            Ok(()) => {
                self.status = format!("Saved a copy to {}", out.display());
                open_in_system_reader(&out);
            }
            Err(e) => self.status = format!("Could not save: {e}"),
        }
    }
}

/// A fixed set of annotations used by `--selftest`, exercising the coordinate conversion,
/// the appearance streams and the save path without a GUI.
fn demo_annotations(doc: &PdfDocument) -> Vec<Placed> {
    let box0 = doc.page_box(0).unwrap_or(PageBox { x0: 0.0, y0: 0.0, x1: 612.0, y1: 792.0 });
    let _ = box0;
    vec![
        Placed {
            page: 0,
            rect: egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(16.0, 16.0)),
            kind: PlacedKind::Checkbox { checked: true },
        },
        Placed {
            page: 0,
            rect: egui::Rect::from_min_size(egui::pos2(64.0, 40.0), egui::vec2(16.0, 16.0)),
            kind: PlacedKind::Checkbox { checked: false },
        },
        Placed {
            page: 0,
            // Sized to the strokes themselves; the appearance box matches the annotation box.
            rect: egui::Rect::from_min_size(egui::pos2(40.0, 70.0), egui::vec2(40.0, 20.0)),
            kind: PlacedKind::Signature {
                // A small "scribble": two strokes, enough to prove Ink round-trips.
                strokes: vec![
                    vec![
                        (2.0, 4.0), (10.0, 16.0), (18.0, 4.0), (26.0, 16.0), (34.0, 4.0),
                    ],
                    vec![(6.0, 11.0), (30.0, 11.0)],
                ],
            },
        },
        Placed {
            page: 0,
            rect: egui::Rect::from_min_size(egui::pos2(90.0, 38.0), egui::vec2(220.0, 16.0)),
            kind: PlacedKind::Text {
                // CJK + Latin on purpose: this exercises the embedded-subset-font path.
                text: std::env::var("SELFTEST_TEXT")
                    .unwrap_or_else(|_| "Signature checked 2026-11-01".to_owned()),
            },
        },
    ]
}

/// Turn `Placed` items into standard PDF annotations on the matching page.
/// Turn placed items into standard PDF annotations on the matching page.
fn write_annotations(
    doc: &mut PdfDocument,
    placed: &[Placed],
    fonts: &FontSet,
) -> Result<(), String> {
    for item in placed {
        let Some(page_box) = doc.page_box(item.page) else {
            continue;
        };
        let (x0, y1) = page_box.visible_to_user(item.rect.min.x, item.rect.max.y);
        let (x1, y0) = page_box.visible_to_user(item.rect.max.x, item.rect.min.y);
        let rect = pdf_annot::normalize([x0, y0, x1, y1]);
        let page_id = doc
            .page_id(item.page)
            .ok_or_else(|| format!("page {} missing", item.page))?;

        match &item.kind {
            PlacedKind::Checkbox { checked } => {
                let pending = pdf_annot::checkbox(rect, *checked, INK);
                attach(doc, page_id, pending, None);
            }
            PlacedKind::Signature { strokes } => {
                // Strokes are already in the annotation's local space, so the appearance box
                // is exactly the annotation box.
                let width = (rect[2] - rect[0]).max(1.0);
                let height = (rect[3] - rect[1]).max(1.0);
                let pending = pdf_annot::ink(rect, strokes, (width, height), INK);
                attach(doc, page_id, pending, None);
            }
            PlacedKind::Text { text } => {
                let width = (rect[2] - rect[0]).max(40.0);
                // Typeset the text with Typst: this is what makes CJK work, because the
                // output carries an embedded, subset font.
                let page = pdf_typeset::typeset(fonts, text, width, 11.0, INK)
                    .map_err(|e| format!("typesetting failed: {e}"))?;
                let merged = pdf_typeset::merge_first_page(doc.document_mut(), &page.pdf)
                    .map_err(|e| format!("could not embed text appearance: {e}"))?;
                // Grow the annotation to fit the typeset block.
                let height = merged.height.max(12.0);
                let fitted = [rect[0], rect[3] - height, rect[2], rect[3]];
                let pending = pdf_annot::free_text_with_appearance(
                    fitted,
                    text,
                    (merged.width, merged.height),
                    Vec::new(),
                    lopdf::Dictionary::new(),
                    "0 g /Helv 11 Tf",
                );
                attach(doc, page_id, pending, Some(merged.form_id));
            }
        }
    }
    Ok(())
}

/// Register an annotation, using `form_id` as its appearance when the appearance was
/// already built elsewhere (e.g. merged from Typst).
fn attach(
    doc: &mut PdfDocument,
    page_id: lopdf::ObjectId,
    pending: pdf_annot::PendingAnnotation,
    form_id: Option<lopdf::ObjectId>,
) {
    use lopdf::Object;
    let pdf_annot::PendingAnnotation { dict, form } = pending;
    let form_id = match form_id {
        Some(id) => id,
        None => doc.document_mut().add_object(form),
    };
    // /AP must be an indirect reference; an inline stream is ignored by some viewers.
    let annot = {
        let mut dict = dict;
        dict.set(
            "AP",
            lopdf::dictionary! { "N" => lopdf::Object::Reference(form_id) },
        );
        dict
    };
    let id = doc.document_mut().add_object(annot);
    let d = doc.document_mut();
    let Ok(page) = d.get_object_mut(page_id).and_then(|o| o.as_dict_mut()) else {
        return;
    };
    match page
        .get_mut(b"Annots")
        .ok()
        .and_then(|o| o.as_array_mut().ok())
    {
        Some(arr) => arr.push(Object::Reference(id)),
        None => {
            page.set("Annots", Object::Array(vec![Object::Reference(id)]));
        }
    }
}


/// Hand the finished copy to the OS PDF reader (Preview on macOS).
fn open_in_system_reader(path: &PathBuf) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Native file picker without extra dependencies: the OS dialog where available.
fn pick_pdf() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("osascript")
            .args([
                "-e",
                "POSIX path of (choose file with prompt \"Choose a PDF\" of type {\"com.adobe.pdf\"})",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(PathBuf::from(s)) }
    }
    #[cfg(target_os = "linux")]
    {
        let out = std::process::Command::new("zenity")
            .args(["--file-selection", "--file-filter=PDF files | *.pdf"])
            .output()
            .ok()?;
        if !out.status.success() { return None; }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(PathBuf::from(s)) }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_render();
        let mut want_render = false;

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open PDF").clicked() {
                    if let Some(path) = pick_pdf() {
                        self.open(path.clone());
                    }
                }
                ui.separator();
                ui.selectable_value(&mut self.tool, Tool::Select, "Select");
                ui.selectable_value(&mut self.tool, Tool::Checkbox, "Checkbox");
                ui.selectable_value(&mut self.tool, Tool::Text, "Text");
                ui.selectable_value(&mut self.tool, Tool::Sign, "Sign");
                ui.separator();
                if ui.button("-").clicked() {
                    self.zoom = (self.zoom - 0.2).max(0.4);
                    want_render = true;
                }
                ui.label(format!("{:.0}%", self.zoom * 100.0));
                if ui.button("+").clicked() {
                    self.zoom = (self.zoom + 0.2).min(6.0);
                    want_render = true;
                }
                ui.separator();
                if ui.button("Save copy").clicked() {
                    self.save_copy();
                }
            });
            ui.horizontal(|ui| {
                let pages = self.doc.as_ref().map(|d| d.page_count()).unwrap_or(0);
                if ui.button("<").clicked() && self.page > 0 {
                    self.page -= 1;
                    want_render = true;
                }
                ui.label(format!("{}/{}", self.page + 1, pages.max(1)));
                if ui.button(">").clicked() && self.page + 1 < pages {
                    self.page += 1;
                    want_render = true;
                }
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let Some((page_no, rendered)) = &self.displayed else {
                ui.centered_and_justified(|ui| ui.label("Rendering…"));
                return;
            };
            let size = egui::vec2(rendered.width as f32, rendered.height as f32);
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
            let tex = self.texture.get_or_insert_with(|| {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [rendered.width as usize, rendered.height as usize],
                    &rendered.rgba,
                );
                ui.ctx()
                    .load_texture("page", img, egui::TextureOptions::LINEAR)
            });
            ui.painter().image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );

            let page_now = *page_no;
            let mut toggle: Option<usize> = None;
            let mut editing_text: Option<usize> = self.editing_text;
            let mut text_edit: Option<(usize, String)> = None;
            let mut finish_text = false;
            for (i, item) in self.placed.iter().enumerate() {
                if item.page != page_now {
                    continue;
                }
                let r = egui::Rect::from_min_max(
                    rect.min + item.rect.min.to_vec2(),
                    rect.min + item.rect.max.to_vec2(),
                );
                match &item.kind {
                    PlacedKind::Checkbox { checked } => {
                        let resp = ui.interact(r, ui.scope_id().with(("cb", i)), egui::Sense::click());
                        let p = ui.painter();
                        p.rect_filled(r, 0.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 240));
                        p.rect_stroke(
                            r,
                            0.0,
                            egui::Stroke::new(1.2, egui::Color32::from_rgb(40, 115, 230)),
                            egui::StrokeKind::Middle,
                        );
                        if *checked {
                            let a = r.left_bottom() + egui::vec2(r.width() * 0.2, -r.height() * 0.45);
                            let b = r.center_bottom() + egui::vec2(0.0, -r.height() * 0.18);
                            let c = r.right_top() + egui::vec2(-r.width() * 0.12, r.height() * 0.18);
                            let s = egui::Stroke::new(2.0, egui::Color32::from_rgb(25, 140, 60));
                            p.line_segment([a, b], s);
                            p.line_segment([b, c], s);
                        }
                        if resp.clicked() {
                            toggle = Some(i);
                        }
                    }
                    PlacedKind::Signature { strokes } => {
                        let p = ui.painter();
                        let stroke = egui::Stroke::new(1.6, ink_color());
                        for line in strokes {
                            let pts: Vec<egui::Pos2> = line
                                .iter()
                                .map(|(x, y)| {
                                    // Local y grows upward; the screen grows downward.
                                    egui::pos2(r.min.x + x, r.max.y - y)
                                })
                                .collect();
                            if pts.len() >= 2 {
                                p.add(egui::Shape::line(pts, stroke));
                            }
                        }
                    }
                    PlacedKind::Text { text } => {
                        // A lightweight inline editor: double-click to edit, Enter or click
                        // away to commit. The widget is placed over the annotation box and
                        // sized to it, so what you see matches what gets typeset.
                        let editor_rect = egui::Rect::from_min_size(
                            r.min,
                            egui::vec2(r.width().max(120.0), 22.0),
                        );
                        if editing_text == Some(i) {
                            let mut buf = text.clone();
                            let out = ui.put(
                                editor_rect,
                                egui::TextEdit::singleline(&mut buf)
                                    .font(egui::FontId::proportional(12.0))
                                    .background_color(egui::Color32::from_rgba_unmultiplied(255, 255, 240, 245))
                                    .hint_text("Type annotation text"),
                            );
                            if buf != *text {
                                text_edit = Some((i, buf));
                            }
                            if out.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                finish_text = true;
                            }
                            // Enter alone also commits, even without losing focus first.
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                finish_text = true;
                            }
                            out.request_focus();
                        } else {
                            let inner = ui.painter();
                            inner.rect_filled(
                                r,
                                0.0,
                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
                            );
                            inner.text(
                                r.min + egui::vec2(2.0, 2.0),
                                egui::Align2::LEFT_TOP,
                                if text.is_empty() { "(empty)" } else { text },
                                egui::FontId::proportional(11.0),
                                egui::Color32::BLACK,
                            );
                            let resp = ui.interact(
                                editor_rect,
                                ui.scope_id().with(("txt", i)),
                                egui::Sense::click(),
                            );
                            if resp.double_clicked() {
                                editing_text = Some(i);
                            }
                        }
                    }
                }
            }
            if let Some(i) = toggle {
                if let PlacedKind::Checkbox { checked } = &mut self.placed[i].kind {
                    *checked = !*checked;
                }
            }
            if let Some((i, buf)) = text_edit {
                if let Some(Placed { kind: PlacedKind::Text { text }, .. }) = self.placed.get_mut(i) {
                    *text = buf;
                }
            }
            if finish_text {
                editing_text = None;
            }
            self.editing_text = editing_text;

            // Signing: drag to draw a stroke; releasing seals it into an annotation.
            if self.tool == Tool::Sign {
                let painter = ui.painter();
                if response.dragged() || response.dragged_by(egui::PointerButton::Primary) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        self.current_stroke.get_or_insert_with(Vec::new).push(pos);
                    }
                }
                if let Some(points) = &self.current_stroke {
                    if points.len() >= 2 {
                        painter.add(egui::Shape::line(
                            points.clone(),
                            egui::Stroke::new(1.6, ink_color()),
                        ));
                    }
                }
                if response.drag_stopped() {
                    if let Some(points) = self.current_stroke.take() {
                        self.finish_stroke(page_now, points, rect.min);
                    }
                }
                return;
            }

            // Placement: single click on the page while a drawing tool is active.
            //
            // Two guards, both learned the hard way:
            // 1. Double-click on an existing annotation belongs to that annotation (it opens
            //    its editor), so a click landing inside one must not create a new one.
            // 2. Only a *single* click places anything; otherwise the second click of a
            //    double-click would drop a duplicate.
            let clicked_empty_space = response.clicked()
                && !response.double_clicked()
                && self.tool != Tool::Select;
            if clicked_empty_space {
                if let Some(pos) = response.interact_pointer_pos() {
                    let hit_existing = self.placed.iter().any(|p| {
                        p.page == page_now
                            && egui::Rect::from_min_size(
                                rect.min + p.rect.min.to_vec2(),
                                p.rect.size(),
                            )
                            .expand(2.0)
                            .contains(pos)
                    });
                    if !hit_existing {
                        let top_left = pos;
                        match self.tool {
                            Tool::Checkbox => self.placed.push(Placed {
                                page: page_now,
                                rect: egui::Rect::from_min_size(top_left, egui::vec2(16.0, 16.0)),
                                kind: PlacedKind::Checkbox { checked: true },
                            }),
                            Tool::Text => {
                                let idx = self.placed.len();
                                self.placed.push(Placed {
                                    page: page_now,
                                    rect: egui::Rect::from_min_size(
                                        top_left,
                                        egui::vec2(180.0, 22.0),
                                    ),
                                    kind: PlacedKind::Text { text: String::new() },
                                });
                                // Drop straight into editing: an empty box is useless.
                                self.editing_text = Some(idx);
                                self.tool = Tool::Select;
                            }
                            Tool::Select | Tool::Sign => {}
                        }
                    }
                }
            }
        });

        if want_render {
            self.request_render();
        }
    }
}
