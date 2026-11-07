use label_render::document::{document_preview_png, Element, ElementKind, LabelDocument, ShapeKind, ShapeStyle};
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
            font_path: None,
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
            font_path: None,
        },
    });

    let png = document_preview_png(&doc, &RenderOptions::default(), 6).unwrap();
    std::fs::write("/tmp/builder_preview.png", png).unwrap();
    println!("wrote /tmp/builder_preview.png");

    let mut shapes = LabelDocument::new(360, 32);
    shapes.elements.push(Element {
        id: 1,
        x: 2.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Circle, size: 24.0, style: ShapeStyle::Filled },
    });
    shapes.elements.push(Element {
        id: 2,
        x: 32.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Circle, size: 24.0, style: ShapeStyle::Outline },
    });
    shapes.elements.push(Element {
        id: 3,
        x: 62.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Square, size: 24.0, style: ShapeStyle::Filled },
    });
    shapes.elements.push(Element {
        id: 4,
        x: 92.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Square, size: 24.0, style: ShapeStyle::Outline },
    });
    shapes.elements.push(Element {
        id: 5,
        x: 122.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Triangle, size: 24.0, style: ShapeStyle::Filled },
    });
    shapes.elements.push(Element {
        id: 6,
        x: 152.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Triangle, size: 24.0, style: ShapeStyle::Outline },
    });
    shapes.elements.push(Element {
        id: 7,
        x: 182.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Circle, size: 24.0, style: ShapeStyle::Dotted },
    });
    shapes.elements.push(Element {
        id: 8,
        x: 212.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Square, size: 24.0, style: ShapeStyle::Dotted },
    });
    shapes.elements.push(Element {
        id: 9,
        x: 242.0,
        y: 4.0,
        kind: ElementKind::Shape { shape: ShapeKind::Triangle, size: 24.0, style: ShapeStyle::Dotted },
    });
    let png = document_preview_png(&shapes, &RenderOptions::default(), 6).unwrap();
    std::fs::write("/tmp/builder_shapes_preview.png", png).unwrap();
    println!("wrote /tmp/builder_shapes_preview.png");
}
