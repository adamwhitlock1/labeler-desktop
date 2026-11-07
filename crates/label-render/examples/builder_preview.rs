use label_render::document::{document_preview_png, Element, ElementKind, LabelDocument};
use label_render::{RenderOptions, TextAlign};

fn main() {
    let mut doc = LabelDocument::new(260, 32);
    doc.elements.push(Element {
        id: 1,
        x: 4.0,
        y: 3.0,
        kind: ElementKind::Text {
            lines: vec!["Pantry".to_string()],
            size: 14.0,
            bold: true,
            align: TextAlign::Left,
        },
    });
    doc.elements.push(Element {
        id: 2,
        x: 4.0,
        y: 18.0,
        kind: ElementKind::Text {
            lines: vec!["Best by 12/25".to_string()],
            size: 10.0,
            bold: false,
            align: TextAlign::Left,
        },
    });

    let png = document_preview_png(&doc, &RenderOptions::default(), 6).unwrap();
    std::fs::write("/tmp/builder_preview.png", png).unwrap();
    println!("wrote /tmp/builder_preview.png");
}
