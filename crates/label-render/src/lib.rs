//! Turns text or an arbitrary image into a [`label_core::Raster`] the
//! printer can print: rotate to portrait, resize to the print head's
//! fixed cross-feed resolution, then binarize.

pub mod document;

use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use image::{DynamicImage, GrayImage, imageops};
use imageproc::drawing::{draw_text_mut, text_size};
use label_core::{Raster, PRINT_HEAD_DOTS};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("could not read font file {0}: {1}")]
    FontIo(PathBuf, std::io::Error),
    #[error("could not parse font file {0}")]
    FontParse(PathBuf),
    #[error("no usable font found; pass an explicit font path")]
    NoDefaultFont,
    #[error(transparent)]
    Image(#[from] image::ImageError),
}

/// Dithering strategy used to turn grayscale into the printer's 1-bit
/// raster.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dither {
    /// Hard black/white cut at the given luma threshold (0-255).
    Threshold(u8),
    FloydSteinberg,
    Atkinson,
}

impl Default for Dither {
    fn default() -> Self {
        Dither::Threshold(128)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// Options shared by text and image rendering.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Print head cross-feed resolution in dots. 32 on the LT200B.
    pub cross: u32,
    pub dither: Dither,
    pub invert: bool,
    /// -100.0..=100.0, 0 = no change.
    pub contrast: f32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            cross: PRINT_HEAD_DOTS,
            dither: Dither::default(),
            invert: false,
            contrast: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TextOptions {
    pub font_path: Option<PathBuf>,
    pub bold: bool,
    pub size: f32,
    pub align: TextAlign,
    /// Draw a 1px frame around the whole label.
    pub frame: bool,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            font_path: None,
            bold: false,
            size: 64.0,
            align: TextAlign::Left,
            frame: false,
        }
    }
}

/// Common installed-font locations to try when no explicit path is given.
fn default_font_candidates(bold: bool) -> &'static [&'static str] {
    if bold {
        &[
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "C:\\Windows\\Fonts\\arialbd.ttf",
        ]
    } else {
        &[
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "C:\\Windows\\Fonts\\arial.ttf",
        ]
    }
}

pub(crate) fn load_font(path: Option<&Path>, bold: bool) -> Result<FontVec, RenderError> {
    let candidates: Vec<PathBuf> = match path {
        Some(p) => vec![p.to_path_buf()],
        None => default_font_candidates(bold).iter().map(PathBuf::from).collect(),
    };

    for candidate in &candidates {
        match std::fs::read(candidate) {
            Ok(bytes) => {
                return FontVec::try_from_vec(bytes)
                    .map_err(|_| RenderError::FontParse(candidate.clone()));
            }
            Err(e) if path.is_some() => return Err(RenderError::FontIo(candidate.clone(), e)),
            Err(_) => continue,
        }
    }
    Err(RenderError::NoDefaultFont)
}

