//! End-to-end checks on the annotation write path.
//!
//! These run without a window. They build a small PDF in memory, write annotations
//! through the same builders the app uses, save it, then reopen it and assert that every
//! annotation survived with an indirect appearance reference.

use lopdf::{Dictionary, Document, Object, Stream, dictionary};

/// A minimal one-page, Letter-sized PDF.
fn sample_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let contents = doc.add_object(Stream::new(Dictionary::new(), b"q Q".to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "MediaBox" => Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()]),
        "Resources" => Dictionary::new(),
        "Contents" => Object::Reference(contents),
    });
    let pages_id = doc.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => Object::Array(vec![Object::Reference(page_id)]),
        "Count" => 1,
    });
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => Object::Reference(pages_id),
    });
    doc.get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Parent", Object::Reference(pages_id));
    doc.trailer.set("Root", Object::Reference(catalog));
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

/// Append a pending annotation to `doc`, wiring /AP as an indirect reference.
fn push(doc: &mut Document, page_id: lopdf::ObjectId, pending: pdf_annot::PendingAnnotation) {
    let pdf_annot::PendingAnnotation { dict, form } = pending;
    let form_id = doc.add_object(form);
    let annot = pdf_annot::PendingAnnotation { dict, form: Stream::new(Dictionary::new(), Vec::new()) };
    let annot_id = doc.add_object(annot.into_dictionary(form_id));
    let page = doc.get_object_mut(page_id).unwrap().as_dict_mut().unwrap();
    let mut annots = match page.get(b"Annots").and_then(|o| o.as_array()) {
        Ok(existing) => existing.clone(),
        Err(_) => Vec::new(),
    };
    annots.push(Object::Reference(annot_id));
    page.set("Annots", Object::Array(annots));
}

fn annotations(doc: &Document, page_id: lopdf::ObjectId) -> Vec<Dictionary> {
    let page = doc.get_dictionary(page_id).unwrap();
    let Ok(annots) = page.get(b"Annots").and_then(|o| o.as_array()) else {
        return Vec::new();
    };
    annots
        .iter()
        .filter_map(|o| o.as_reference().ok())
        .filter_map(|id| doc.get_dictionary(id).ok().cloned())
        .collect()
}

#[test]
fn checkbox_and_ink_survive_a_save() {
    let mut doc = Document::load_mem(&sample_pdf()).unwrap();
    let page_id = *doc.get_pages().values().next().unwrap();

    push(&mut doc, page_id, pdf_annot::checkbox([40.0, 700.0, 56.0, 716.0], true, [0.1, 0.5, 0.2]));
    let strokes = vec![vec![(1.0, 2.0), (5.0, 8.0), (9.0, 3.0)]];
    push(&mut doc, page_id, pdf_annot::ink([60.0, 690.0, 100.0, 710.0], &strokes, (40.0, 20.0), [0.1, 0.1, 0.1]));

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();

    let out = Document::load_mem(&bytes).unwrap();
    let page_id = *out.get_pages().values().next().unwrap();
    let found = annotations(&out, page_id);
    let kinds: Vec<String> = found
        .iter()
        .filter_map(|d| d.get(b"Subtype").ok()?.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect();
    assert_eq!(kinds.len(), 2, "expected two annotations, got {kinds:?}");
    assert!(kinds.iter().any(|k| k == "Square"), "{kinds:?}");
    assert!(kinds.iter().any(|k| k == "Ink"), "{kinds:?}");
}

#[test]
fn appearance_is_always_an_indirect_reference() {
    let mut doc = Document::load_mem(&sample_pdf()).unwrap();
    let page_id = *doc.get_pages().values().next().unwrap();
    push(&mut doc, page_id, pdf_annot::checkbox([40.0, 700.0, 56.0, 716.0], true, [0.1, 0.5, 0.2]));

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    let out = Document::load_mem(&bytes).unwrap();
    let page_id = *out.get_pages().values().next().unwrap();

    for annot in annotations(&out, page_id) {
        let ap = annot.get(b"AP").unwrap().as_dict().unwrap();
        // This is the rule that cost the most debugging time: an inline stream is ignored
        // by PDFium and others, so /AP /N must point at a top-level object.
        assert!(
            matches!(ap.get(b"N"), Ok(Object::Reference(_))),
            "/AP /N must be an indirect reference",
        );
    }
}

#[test]
fn checked_and_unchecked_checkboxes_differ() {
    let mut doc = Document::load_mem(&sample_pdf()).unwrap();
    let page_id = *doc.get_pages().values().next().unwrap();
    push(&mut doc, page_id, pdf_annot::checkbox([40.0, 700.0, 56.0, 716.0], true, [0.1, 0.5, 0.2]));
    push(&mut doc, page_id, pdf_annot::checkbox([60.0, 700.0, 76.0, 716.0], false, [0.1, 0.5, 0.2]));
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    let out = Document::load_mem(&bytes).unwrap();
    let page_id = *out.get_pages().values().next().unwrap();

    let bodies: Vec<String> = annotations(&out, page_id)
        .iter()
        .filter_map(|d| {
            let ap = d.get(b"AP").ok()?.as_dict().ok()?;
            let id = ap.get(b"N").ok()?.as_reference().ok()?;
            let stream = out.get_object(id).ok()?.as_stream().ok()?;
            Some(String::from_utf8_lossy(&stream.decompressed_content().ok()?).into_owned())
        })
        .collect();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].len() > bodies[1].len(), "checked box must draw the tick");
}
