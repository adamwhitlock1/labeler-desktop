use label_render::document::{raster_from_document, Element, ElementKind, LabelDocument};
use label_render::{physical_preview_png, RenderOptions, TapeProfile};
use std::path::PathBuf;

fn main() {
    let icon_dir = std::env::args().nth(1).expect("pass icon dir");
    let mut doc = LabelDocument::new(80, 32);
    doc.elements.push(Element {
        id: 1,
        x: 2.0,
        y: 4.0,
        kind: ElementKind::Image {
            path: PathBuf::from(format!("{icon_dir}/magnifying-glass.svg")),
            width: 24.0,
            height: 24.0,
        },
    });
    doc.elements.push(Element {
        id: 2,
        x: 32.0,
        y: 4.0,
        kind: ElementKind::Image {
            path: PathBuf::from(format!("{icon_dir}/home.svg")),
            width: 24.0,
            height: 24.0,
        },
    });

    let raster = raster_from_document(&doc, &RenderOptions::default()).unwrap();
    let png = physical_preview_png(&raster, &TapeProfile::default(), 20.0).unwrap();
    std::fs::write("/tmp/physical_preview.png", png).unwrap();
    println!("wrote /tmp/physical_preview.png (raster {}x{})", raster.cross, raster.feed);
}