/// Renders one or more lines of text to a [`Raster`].
pub fn raster_from_text(
    lines: &[&str],
    text_opts: &TextOptions,
    render_opts: &RenderOptions,
) -> Result<Raster, RenderError> {
    let font = load_font(text_opts.font_path.as_deref(), text_opts.bold)?;
    let font_ref: &FontVec = &font;
    let scale = PxScale::from(text_opts.size);
    let scaled = font_ref.as_scaled(scale);

    let line_height = (scaled.height().ceil() as i32).max(1);
    let widths: Vec<i32> = lines
        .iter()
        .map(|l| text_size(scale, font_ref, l).0 as i32)
        .collect();
    let max_width = widths.iter().copied().max().unwrap_or(1).max(1);
    let total_height = line_height * lines.len().max(1) as i32;

    // The whole canvas's narrow (height) dimension gets stretched to
    // exactly fill the print head's cross-feed dots after rotation, so
    // anything drawn on the canvas's own edge lands on the printer's
    // outermost dots. Thermal print heads commonly don't reliably mark
    // those (tape-width tolerance margin), which clips a frame drawn
    // flush with the edge. `frame_margin` keeps the frame — and really
    // all content — a few dots in from the absolute edge.
    let frame_margin = if text_opts.frame { 3 } else { 0 };
    let pad = if text_opts.frame { 4 } else { 0 };
    let outer = pad + frame_margin;
    let mut canvas = GrayImage::from_pixel(
        (max_width + outer * 2) as u32,
        (total_height + outer * 2) as u32,
        image::Luma([255u8]),
    );

    for (i, (line, width)) in lines.iter().zip(widths.iter()).enumerate() {
        let x = outer
            + match text_opts.align {
                TextAlign::Left => 0,
                TextAlign::Center => (max_width - width) / 2,
                TextAlign::Right => max_width - width,
            };
        let y = outer + line_height * i as i32;
        draw_text_mut(&mut canvas, image::Luma([0u8]), x, y, scale, font_ref, line);
        if text_opts.bold {
            // faux-bold: redraw offset by 1px when no separate bold face was found
            draw_text_mut(&mut canvas, image::Luma([0u8]), x + 1, y, scale, font_ref, line);
        }
    }

    if text_opts.frame {
        let (w, h) = (canvas.width(), canvas.height());
        imageproc::drawing::draw_hollow_rect_mut(
            &mut canvas,
            imageproc::rect::Rect::at(frame_margin, frame_margin)
                .of_size(w - frame_margin as u32 * 2, h - frame_margin as u32 * 2),
            image::Luma([0u8]),
        );
    }

    Ok(raster_from_dynamic_image(
        &DynamicImage::ImageLuma8(canvas),
        render_opts,
    ))
}

/// Rotates, resizes and binarizes an arbitrary image into a [`Raster`].
///
/// Mirrors the upstream pipeline's geometry (rotate to portrait, resize so
/// the cross-feed dimension matches the print head, with the feed length
/// scaled by the printer's known non-square dot pitch) but resizes the
/// full-precision grayscale image before binarizing, so dithering has real
/// gradient information to work with.
pub fn raster_from_dynamic_image(img: &DynamicImage, opts: &RenderOptions) -> Raster {
    let gray = img.to_luma8();
    let rotated = imageops::rotate90(&gray);

    let (rw, rh) = rotated.dimensions();
    let cross = opts.cross.max(1);
    // The printer's feed-direction dot pitch is roughly half its
    // cross-feed pitch, hence the factor of 2 relative to a plain
    // aspect-preserving scale to `cross`.
    let feed = ((2 * cross) as f32 * (rh as f32 / rw as f32)).round().max(1.0) as u32;

    let resized = imageops::resize(&rotated, cross, feed, imageops::FilterType::Lanczos3);
    let adjusted = if opts.contrast != 0.0 {
        imageops::contrast(&resized, opts.contrast)
    } else {
        resized
    };

    let bits = binarize(&adjusted, opts.dither, opts.invert);
    Raster::new(cross, feed, bits)
}

pub(crate) fn binarize(img: &GrayImage, dither: Dither, invert: bool) -> Vec<bool> {
    let (w, h) = img.dimensions();
    let black = |v: bool| if invert { !v } else { v };

    match dither {
        Dither::Threshold(t) => img.pixels().map(|p| black(p.0[0] < t)).collect(),
        Dither::FloydSteinberg => {
            let mut buf: Vec<f32> = img.pixels().map(|p| p.0[0] as f32).collect();
            let mut out = vec![false; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    let old = buf[i];
                    let new = if old < 128.0 { 0.0 } else { 255.0 };
                    let err = old - new;
                    out[i] = black(new == 0.0);
                    let mut add = |dx: i64, dy: i64, weight: f32| {
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if nx >= 0 && ny >= 0 && (nx as u32) < w && (ny as u32) < h {
                            let ni = (ny as u32 * w + nx as u32) as usize;
                            buf[ni] += err * weight;
                        }
                    };
                    add(1, 0, 7.0 / 16.0);
                    add(-1, 1, 3.0 / 16.0);
                    add(0, 1, 5.0 / 16.0);
                    add(1, 1, 1.0 / 16.0);
                }
            }
            out
        }
        Dither::Atkinson => {
            let mut buf: Vec<f32> = img.pixels().map(|p| p.0[0] as f32).collect();
            let mut out = vec![false; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    let old = buf[i];
                    let new = if old < 128.0 { 0.0 } else { 255.0 };
                    let err = (old - new) / 8.0;
                    out[i] = black(new == 0.0);
                    let mut add = |dx: i64, dy: i64| {
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if nx >= 0 && ny >= 0 && (nx as u32) < w && (ny as u32) < h {
                            let ni = (ny as u32 * w + nx as u32) as usize;
                            buf[ni] += err;
                        }
                    };
                    add(1, 0);
                    add(2, 0);
                    add(-1, 1);
                    add(0, 1);
                    add(1, 1);
                    add(0, 2);
                }
            }
            out
        }
    }
}

