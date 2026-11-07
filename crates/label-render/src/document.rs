//! A freeform label document: a fixed-height canvas (the tape's
//! cross-feed width) of arbitrary length, holding freely positioned text
//! and image elements. Lets a builder UI place content by (x, y) in dot
//! units and composite the whole thing down to one [`Raster`] at print
//! time.

use std::path::PathBuf;

use ab_glyph::{Font, PxScale, ScaleFont};
use image::{imageops, GrayImage};
use imageproc::drawing::{draw_filled_circle_mut, draw_filled_rect_mut, draw_polygon_mut, draw_text_mut};
use imageproc::point::Point;
use imageproc::rect::Rect;
use label_core::Raster;

use crate::{binarize, load_font, RenderError, RenderOptions, TextAlign};

/// Supersampling factor used while compositing, so text and resized
/// images keep antialiased edges after the final downscale to the
/// printer's real dot grid.
const SUPER: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ShapeKind {
    Circle,
    Square,
    Triangle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ShapeStyle {
    Filled,
    /// A continuous outline, about one printed dot thick.
    Outline,
    /// An outline drawn as evenly spaced dots instead of a solid line.
    Dotted,
}

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
    Shape {
        shape: ShapeKind,
        /// Bounding box side length, in dots.
        size: f32,
        style: ShapeStyle,
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
            ElementKind::Shape { shape, size, style } => {
                draw_shape_element(&mut canvas, *shape, *size, *style, sx, sy)
            }
        }
    }

    // `doc.feed_dots`/`doc.cross_dots` are "logical", square-pitch dots: a
    // square laid out in the document looks square. The printer's real
    // dot grid isn't square — its feed-direction pitch is about half its
    // cross-feed pitch (see raster_from_dynamic_image) — so the actual
    // raster needs roughly twice as many feed dots to cover the same
    // physical length. Skipping this (as an earlier version did) prints
    // everything visibly compressed along the feed direction.
    let feed_dots_raw = (doc.feed_dots.max(1)) * 2;
    let cross_dots = doc.cross_dots.max(1);

    let rotated = imageops::rotate90(&canvas);
    let resized = imageops::resize(&rotated, cross_dots, feed_dots_raw, imageops::FilterType::Lanczos3);
    let adjusted = if render_opts.contrast != 0.0 {
        imageops::contrast(&resized, render_opts.contrast)
    } else {
        resized
    };

    let bits = binarize(&adjusted, render_opts.dither, render_opts.invert);
    Ok(Raster::new(cross_dots, feed_dots_raw, bits))
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
    let target_w = ((width * SUPER as f32).round() as u32).max(1);
    let target_h = ((height * SUPER as f32).round() as u32).max(1);

    let is_svg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    let resized = if is_svg {
        rasterize_svg(path, target_w, target_h)?
    } else {
        let img = image::open(path)?.to_luma8();
        imageops::resize(&img, target_w, target_h, imageops::FilterType::Lanczos3)
    };

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

/// Rasterizes an SVG file to a grayscale bitmap at exactly `target_w` x
/// `target_h`, composited over white — icons are typically drawn as
/// colored/black shapes on a transparent background, and the printer only
/// has black ink on white tape.
fn rasterize_svg(path: &PathBuf, target_w: u32, target_h: u32) -> Result<GrayImage, RenderError> {
    let data = std::fs::read(path).map_err(|e| RenderError::SvgIo(path.clone(), e))?;
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default())
        .map_err(|e| RenderError::SvgParse(path.clone(), e.to_string()))?;

    let size = tree.size();
    let (sw, sh) = (size.width().max(1.0), size.height().max(1.0));
    let mut pixmap = resvg::tiny_skia::Pixmap::new(target_w.max(1), target_h.max(1))
        .ok_or_else(|| RenderError::SvgParse(path.clone(), "invalid target size".to_string()))?;
    let transform = resvg::tiny_skia::Transform::from_scale(target_w as f32 / sw, target_h as f32 / sh);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let mut gray = GrayImage::from_pixel(target_w, target_h, image::Luma([255u8]));
    for (i, px) in pixmap.pixels().iter().enumerate() {
        let a = px.alpha();
        if a == 0 {
            continue;
        }
        // Premultiplied RGB, composited over opaque white: the
        // "darkening" an alpha-blended pixel contributes is exactly its
        // premultiplied luma; what's left of the white background is
        // (255 - alpha).
        let lum = 0.299 * px.red() as f32 + 0.587 * px.green() as f32 + 0.114 * px.blue() as f32;
        let out = (lum + (255 - a) as f32).clamp(0.0, 255.0) as u8;
        let x = (i as u32) % target_w;
        let y = (i as u32) / target_w;
        gray.put_pixel(x, y, image::Luma([out]));
    }
    Ok(gray)
}

/// Stroke width for an unfilled shape's outline, in supersampled pixels.
/// A plain 1px outline (what imageproc's `draw_hollow_*` give you)
/// mostly disappears once the final downscale divides by `SUPER` — this
/// keeps about one solid dot of outline in the printed result.
const SHAPE_STROKE: i32 = SUPER as i32;

