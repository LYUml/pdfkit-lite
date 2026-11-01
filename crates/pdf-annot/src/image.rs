//! Encode raster images as PDF Image XObjects.
//!
//! Signature images are usually PNGs with transparency, so the interesting part is the
//! soft mask: a grayscale image named by `/SMask` supplies the alpha channel, which is how
//! PDF spells "transparent background". Without it a signature would sit on an opaque
//! white rectangle and hide whatever is underneath.

use lopdf::{Dictionary, Object, Stream, dictionary};

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("the image could not be decoded: {0}")]
    Decode(String),
}

/// A decoded RGBA image, ready to embed.
#[derive(Debug, Clone)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// Straight (non-premultiplied) RGBA, row-major, top-left origin.
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    pub fn from_png(bytes: &[u8]) -> Result<Self, ImageError> {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().map_err(|e| ImageError::Decode(e.to_string()))?;
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| ImageError::Decode(e.to_string()))?;
        buf.truncate(info.buffer_size());

        let (width, height) = (info.width, info.height);
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => {
                let mut out = Vec::with_capacity(buf.len() / 3 * 4);
                for px in buf.chunks_exact(3) {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
                out
            }
            png::ColorType::Grayscale => {
                let mut out = Vec::with_capacity(buf.len() * 4);
                for v in &buf {
                    out.extend_from_slice(&[*v, *v, *v, 255]);
                }
                out
            }
            png::ColorType::GrayscaleAlpha => {
                let mut out = Vec::with_capacity(buf.len() / 2 * 4);
                for px in buf.chunks_exact(2) {
                    out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
                }
                out
            }
            png::ColorType::Indexed => {
                return Err(ImageError::Decode(
                    "indexed PNGs are not supported; re-save as RGBA".to_owned(),
                ));
            }
        };
        if rgba.len() != (width * height * 4) as usize {
            return Err(ImageError::Decode(format!(
                "unexpected buffer size: {} for {width}x{height}",
                rgba.len()
            )));
        }
        Ok(Self {
            width,
            height,
            pixels: rgba,
        })
    }

    /// Whether any pixel is non-opaque, i.e. whether a soft mask is worth writing.
    pub fn has_transparency(&self) -> bool {
        self.pixels.chunks_exact(4).any(|px| px[3] != 255)
    }
}

/// Build the image XObject (plus its soft mask when needed) and add it to `doc`.
///
/// Returns the object id to reference from an appearance stream's `/XObject` dictionary.
pub fn add_image_xobject(doc: &mut lopdf::Document, image: &RgbaImage) -> lopdf::ObjectId {
    let (w, h) = (image.width as i64, image.height as i64);

    // PDF images are bottom-up in principle; viewers accept either, but flipping rows keeps
    // the common case exact.
    let mut rgb = Vec::with_capacity((image.width * image.height * 3) as usize);
    let mut alpha = Vec::with_capacity((image.width * image.height) as usize);
    let stride = (image.width * 4) as usize;
    for row in image.pixels.chunks_exact(stride).rev() {
        for px in row.chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
            alpha.push(px[3]);
        }
    }

    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => w,
        "Height" => h,
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8,
    };

    if image.has_transparency() {
        let mask = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => w,
                "Height" => h,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            alpha,
        );
        let mask_id = doc.add_object(mask);
        dict.set("SMask", Object::Reference(mask_id));
    }

    let mut stream = Stream::new(dict, rgb);
    let _ = stream.compress();
    doc.add_object(stream)
}

/// Serialize a dictionary as it would appear in the PDF, for embedding inside a form's
/// `/Resources` (lopdf can nest dictionaries directly, so this is only a convenience).
pub fn resources_with_image(image_ref: lopdf::ObjectId) -> Dictionary {
    dictionary! {
        "XObject" => dictionary! { "Sig" => Object::Reference(image_ref) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_png(w: u32, h: u32, rgba: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&rgba).unwrap();
        }
        out
    }

    #[test]
    fn decodes_rgba_png_and_detects_alpha() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 0];
        let png = make_png(2, 1, rgba);
        let img = RgbaImage::from_png(&png).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert!(img.has_transparency());
    }

    #[test]
    fn opaque_image_gets_no_soft_mask() {
        let rgba = vec![1, 2, 3, 255, 4, 5, 6, 255];
        let png = make_png(2, 1, rgba);
        let img = RgbaImage::from_png(&png).unwrap();
        assert!(!img.has_transparency());

        let mut doc = lopdf::Document::with_version("1.7");
        let id = add_image_xobject(&mut doc, &img);
        let obj = doc.get_object(id).unwrap().as_stream().unwrap();
        assert!(!obj.dict.has(b"SMask"));
        assert_eq!(obj.dict.get(b"Width").unwrap().as_i64().unwrap(), 2);
    }

    #[test]
    fn transparent_image_gets_a_soft_mask() {
        let rgba = vec![1, 2, 3, 0, 4, 5, 6, 255];
        let png = make_png(2, 1, rgba);
        let img = RgbaImage::from_png(&png).unwrap();
        assert!(img.has_transparency());

        let mut doc = lopdf::Document::with_version("1.7");
        let id = add_image_xobject(&mut doc, &img);
        let obj = doc.get_object(id).unwrap().as_stream().unwrap();
        assert!(matches!(obj.dict.get(b"SMask"), Ok(Object::Reference(_))));
    }
}
