//! Merge a typeset page (a self-contained one-page PDF) into a host document as a
//! Form XObject, carrying its fonts along.
//!
//! Typst emits a complete PDF with embedded font subsets. To use that page as an
//! annotation appearance in someone else's PDF, every object it references must be copied
//! into the host and renumbered consistently. This module does exactly that, and nothing
//! else.

use std::collections::HashMap;

use lopdf::{Document, Dictionary, Object, ObjectId, Stream, dictionary};

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("the interim PDF could not be read: {0}")]
    Read(String),
    #[error("the interim PDF has no page")]
    NoPage,
    #[error("the interim page has no content stream")]
    NoContent,
}

/// Result of merging one typeset page into the host document.
#[derive(Debug)]
pub struct MergedAppearance {
    /// The new form stream object in the host document.
    pub form_id: ObjectId,
    /// Bounding box of the appearance, in points.
    pub width: f32,
    pub height: f32,
}

/// Copy the first page of `interim_pdf` into `host` as a Form XObject.
///
/// Fonts and any other resources the page refers to are copied recursively, so the
/// appearance is self-contained and survives the host document being saved and reopened.
pub fn merge_first_page(
    host: &mut Document,
    interim_pdf: &[u8],
) -> Result<MergedAppearance, MergeError> {
    let interim = Document::load_mem(interim_pdf).map_err(|e| MergeError::Read(e.to_string()))?;

    let pages = interim.get_pages();
    let (&_number, &page_id) = pages.iter().next().ok_or(MergeError::NoPage)?;
    let page = interim
        .get_dictionary(page_id)
        .map_err(|e| MergeError::Read(e.to_string()))?;

    let content = interim.get_page_content(page_id);
    if content.is_empty() {
        return Err(MergeError::NoContent);
    }

    // Page box, in points, with a sane fallback.
    let (width, height) = media_box(&interim, page).unwrap_or((612.0, 792.0));

    // Copy the page's resources, rewriting every indirect reference we follow.
    let mut copied: HashMap<ObjectId, ObjectId> = HashMap::new();
    let resources = match page.get(b"Resources") {
        Ok(obj) => copy_object(host, &interim, obj, &mut copied),
        Err(_) => Object::Dictionary(Dictionary::new()),
    };

    let form = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => Object::Array(vec![
                0.into(), 0.into(), Object::Real(width), Object::Real(height),
            ]),
            "Resources" => resources,
            "FormType" => 1,
        },
        content,
    );
    let form_id = host.add_object(form);
    Ok(MergedAppearance {
        form_id,
        width,
        height,
    })
}

fn media_box(doc: &Document, page: &Dictionary) -> Option<(f32, f32)> {
    for key in [b"MediaBox".as_slice(), b"CropBox"] {
        let Ok(obj) = page.get_deref(key, doc) else {
            continue;
        };
        let Ok(arr) = obj.as_array() else { continue };
        let nums: Vec<f32> = arr
            .iter()
            .filter_map(|o| {
                o.as_float()
                    .ok()
                    .or_else(|| o.as_i64().ok().map(|i| i as f32))
            })
            .collect();
        if nums.len() == 4 {
            return Some(((nums[2] - nums[0]).abs(), (nums[3] - nums[1]).abs()));
        }
    }
    None
}

/// Deep-copy `object` into `host`, following and rewriting indirect references.
///
/// Cycles are broken by pre-allocating an id before recursing.
fn copy_object(
    host: &mut Document,
    src: &Document,
    object: &Object,
    copied: &mut HashMap<ObjectId, ObjectId>,
) -> Object {
    match object {
        Object::Reference(id) => {
            if let Some(existing) = copied.get(id) {
                return Object::Reference(*existing);
            }
            let Ok(resolved) = src.get_object(*id) else {
                // A dangling reference is better dropped than copied as a broken link.
                return Object::Null;
            };
            let resolved = resolved.clone();
            // Reserve the slot first so that a self-referencing object terminates.
            let placeholder = host.add_object(Object::Null);
            copied.insert(*id, placeholder);
            let copy = copy_object(host, src, &resolved, copied);
            if let Ok(slot) = host.get_object_mut(placeholder) {
                *slot = copy;
            }
            Object::Reference(placeholder)
        }
        Object::Array(items) => {
            Object::Array(items.iter().map(|o| copy_object(host, src, o, copied)).collect())
        }
        Object::Dictionary(dict) => {
            let mut out = Dictionary::new();
            for (key, value) in dict.iter() {
                out.set(key.clone(), copy_object(host, src, value, copied));
            }
            Object::Dictionary(out)
        }
        Object::Stream(stream) => {
            let dict = match copy_object(
                host,
                src,
                &Object::Dictionary(stream.dict.clone()),
                copied,
            ) {
                Object::Dictionary(d) => d,
                _ => Dictionary::new(),
            };
            let mut copied_stream = Stream::new(dict, stream.content.clone());
            // Keep the original filter rather than re-encoding.
            copied_stream.allows_compression = false;
            Object::Stream(copied_stream)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_a_minimal_page_as_a_form() {
        let mut interim = Document::with_version("1.7");
        let contents_id = interim.add_object(Stream::new(Dictionary::new(), b"q Q".to_vec()));
        let page_id = interim.add_object(dictionary! {
            "Type" => "Page",
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 200.into(), 100.into()]),
            "Resources" => Dictionary::new(),
            "Contents" => Object::Reference(contents_id),
        });
        let pages_id = interim.add_object(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            "Count" => 1,
        });
        let cat_id = interim.add_object(dictionary! {
            "Type" => "Catalog", "Pages" => Object::Reference(pages_id),
        });
        interim
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Parent", Object::Reference(pages_id));
        interim.trailer.set("Root", Object::Reference(cat_id));
        let mut bytes = Vec::new();
        interim.save_to(&mut bytes).unwrap();

        let mut host = Document::with_version("1.7");
        let merged = merge_first_page(&mut host, &bytes).unwrap();
        assert!((merged.width - 200.0).abs() < 0.5);
        assert!((merged.height - 100.0).abs() < 0.5);
        let form = host.get_object(merged.form_id).unwrap();
        assert!(matches!(form, Object::Stream(_)));
    }
}
