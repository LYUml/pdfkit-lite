//! Writes an image signature into a PDF, for verifying the SMask path end to end.
//!
//! Usage: image_stamp <input.pdf> <signature.png> <output.pdf>

use lopdf::{Dictionary, Document, Object, Stream, dictionary};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (_in, png, out) = (&args[1], &args[2], &args[3]);
    let mut doc = Document::load(&args[1])?;

    let image = pdf_annot::RgbaImage::from_png(&std::fs::read(png)?)?;
    println!(
        "image {}x{}, transparency: {}",
        image.width,
        image.height,
        image.has_transparency()
    );
    let image_ref = pdf_annot::add_image_xobject(&mut doc, &image);

    let rect = [60.0f32, 640.0, 260.0, 700.0];
    let pending = pdf_annot::image_stamp(
        rect,
        image_ref,
        (image.width as f32, image.height as f32),
        "signature",
    );
    let pdf_annot::PendingAnnotation { dict, form } = pending;
    let form_id = doc.add_object(form);
    let annot_id = doc.add_object(pdf_annot::PendingAnnotation { dict, form: Stream::new(Dictionary::new(), Vec::new()) }.into_dictionary(form_id));

    let page_id = *doc.get_pages().values().next().unwrap();
    let page = doc.get_object_mut(page_id)?.as_dict_mut()?;
    let mut annots = match page.get(b"Annots").and_then(|o| o.as_array()) {
        Ok(a) => a.clone(),
        Err(_) => Vec::new(),
    };
    let _ = dictionary! {};
    annots.push(Object::Reference(annot_id));
    page.set("Annots", Object::Array(annots));

    doc.save(out)?;
    println!("wrote {out}");
    Ok(())
}
