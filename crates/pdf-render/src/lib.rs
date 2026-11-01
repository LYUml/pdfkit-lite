//! Page rasterization on top of `hayro` (pure Rust, no C dependencies).
//!
//! This crate knows nothing about the GUI or about annotations: it turns a page of a
//! parsed PDF into an RGBA buffer at a requested scale. Everything else in the app draws
//! on top of that buffer.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("the PDF could not be parsed")]
    Parse,
    #[error("the PDF is encrypted and cannot be opened without a password")]
    Encrypted,
    #[error("page {0} does not exist")]
    NoSuchPage(usize),
}

/// A rendered page in straight (non-premultiplied) RGBA8, top-left origin, ready to be
/// uploaded as an egui texture.
#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An opened document, already parsed and ready for rendering.
pub struct Rasterizer {
    pdf: Pdf,
}

impl std::fmt::Debug for Rasterizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rasterizer")
            .field("pages", &self.pdf.pages().len())
            .finish()
    }
}

impl Rasterizer {
    /// Parse `bytes` into a renderable handle.
    pub fn new(bytes: Vec<u8>) -> Result<Self, RenderError> {
        let pdf = Pdf::new(bytes).map_err(|e| match e {
            hayro::hayro_syntax::LoadPdfError::Decryption(_) => RenderError::Encrypted,
            hayro::hayro_syntax::LoadPdfError::Invalid => RenderError::Parse,
        })?;
        Ok(Self { pdf })
    }

    pub fn page_count(&self) -> usize {
        self.pdf.pages().len()
    }

    /// Render page `index` at `scale` (1.0 == 72 dpi) onto a white background.
    ///
    /// Run this off the UI thread: complex pages take tens of milliseconds.
    pub fn render_page(&self, index: usize, scale: f32) -> Result<RenderedPage, RenderError> {
        render_pdf(&self.pdf, index, scale)
    }

    /// Page dimensions in points (1 point == 1/72 inch) at scale 1.0.
    pub fn page_size_pt(&self, index: usize) -> Result<(f32, f32), RenderError> {
        page_size(&self.pdf, index)
    }

    pub fn page_sizes_pt(&self) -> Vec<(f32, f32)> {
        let pages = self.pdf.pages();
        (0..pages.len())
            .filter_map(|i| pages.get(i).map(|p| {
                let (w, h) = p.render_dimensions();
                (w as f32, h as f32)
            }))
            .collect()
    }
}

fn page_size(pdf: &Pdf, index: usize) -> Result<(f32, f32), RenderError> {
    let pages = pdf.pages();
    let page = pages.get(index).ok_or(RenderError::NoSuchPage(index))?;
    let (w, h) = page.render_dimensions();
    Ok((w as f32, h as f32))
}

fn render_pdf(pdf: &Pdf, index: usize, scale: f32) -> Result<RenderedPage, RenderError> {
    let pages = pdf.pages();
    let page = pages.get(index).ok_or(RenderError::NoSuchPage(index))?;
    // `RenderCache` borrows the document, so it lives for one render call.
    let cache = RenderCache::new();
    let settings = InterpreterSettings::default();
    let render_settings = RenderSettings::default();
    let rendered = render(
        page,
        &cache,
        &settings,
        &render_settings,
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let width = rendered.width() as u32;
    let height = rendered.height() as u32;
    // hayro hands back premultiplied RGBA on a white background; un-premultiply defensively
    // and force alpha to 255 so egui treats it as an opaque texture.
    let mut rgba = rendered.data_as_u8_slice().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        let a = pixel[3];
        if a != 0 && a != 255 {
            pixel[0] = (pixel[0] as u32 * 255 / a as u32).min(255) as u8;
            pixel[1] = (pixel[1] as u32 * 255 / a as u32).min(255) as u8;
            pixel[2] = (pixel[2] as u32 * 255 / a as u32).min(255) as u8;
        }
        pixel[3] = 255;
    }
    Ok(RenderedPage {
        width,
        height,
        rgba,
    })
}

/// Background rendering so page rasterization never blocks the UI thread.
pub mod worker {
    use super::{Rasterizer, RenderError, RenderedPage};
    use std::sync::mpsc::{Receiver, Sender, channel};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RenderRequest {
        /// Monotonic id so the UI can drop stale results.
        pub token: u64,
        pub page: usize,
        /// Scale in thousandths, to keep the request `Copy + Eq`.
        pub scale_milli: u32,
    }

    #[derive(Debug)]
    pub struct RenderResult {
        pub token: u64,
        pub page: usize,
        pub scale_milli: u32,
        pub result: Result<RenderedPage, RenderError>,
    }

    /// Spawn a render worker. Dropping the sender stops the thread.
    pub fn spawn(bytes: Vec<u8>) -> (Sender<RenderRequest>, Receiver<RenderResult>) {
        let (req_tx, req_rx) = channel::<RenderRequest>();
        let (res_tx, res_rx) = channel::<RenderResult>();
        std::thread::spawn(move || {
            let raster = match Rasterizer::new(bytes) {
                Ok(r) => r,
                Err(e) => {
                    let _ = res_tx.send(RenderResult {
                        token: 0,
                        page: 0,
                        scale_milli: 0,
                        result: Err(e),
                    });
                    return;
                }
            };
            while let Ok(mut req) = req_rx.recv() {
                // Coalesce: when newer requests are queued, only render the newest one.
                while let Ok(newer) = req_rx.try_recv() {
                    req = newer;
                }
                let scale = req.scale_milli as f32 / 1000.0;
                let result = raster.render_page(req.page, scale);
                if res_tx
                    .send(RenderResult {
                        token: req.token,
                        page: req.page,
                        scale_milli: req.scale_milli,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        (req_tx, res_rx)
    }
}
