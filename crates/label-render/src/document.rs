//! A freeform label document: a fixed-height canvas (the tape's
//! cross-feed width) of arbitrary length, holding freely positioned text
//! and image elements. Lets a builder UI place content by (x, y) in dot
//! units and composite the whole thing down to one [`Raster`] at print
//! time.

use std::path::PathBuf;

use ab_glyph::{Font, PxScale, ScaleFont};
use image::{imageops, GrayImage};
use imageproc::drawing::draw_text_mut;
use label_core::Raster;

use crate::{binarize, load_font, RenderError, RenderOptions, TextAlign};

/// Supersampling factor used while compositing, so text and resized
/// images keep antialiased edges after the final downscale to the
/// printer's real dot grid.
const SUPER: u32 = 4;

#[derive(Debug, Clone)]
pub enum ElementKind {
    Text {
        lines: Vec<String>,
        /// Text height, in dots (the document's native, non-supersampled
        /// unit — the whole tape is only `cross_dots` dots tall).
        size: f32,
        bold: bool,
        align: TextAlign,
    },
    Image {
        path: PathBuf,
        /// On-canvas size, in dots.
        width: f32,
        height: f32,
    },
}

#[derive(Debug, Clone)]
pub struct Element {
    pub id: u64,
    /// Top-left position, in dots, in human-reading orientation (x grows
    /// along the feed direction, y across the tape).
    pub x: f32,
    pub y: f32,
    pub kind: ElementKind,
}

/// A label laid out as a human reads it: `feed_dots` long, `cross_dots`
/// tall (normally [`label_core::PRINT_HEAD_DOTS`]).
#[derive(Debug, Clone)]
pub struct LabelDocument {
    pub feed_dots: u32,
    pub cross_dots: u32,
    pub elements: Vec<Element>,
}

impl LabelDocument {
    pub fn new(feed_dots: u32, cross_dots: u32) -> Self {
        Self {
            feed_dots,
            cross_dots,
            elements: Vec::new(),
        }
    }
}

/// Composites every element onto one supersampled canvas (in natural,
/// human-reading orientation), then rotates and downscales it to the
/// printer's real dot grid exactly like [`crate::raster_from_text`] and
/// [`crate::raster_from_dynamic_image`] do for a single element.
pub fn raster_from_document(
    doc: &LabelDocument,
    render_opts: &RenderOptions,
) -> Result<Raster, RenderError> {
    let super_w = (doc.feed_dots * SUPER).max(1);
    let super_h = (doc.cross_dots * SUPER).max(1);
    let mut canvas = GrayImage::from_pixel(super_w, super_h, image::Luma([255u8]));

    for element in &doc.elements {
        let sx = (element.x * SUPER as f32).round() as i64;
        let sy = (element.y * SUPER as f32).round() as i64;
        match &element.kind {
            ElementKind::Text {
                lines,
                size,
                bold,
                align,
            } => draw_text_element(&mut canvas, lines, *size, *bold, *align, sx, sy)?,
            ElementKind::Image {
                path,
                width,
                height,
            } => draw_image_element(&mut canvas, path, *width, *height, sx, sy)?,
        }
    }

    let rotated = imageops::rotate90(&canvas);
    let resized = imageops::resize(
        &rotated,
        doc.cross_dots.max(1),
        doc.feed_dots.max(1),
        imageops::FilterType::Lanczos3,
    );
    let adjusted = if render_opts.contrast != 0.0 {
        imageops::contrast(&resized, render_opts.contrast)
    } else {
        resized
    };

    let bits = binarize(&adjusted, render_opts.dither, render_opts.invert);
    Ok(Raster::new(doc.cross_dots.max(1), doc.feed_dots.max(1), bits))
}

fn draw_text_element(
    canvas: &mut GrayImage,
    lines: &[String],
    size: f32,
    bold: bool,
    align: TextAlign,
    x: i64,
    y: i64,
) -> Result<(), RenderError> {
    let font = load_font(None, bold)?;
    let font_ref = &font;
    let scale = PxScale::from(size * SUPER as f32);
    let scaled = font_ref.as_scaled(scale);
    let line_height = scaled.height().ceil() as i64;

    let widths: Vec<i64> = lines
        .iter()
        .map(|l| imageproc::drawing::text_size(scale, font_ref, l).0 as i64)
        .collect();
    let max_width = widths.iter().copied().max().unwrap_or(0);

    for (i, (line, width)) in lines.iter().zip(widths.iter()).enumerate() {
        let line_x = x
            + match align {
                TextAlign::Left => 0,
                TextAlign::Center => (max_width - width) / 2,
                TextAlign::Right => max_width - width,
            };
        let line_y = y + line_height * i as i64;
        draw_text_mut(canvas, image::Luma([0u8]), line_x as i32, line_y as i32, scale, font_ref, line);
        if bold {
            draw_text_mut(
                canvas,
                image::Luma([0u8]),
                line_x as i32 + 1,
                line_y as i32,
                scale,
                font_ref,
                line,
            );
        }
    }
    Ok(())
}

fn draw_image_element(
    canvas: &mut GrayImage,
    path: &PathBuf,
    width: f32,
    height: f32,
    x: i64,
    y: i64,
) -> Result<(), RenderError> {
    let img = image::open(path)?.to_luma8();
    let target_w = ((width * SUPER as f32).round() as u32).max(1);
    let target_h = ((height * SUPER as f32).round() as u32).max(1);
    let resized = imageops::resize(&img, target_w, target_h, imageops::FilterType::Lanczos3);

    let (cw, ch) = canvas.dimensions();
    for (px, py, pixel) in resized.enumerate_pixels() {
        let dx = x + px as i64;
        let dy = y + py as i64;
        if dx >= 0 && dy >= 0 && (dx as u32) < cw && (dy as u32) < ch {
            canvas.put_pixel(dx as u32, dy as u32, *pixel);
        }
    }
    Ok(())
}

/// Renders a document to a pixel-exact, human-upright PNG preview — handy
/// for a builder UI's canvas background, same framing [`crate::preview_png`]
/// uses for a single element.
pub fn document_preview_png(
    doc: &LabelDocument,
    render_opts: &RenderOptions,
    scale: u32,
) -> Result<Vec<u8>, RenderError> {
    let raster = raster_from_document(doc, render_opts)?;
    crate::preview_png(&raster, scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_document_is_blank_and_sized() {
        let doc = LabelDocument::new(100, 32);
        let raster = raster_from_document(&doc, &RenderOptions::default()).unwrap();
        assert_eq!(raster.cross, 32);
        assert_eq!(raster.feed, 100);
        assert!(raster.bits.iter().all(|&b| !b));
    }

    #[test]
    fn text_element_produces_ink() {
        let mut doc = LabelDocument::new(200, 32);
        doc.elements.push(Element {
            id: 1,
            x: 5.0,
            y: 4.0,
            kind: ElementKind::Text {
                lines: vec!["Hi".to_string()],
                size: 20.0,
                bold: false,
                align: TextAlign::Left,
            },
        });
        let raster = raster_from_document(&doc, &RenderOptions::default()).unwrap();
        assert!(raster.bits.iter().any(|&b| b));
    }
}
