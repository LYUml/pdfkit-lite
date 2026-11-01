//! Standard PDF annotations with explicit appearance streams.
//!
//! Everything written here is plain PDF that Preview, Acrobat and browser viewers all
//! understand, so a file we touch stays editable after it leaves this app.
//!
//! ## Hard-won rules for appearance streams
//!
//! 1. `/AP /N` must be an **indirect reference** (`/AP <</N 54 0 R>>`). An inline stream
//!    nested inside the annotation dictionary is silently ignored by PDFium and others.
//! 2. Do **not** set `/Matrix` on the form. Viewers already map `BBox` onto `Rect`;
//!    adding the mapping again scales the artwork twice.
//! 3. `/Rect` must be normalized to `x0 <= x1, y0 <= y1`.
//! 4. Draw shapes as **filled paths**. Stroked polylines depend on line cap/join state and
//!    render differently across viewers.
//! 5. Appearance coordinates are PDF user space (origin bottom-left, y up) while a UI is
//!    usually top-left origin with y down. Convert once, at the boundary.

mod image;
pub use image::{ImageError, RgbaImage, add_image_xobject, resources_with_image};

use lopdf::{Dictionary, Object, Stream, dictionary};

/// A rectangle in PDF user space.
pub type Rect = [f32; 4];

/// Normalize to `x0 <= x1, y0 <= y1`, as `/Rect` requires.
pub fn normalize(rect: Rect) -> Rect {
    [
        rect[0].min(rect[2]),
        rect[1].min(rect[3]),
        rect[0].max(rect[2]),
        rect[1].max(rect[3]),
    ]
}

fn rect_object(rect: Rect) -> Object {
    let r = normalize(rect);
    Object::Array(vec![r[0].into(), r[1].into(), r[2].into(), r[3].into()])
}

/// An annotation whose appearance still has to become a top-level object.
///
/// Builders return this instead of a finished `Dictionary` so the caller can register the
/// form stream with the document and wire up the reference (rule 1 above).
#[derive(Debug)]
pub struct PendingAnnotation {
    /// The annotation dictionary, without `/AP`.
    pub dict: Dictionary,
    /// The normal appearance stream.
    pub form: Stream,
}

impl PendingAnnotation {
    /// Attach the appearance as an indirect reference.
    pub fn into_dictionary(self, form_id: lopdf::ObjectId) -> Dictionary {
        let mut dict = self.dict;
        dict.set("AP", dictionary! { "N" => Object::Reference(form_id) });
        dict
    }
}

fn form_stream(width: f32, height: f32, content: String) -> Stream {
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => Object::Array(vec![
                0.into(), 0.into(), Object::Real(width), Object::Real(height),
            ]),
            "Resources" => Dictionary::new(),
            "FormType" => 1,
        },
        content.into_bytes(),
    )
}

/// Append one thick line segment as a filled quad.
fn push_leg(content: &mut String, from: (f32, f32), to: (f32, f32), t: f32) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let (nx, ny) = (-dy / len * t, dx / len * t);
    content.push_str(&format!(
        "{a0} {b0} m {a1} {b1} l {a2} {b2} l {a3} {b3} l h f\n",
        a0 = from.0 + nx, b0 = from.1 + ny,
        a1 = to.0 + nx,   b1 = to.1 + ny,
        a2 = to.0 - nx,   b2 = to.1 - ny,
        a3 = from.0 - nx, b3 = from.1 - ny,
    ));
}