pub(crate) fn raster_to_gray(raster: &Raster) -> GrayImage {
    let mut img = GrayImage::from_pixel(raster.cross, raster.feed, image::Luma([255u8]));
    for (i, &bit) in raster.bits.iter().enumerate() {
        let x = (i as u32) % raster.cross;
        let y = (i as u32) / raster.cross;
        img.put_pixel(x, y, image::Luma([if bit { 0 } else { 255 }]));
    }
    img
}

fn encode_png(img: GrayImage, scale: u32) -> Result<Vec<u8>, RenderError> {
    let scale = scale.max(1);
    let (w, h) = img.dimensions();
    let upscaled = imageops::resize(&img, w * scale, h * scale, imageops::FilterType::Nearest);
    let mut bytes = Vec::new();
    DynamicImage::ImageLuma8(upscaled)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// Renders a raster to a pixel-exact PNG in the print head's raw feed
/// orientation (rotated 90° from how a human reads the label). Use this to
/// verify exactly what will be sent to the printer.
pub fn raw_preview_png(raster: &Raster, scale: u32) -> Result<Vec<u8>, RenderError> {
    encode_png(raster_to_gray(raster), scale)
}

/// Renders a raster to a pixel-exact PNG rotated back to how a human reads
/// the finished label. This is what a desktop/CLI preview should show.
pub fn preview_png(raster: &Raster, scale: u32) -> Result<Vec<u8>, RenderError> {
    let upright = imageops::rotate270(&raster_to_gray(raster));
    encode_png(upright, scale)
}

/// Converts a raster's dot grid into an estimated label length in
/// millimeters, given the printer's feed-direction dots-per-mm.
pub fn estimate_length_mm(raster: &Raster, feed_dots_per_mm: f32) -> f32 {
    raster.feed as f32 / feed_dots_per_mm.max(0.01)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_binarize_matches_simple_cutoff() {
        let img = GrayImage::from_fn(2, 1, |x, _| image::Luma([if x == 0 { 10 } else { 250 }]));
        let bits = binarize(&img, Dither::Threshold(128), false);
        assert_eq!(bits, vec![true, false]);
    }

    #[test]
    fn invert_flips_result() {
        let img = GrayImage::from_fn(2, 1, |x, _| image::Luma([if x == 0 { 10 } else { 250 }]));
        let bits = binarize(&img, Dither::Threshold(128), true);
        assert_eq!(bits, vec![false, true]);
    }

    #[test]
    fn raster_from_dynamic_image_has_expected_cross() {
        let img = DynamicImage::ImageLuma8(GrayImage::from_pixel(100, 50, image::Luma([200])));
        let raster = raster_from_dynamic_image(&img, &RenderOptions::default());
        assert_eq!(raster.cross, 32);
        assert_eq!(raster.bits.len() as u32, raster.cross * raster.feed);
    }
}

#[cfg(test)]
mod text_tests {
    use super::*;

    #[test]
    fn raster_from_text_produces_nonempty_raster() {
        let opts = TextOptions::default();
        let render_opts = RenderOptions::default();
        let raster = raster_from_text(&["Hello"], &opts, &render_opts).expect("render ok");
        assert_eq!(raster.cross, 32);
        assert!(raster.feed > 0);
        assert!(raster.bits.iter().any(|&b| b), "expected some black pixels");
    }
}
