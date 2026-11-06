use label_render::{raster_from_text, preview_png, raw_preview_png, RenderOptions, TextOptions, Dither};

fn main() {
    let text_opts = TextOptions::default();
    let render_opts = RenderOptions { dither: Dither::FloydSteinberg, ..Default::default() };
    let raster = raster_from_text(&["Hello LT200B"], &text_opts, &render_opts).expect("render");
    let png = preview_png(&raster, 8).expect("png");
    std::fs::write("/tmp/lt200b_preview_raw.png", raw_preview_png(&raster, 8).unwrap()).unwrap();
    std::fs::write("/tmp/lt200b_preview.png", png).unwrap();
    println!("cross={} feed={}", raster.cross, raster.feed);
}