/// A checkbox: a `Square` annotation drawing a box and, when checked, a tick.
pub fn checkbox(rect: Rect, checked: bool, ink: [f32; 3]) -> PendingAnnotation {
    let r = normalize(rect);
    let w = (r[2] - r[0]).max(1.0);
    let h = (r[3] - r[1]).max(1.0);
    let border = [0.15, 0.45, 0.90];

    let mut content = String::new();
    content.push_str(&format!("q 1 1 1 rg 0 0 {w} {h} re f Q\n"));
    let inset = 0.6f32.min(w / 4.0).min(h / 4.0);
    content.push_str(&format!(
        "q {br} {bg} {bb} RG 1.4 w {inset} {inset} {bw} {bh} re S Q\n",
        br = border[0], bg = border[1], bb = border[2],
        bw = (w - 2.0 * inset).max(0.1),
        bh = (h - 2.0 * inset).max(0.1),
    ));
    if checked {
        // Long arm ends at the top-right, i.e. large y in user space.
        let pad = 0.18 * w.max(h);
        let (ax, ay) = (pad, h * 0.48);
        let (bx, by) = (w * 0.36, pad);
        let (cx, cy) = (w - pad, h - pad);
        let t = 0.085 * w.max(h);
        content.push_str(&format!("q {} {} {} rg\n", ink[0], ink[1], ink[2]));
        push_leg(&mut content, (ax, ay), (bx, by), t);
        push_leg(&mut content, (bx, by), (cx, cy), t);
        content.push_str("Q\n");
    }

    PendingAnnotation {
        dict: dictionary! {
            "Type" => "Annot",
            "Subtype" => "Square",
            "Rect" => rect_object(rect),
            "F" => 4,
            "C" => Object::Array(vec![
                Object::Real(border[0]), Object::Real(border[1]), Object::Real(border[2]),
            ]),
            "Contents" => Object::string_literal(if checked { "checked" } else { "unchecked" }),
        },
        form: form_stream(w, h, content),
    }
}

/// A `FreeText` annotation whose appearance was produced elsewhere (e.g. by Typst).
///
/// `appearance` spans `bbox`; the caller supplies the resources it needs (usually a
/// `/Font` dictionary referencing embedded fonts).
pub fn free_text_with_appearance(
    rect: Rect,
    contents: &str,
    bbox: (f32, f32),
    appearance_content: Vec<u8>,
    resources: Dictionary,
    da: &str,
) -> PendingAnnotation {
    let mut stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => Object::Array(vec![
                0.into(), 0.into(), Object::Real(bbox.0), Object::Real(bbox.1),
            ]),
            "Resources" => resources,
            "FormType" => 1,
        },
        appearance_content,
    );
    let _ = stream.compress();

    let contents = if contents.is_ascii() {
        Object::string_literal(contents)
    } else {
        // PDF text strings are PDFDocEncoded or UTF-16BE.
        let mut bytes = vec![0xFEu8, 0xFF];
        for unit in contents.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        Object::String(bytes, lopdf::StringFormat::Hexadecimal)
    };

    PendingAnnotation {
        dict: dictionary! {
            "Type" => "Annot",
            "Subtype" => "FreeText",
            "Rect" => rect_object(rect),
            "F" => 4,
            "DA" => Object::string_literal(da),
            "Border" => Object::Array(vec![0.into(), 0.into(), 0.into()]),
            "Contents" => contents,
        },
        form: stream,
    }
}

