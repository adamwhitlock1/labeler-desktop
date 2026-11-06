use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use label_core::create_job;
use label_render::{raster_from_dynamic_image, raster_from_text, Dither, RenderOptions, TextAlign, TextOptions};

#[derive(Parser)]
#[command(name = "label-cli", about = "Print to a DYMO LT200B label maker over Bluetooth LE")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan for nearby BLE devices.
    Scan {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Print one or more lines of text.
    Text {
        /// Printer id, from `scan` (or a saved config value).
        #[arg(long)]
        device: String,
        text: Vec<String>,
        #[arg(long, default_value_t = 64.0)]
        size: f32,
        #[arg(long)]
        bold: bool,
        #[arg(long, value_enum, default_value_t = Align::Left)]
        align: Align,
        #[arg(long)]
        font: Option<PathBuf>,
        #[arg(long)]
        frame: bool,
        #[arg(long, value_enum, default_value_t = DitherArg::Threshold)]
        dither: DitherArg,
        #[arg(long)]
        invert: bool,
        #[arg(long, default_value_t = 1)]
        copies: u32,
        /// Don't print; just write a preview PNG.
        #[arg(long)]
        dry_run: Option<PathBuf>,
    },
    /// Print an image file.
    Image {
        #[arg(long)]
        device: String,
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = DitherArg::FloydSteinberg)]
        dither: DitherArg,
        #[arg(long)]
        invert: bool,
        #[arg(long, default_value_t = 0.0)]
        contrast: f32,
        #[arg(long, default_value_t = 1)]
        copies: u32,
        #[arg(long)]
        dry_run: Option<PathBuf>,
    },
    /// Render a preview PNG without connecting to a printer.
    Preview {
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        image: Option<PathBuf>,
        #[arg(long, default_value = "/tmp/label-preview.png")]
        out: PathBuf,
        #[arg(long, default_value_t = 8)]
        scale: u32,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Align {
    Left,
    Center,
    Right,
}

impl From<Align> for TextAlign {
    fn from(a: Align) -> Self {
        match a {
            Align::Left => TextAlign::Left,
            Align::Center => TextAlign::Center,
            Align::Right => TextAlign::Right,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum DitherArg {
    Threshold,
    FloydSteinberg,
    Atkinson,
}

impl From<DitherArg> for Dither {
    fn from(d: DitherArg) -> Self {
        match d {
            DitherArg::Threshold => Dither::Threshold(128),
            DitherArg::FloydSteinberg => Dither::FloydSteinberg,
            DitherArg::Atkinson => Dither::Atkinson,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Scan { seconds } => {
            println!("Scanning for {seconds}s...");
            let found = label_device::scan(Duration::from_secs(seconds)).await?;
            if found.is_empty() {
                println!("No named devices found.");
            }
            for d in found {
                println!(
                    "{}  {}{}",
                    d.id,
                    d.name.as_deref().unwrap_or("(unnamed)"),
                    d.rssi.map(|r| format!("  rssi={r}")).unwrap_or_default()
                );
            }
        }
        Command::Text {
            device,
            text,
            size,
            bold,
            align,
            font,
            frame,
            dither,
            invert,
            copies,
            dry_run,
        } => {
            if text.is_empty() {
                bail!("provide text to print");
            }
            let lines: Vec<&str> = text.iter().map(String::as_str).collect();
            let text_opts = TextOptions {
                font_path: font,
                bold,
                size,
                align: align.into(),
                frame,
            };
            let render_opts = RenderOptions {
                dither: dither.into(),
                invert,
                ..Default::default()
            };
            let raster = raster_from_text(&lines, &text_opts, &render_opts)
                .context("rendering text")?;
            println!("rendered {}x{} dots", raster.cross, raster.feed);

            if let Some(out) = dry_run {
                let png = label_render::preview_png(&raster, 6)?;
                std::fs::write(&out, png)?;
                println!("wrote preview to {}", out.display());
                return Ok(());
            }

            let job = create_job(&raster);
            let printer = label_device::Printer::connect(&device).await?;
            for i in 0..copies.max(1) {
                printer.print_job(&job).await?;
                println!("printed copy {}/{copies}", i + 1);
            }
            printer.disconnect().await.ok();
        }
        Command::Image {
            device,
            path,
            dither,
            invert,
            contrast,
            copies,
            dry_run,
        } => {
            let img = image::open(&path).with_context(|| format!("opening {}", path.display()))?;
            let render_opts = RenderOptions {
                dither: dither.into(),
                invert,
                contrast,
                ..Default::default()
            };
            let raster = raster_from_dynamic_image(&img, &render_opts);
            println!("rendered {}x{} dots", raster.cross, raster.feed);

            if let Some(out) = dry_run {
                let png = label_render::preview_png(&raster, 6)?;
                std::fs::write(&out, png)?;
                println!("wrote preview to {}", out.display());
                return Ok(());
            }

            let job = create_job(&raster);
            let printer = label_device::Printer::connect(&device).await?;
            for i in 0..copies.max(1) {
                printer.print_job(&job).await?;
                println!("printed copy {}/{copies}", i + 1);
            }
            printer.disconnect().await.ok();
        }
        Command::Preview { text, image: image_path, out, scale } => {
            let raster = if let Some(text) = text {
                raster_from_text(&[text.as_str()], &TextOptions::default(), &RenderOptions::default())?
            } else if let Some(path) = image_path {
                let img = image::open(&path)?;
                raster_from_dynamic_image(&img, &RenderOptions::default())
            } else {
                bail!("provide --text or --image");
            };
            let png = label_render::preview_png(&raster, scale)?;
            std::fs::write(&out, png)?;
            println!("wrote {} ({}x{} dots)", out.display(), raster.cross, raster.feed);
        }
    }

    Ok(())
}