/// Spacing between dots along a dotted outline, in supersampled pixels.
const DOT_SPACING: f32 = (SHAPE_STROKE * 3) as f32;

fn draw_dots_along(canvas: &mut GrayImage, points: &[(f32, f32)], color: image::Luma<u8>) {
    if points.len() < 2 {
        return;
    }
    let r = (SHAPE_STROKE / 2).max(1);
    let mut carry = 0.0f32;
    for pair in points.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 0.0 {
            continue;
        }
        let mut dist = DOT_SPACING - carry;
        while dist < len {
            let t = dist / len;
            let (px, py) = (x0 + dx * t, y0 + dy * t);
            draw_filled_circle_mut(canvas, (px.round() as i32, py.round() as i32), r, color);
            dist += DOT_SPACING;
        }
        carry = len - (dist - DOT_SPACING);
    }
}

fn draw_shape_element(canvas: &mut GrayImage, shape: ShapeKind, size: f32, style: ShapeStyle, x: i64, y: i64) {
    let black = image::Luma([0u8]);
    let white = image::Luma([255u8]);
    let s = ((size * SUPER as f32).round() as i32).max(2);
    let (x, y) = (x as i32, y as i32);

    match shape {
        ShapeKind::Circle => {
            let r = s / 2;
            let center = (x + r, y + r);
            match style {
                ShapeStyle::Filled => draw_filled_circle_mut(canvas, center, r, black),
                ShapeStyle::Outline => {
                    draw_filled_circle_mut(canvas, center, r, black);
                    if r > SHAPE_STROKE {
                        draw_filled_circle_mut(canvas, center, r - SHAPE_STROKE, white);
                    }
                }
                ShapeStyle::Dotted => {
                    let steps = 64;
                    let pts: Vec<(f32, f32)> = (0..=steps)
                        .map(|i| {
                            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
                            (center.0 as f32 + r as f32 * a.cos(), center.1 as f32 + r as f32 * a.sin())
                        })
                        .collect();
                    draw_dots_along(canvas, &pts, black);
                }
            }
        }
        ShapeKind::Square => {
            let rect = Rect::at(x, y).of_size(s as u32, s as u32);
            match style {
                ShapeStyle::Filled => draw_filled_rect_mut(canvas, rect, black),
                ShapeStyle::Outline => {
                    draw_filled_rect_mut(canvas, rect, black);
                    if s > SHAPE_STROKE * 2 {
                        let inner = Rect::at(x + SHAPE_STROKE, y + SHAPE_STROKE)
                            .of_size((s - SHAPE_STROKE * 2) as u32, (s - SHAPE_STROKE * 2) as u32);
                        draw_filled_rect_mut(canvas, inner, white);
                    }
                }
                ShapeStyle::Dotted => {
                    let (xf, yf, sf) = (x as f32, y as f32, s as f32);
                    let corners = [(xf, yf), (xf + sf, yf), (xf + sf, yf + sf), (xf, yf + sf), (xf, yf)];
                    draw_dots_along(canvas, &corners, black);
                }
            }
        }
        ShapeKind::Triangle => {
            let triangle = |inset: i32| {
                [
                    Point::new(x + s / 2, y + inset * 2),
                    Point::new(x + inset, y + s - 1 - inset),
                    Point::new(x + s - 1 - inset, y + s - 1 - inset),
                ]
            };
            match style {
                ShapeStyle::Filled => draw_polygon_mut(canvas, &triangle(0), black),
                ShapeStyle::Outline => {
                    draw_polygon_mut(canvas, &triangle(0), black);
                    if s > SHAPE_STROKE * 3 {
                        draw_polygon_mut(canvas, &triangle(SHAPE_STROKE), white);
                    }
                }
                ShapeStyle::Dotted => {
                    let t = triangle(0);
                    let pts: Vec<(f32, f32)> = [t[0], t[1], t[2], t[0]]
                        .iter()
                        .map(|p| (p.x as f32, p.y as f32))
                        .collect();
                    draw_dots_along(canvas, &pts, black);
                }
            }
        }
    }
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

/// Returns an image file's width/height aspect ratio (width / height),
/// for both raster formats and SVG — `image::image_dimensions` only
/// handles the former, and a builder UI needs this to size a newly added
/// image/icon element without distorting it.
pub fn image_aspect_ratio(path: &std::path::Path) -> Option<f32> {
    let is_svg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if is_svg {
        let data = std::fs::read(path).ok()?;
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let size = tree.size();
        if size.height() > 0.0 {
            Some(size.width() / size.height())
        } else {
            None
        }
    } else {
        let (w, h) = image::image_dimensions(path).ok()?;
        if h > 0 {
            Some(w as f32 / h as f32)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_document_is_blank_and_sized() {
        let doc = LabelDocument::new(100, 32);
        let raster = raster_from_document(&doc, &RenderOptions::default()).unwrap();
        assert_eq!(raster.cross, 32);
        // feed is doubled relative to the document's logical length to
        // correct for the printer's non-square dot pitch (see comment in
        // raster_from_document).
        assert_eq!(raster.feed, 200);
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