/// A handwritten signature: a standard `Ink` annotation.
///
/// `strokes` are polylines in the appearance's local space, where `(0, 0)` is the
/// bottom-left of `rect`. Drawing them as a stroked path with round caps keeps the
/// annotation editable: a user can move or delete it later, and other viewers that
/// regenerate the appearance still show the same ink.
pub fn ink(rect: Rect, strokes: &[Vec<(f32, f32)>], bbox: (f32, f32), ink: [f32; 3]) -> PendingAnnotation {
    // Callers pass strokes in local space; if the declared bbox does not cover them, widen
    // it. A too-large box squashes nothing but a too-small one clips the ink.
    let max_x = strokes
        .iter()
        .flat_map(|s| s.iter().map(|(x, _)| *x))
        .fold(0.0f32, f32::max);
    let max_y = strokes
        .iter()
        .flat_map(|s| s.iter().map(|(_, y)| *y))
        .fold(0.0f32, f32::max);
    let bbox = (bbox.0.max(max_x).max(1.0), bbox.1.max(max_y).max(1.0));
    let mut content = String::new();
    content.push_str(&format!(
        "q {} {} {} RG 1.6 w 1 J 1 j\n",
        ink[0], ink[1], ink[2]
    ));
    for stroke in strokes {
        let Some((first, rest)) = stroke.split_first() else {
            continue;
        };
        content.push_str(&format!("{} {} m\n", first.0, first.1));
        for (x, y) in rest {
            content.push_str(&format!("{x} {y} l\n"));
        }
        content.push_str("S\n");
    }
    content.push_str("Q\n");

    // /InkList mirrors the geometry so the annotation stays meaningful when edited.
    let ink_list = strokes
        .iter()
        .map(|stroke| {
            let nums: Vec<Object> = stroke
                .iter()
                .flat_map(|(x, y)| [Object::Real(*x), Object::Real(*y)])
                .collect();
            Object::Array(nums)
        })
        .collect::<Vec<_>>();

    PendingAnnotation {
        dict: dictionary! {
            "Type" => "Annot",
            "Subtype" => "Ink",
            "Rect" => rect_object(rect),
            "F" => 4,
            "C" => Object::Array(vec![
                Object::Real(ink[0]), Object::Real(ink[1]), Object::Real(ink[2]),
            ]),
            "Border" => Object::Array(vec![0.into(), 0.into(), Object::Real(1.6)]),
            "InkList" => Object::Array(ink_list),
            "Contents" => Object::string_literal("signature"),
        },
        form: form_stream(bbox.0, bbox.1, content),
    }
}

/// An image signature: a `Stamp` annotation whose appearance paints an image XObject.
///
/// `image_ref` must already exist in the host document as an Image XObject. The image is
/// stretched to `rect`, which matches how a scanned signature is normally placed.
pub fn image_stamp(
    rect: Rect,
    image_ref: lopdf::ObjectId,
    image_size: (f32, f32),
    label: &str,
) -> PendingAnnotation {
    let r = normalize(rect);
    let w = (r[2] - r[0]).max(1.0);
    let h = (r[3] - r[1]).max(1.0);
    let (iw, ih) = (image_size.0.max(1.0), image_size.1.max(1.0));

    // `cm` positions the unit image square onto the appearance box.
    let content = format!("q {w} 0 0 {h} 0 0 cm /Sig Do Q\n");
    let _ = (iw, ih);

    let mut form = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => Object::Array(vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)]),
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Sig" => Object::Reference(image_ref) }
            },
            "FormType" => 1,
        },
        content.into_bytes(),
    );
    let _ = form.compress();

    PendingAnnotation {
        dict: dictionary! {
            "Type" => "Annot",
            "Subtype" => "Stamp",
            "Rect" => rect_object(rect),
            "F" => 4,
            "Name" => Object::Name(b"Approved".to_vec()),
            "Contents" => Object::string_literal(label),
        },
        form,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_inverted_rects() {
        assert_eq!(normalize([100.0, 200.0, 50.0, 20.0]), [50.0, 20.0, 100.0, 200.0]);
    }

    #[test]
    fn ink_carries_one_path_per_stroke() {
        let strokes = vec![vec![(1.0, 2.0), (3.0, 4.0), (5.0, 6.0)], vec![(7.0, 8.0), (9.0, 10.0)]];
        let pending = ink([10.0, 10.0, 60.0, 30.0], &strokes, (50.0, 20.0), [0.1, 0.1, 0.1]);
        let body = String::from_utf8_lossy(&pending.form.content);
        assert_eq!(body.matches(" m\n").count(), 2, "one move per stroke");
        assert!(pending.dict.has(b"InkList"));
    }

    #[test]
    fn checked_checkbox_draws_more_than_unchecked() {
        let a = checkbox([10.0, 10.0, 26.0, 26.0], true, [0.1, 0.5, 0.2]);
        let b = checkbox([10.0, 10.0, 26.0, 26.0], false, [0.1, 0.5, 0.2]);
        assert!(a.form.content.len() > b.form.content.len(), "checked must carry the tick path");
    }
}
