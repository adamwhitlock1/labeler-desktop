mod icons;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// btleplug's macOS backend requires an active Tokio reactor for the
/// lifetime of any Bluetooth operation (CoreBluetooth delegate callbacks
/// are bridged through Tokio channels/tasks). GPUI's own async executor
/// isn't Tokio, so every `label_device` call is spawned onto this runtime
/// instead of being awaited directly in a `cx.spawn` future.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().expect("failed to start Tokio runtime"))
}

/// Matches the LT200B and its siblings by advertised name — "Letratag
/// DC067557FF1E" and similar, loosely, so any Dymo/LetraTag/generic
/// "label" printer that shows up still gets recognized.
fn is_compatible_printer_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("letra") || n.contains("dymo") || n.contains("label")
}

fn dymo_icon_image() -> Arc<gpui_kit::Image> {
    static ICON: OnceLock<Arc<gpui_kit::Image>> = OnceLock::new();
    ICON.get_or_init(|| {
        Arc::new(gpui_kit::Image::from_bytes(
            gpui_kit::ImageFormat::Png,
            include_bytes!("../assets/dymo-letratag.png").to_vec(),
        ))
    })
    .clone()
}

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::notification::NotificationType;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::WindowExt;
use gpui_kit::component::*;
use gpui_kit::base::Disableable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{Image, ImageFormat, PathPromptOptions};
use gpui_kit::*;
use label_core::{create_job, PRINT_HEAD_DOTS};
use label_render::document::{
    raster_from_document, Element as DocElement, ElementKind as DocElementKind, LabelDocument, ShapeKind,
    ShapeStyle,
};
use label_render::fonts::{available_fonts, FontChoice};
use label_render::{Dither, RenderOptions, TextAlign};
use serde::{Deserialize, Serialize};

/// Preview pixels per print dot in the builder canvas.
const PREVIEW_SCALE: f32 = 6.0;
const DEFAULT_BUILDER_FEED_DOTS: u32 = 175;
/// Measured directly off a printed label: the 32-dot printable band is
/// 6mm tall (see [`label_render::TapeProfile::default`]).
const CROSS_DOTS_PER_MM: f32 = PRINT_HEAD_DOTS as f32 / 6.0;
/// New text starts at 60% of the size slider's max.
const DEFAULT_TEXT_SIZE: f32 = 46.0 * 0.6;

#[derive(Clone, Serialize, Deserialize)]
enum BuilderKind {
    Text {
        text: String,
        size: f32,
        bold: bool,
        /// `None` = system default. `#[serde(default)]` so labels saved
        /// before this field existed still load.
        #[serde(default)]
        font_path: Option<PathBuf>,
    },
    Image { path: PathBuf, width: f32, height: f32, aspect: f32 },
    Shape { shape: ShapeKind, size: f32, style: ShapeStyle },
}

#[derive(Clone, Serialize, Deserialize)]
struct BuilderElement {
    id: u64,
    /// Top-left, in preview pixels (canvas-local).
    x: f32,
    y: f32,
    kind: BuilderKind,
}

#[derive(Serialize, Deserialize)]
struct SavedLabel {
    feed_dots: u32,
    elements: Vec<BuilderElement>,
}


struct LabelApp {
    // device
    scanning: bool,
    found: Vec<label_device::DiscoveredPrinter>,
    device_id: Option<String>,
    device_name: Option<String>,
    other_devices_expanded: bool,
    /// GPUI dispatches key events to whatever has focus, not just "the
    /// root" — this is what `on_key_down` on the root div actually needs
    /// in order to receive Delete/Backspace at all.
    focus_handle: gpui_kit::FocusHandle,

    // whole-document render options
    contrast: Entity<SliderState>,
    invert: bool,
    copies: u32,
    printing: bool,

    // builder
    builder_elements: Vec<BuilderElement>,
    builder_selected: Option<u64>,
    builder_next_id: u64,
    builder_feed_dots: u32,
    builder_feed_input: Entity<InputState>,
    builder_text_edit: Entity<InputState>,
    icon_picker_open: bool,
    icon_search: Entity<InputState>,
    builder_size_slider: Entity<SliderState>,
    builder_bold: bool,
    builder_shape_style: ShapeStyle,
    /// System fonts available to the text tool, enumerated once at
    /// startup. "Default" (index 0) maps to `font_path: None`.
    builder_font_choices: Vec<FontChoice>,
    builder_font_select: Entity<SelectState<SearchableVec<String>>>,
    /// Font files already handed to GPUI's text system, so the editor
    /// canvas can render the real typeface — loading is a one-time cost
    /// per font, not something to redo every render.
    loaded_fonts: std::collections::HashSet<PathBuf>,
    canvas_origin: Rc<Cell<Point<Pixels>>>,
    /// (element id, grab offset within the element, in canvas-local px)
    drag_grab: Option<(u64, f32, f32)>,
    /// Cached (1x, 3x) dithered print preview, refreshed by
    /// `maybe_recompute_preview` on a throttle instead of every render —
    /// the full raster/dither pass is expensive enough (font shaping,
    /// resize, dithering) that doing it on every keystroke or drag-move
    /// made typing and dragging visibly laggy.
    preview_cache: Option<Arc<Image>>,
    last_preview_compute: std::time::Instant,
    preview_catchup_scheduled: bool,

    _subscriptions: Vec<Subscription>,
}

impl LabelApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let contrast = cx.new(|_| {
            SliderState::new().min(-100.0).max(100.0).default_value(0.0).step(1.0)
        });
        let builder_feed_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(DEFAULT_BUILDER_FEED_DOTS.to_string())
        });
        let builder_text_edit = cx.new(|cx| InputState::new(window, cx).placeholder("Text..."));
        let icon_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search icons..."));
        let builder_size_slider = cx.new(|_| {
            // Shared by text, image and shape elements. Well over the
            // tape's physical 32-dot height: sizing past that (or
            // dragging past the edge) is the user's call.
            SliderState::new().min(6.0).max(46.0).default_value(DEFAULT_TEXT_SIZE).step(1.0)
        });

        let builder_font_choices = available_fonts();
        let font_options = SearchableVec::new(
            std::iter::once("Default".to_string())
                .chain(builder_font_choices.iter().map(|f| f.family.clone()))
                .collect::<Vec<_>>(),
        );
        let builder_font_select =
            cx.new(|cx| SelectState::new(font_options, Some(IndexPath::default()), window, cx).searchable(true));

        let mut subs = Vec::new();
        subs.push(cx.subscribe(&builder_feed_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.update_feed_dots(cx);
            }
        }));
        subs.push(cx.subscribe(&builder_text_edit, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.update_selected_text(cx);
            }
        }));
        subs.push(cx.subscribe(&builder_size_slider, |this, _, event, cx| {
            if let SliderEvent::Change(v) = event {
                this.update_selected_size(v.start(), cx);
            }
        }));
        subs.push(cx.subscribe(&icon_search, |_this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));
        subs.push(cx.subscribe(&builder_font_select, |this, _, _event: &SelectEvent<SearchableVec<String>>, cx| {
            this.update_selected_font(cx);
        }));

        Self {
            scanning: false,
            found: Vec::new(),
            device_id: None,
            device_name: None,
            other_devices_expanded: false,
            focus_handle,
            contrast,
            invert: false,
            copies: 1,
            printing: false,
            builder_elements: Vec::new(),
            builder_selected: None,
            builder_next_id: 1,
            builder_feed_dots: DEFAULT_BUILDER_FEED_DOTS,
            builder_feed_input,
            builder_text_edit,
            icon_picker_open: false,
            icon_search,
            builder_size_slider,
            builder_bold: false,
            builder_shape_style: ShapeStyle::Filled,
            builder_font_choices,
            builder_font_select,
            loaded_fonts: std::collections::HashSet::new(),
            canvas_origin: Rc::new(Cell::new(Point::default())),
            drag_grab: None,
            preview_cache: None,
            last_preview_compute: std::time::Instant::now() - std::time::Duration::from_secs(1),
            preview_catchup_scheduled: false,
            _subscriptions: subs,
        }
    }

    fn render_opts(&self, cx: &App) -> RenderOptions {
        RenderOptions {
            dither: Dither::Threshold(128),
            invert: self.invert,
            contrast: self.contrast.read(cx).value().start(),
            ..Default::default()
        }
    }

    fn update_feed_dots(&mut self, cx: &mut Context<Self>) {
        let text = self.builder_feed_input.read(cx).value().to_string();
        if let Ok(dots) = text.trim().parse::<u32>() {
            self.builder_feed_dots = dots.clamp(10, 2000);
        }
        cx.notify();
    }

    /// Up/down arrow keys on the length field, since it's a plain number
    /// input rather than a stepper widget.
    fn step_feed_dots(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let new = (self.builder_feed_dots as i32 + delta).clamp(10, 2000) as u32;
        self.builder_feed_dots = new;
        self.builder_feed_input.update(cx, |s, cx| {
            s.set_value(new.to_string(), window, cx);
        });
        cx.notify();
    }

    fn current_job(&self, cx: &App) -> Option<Vec<Vec<u8>>> {
        if self.builder_elements.is_empty() {
            return None;
        }
        let render_opts = self.render_opts(cx);
        let doc = self.builder_document();
        let raster = raster_from_document(&doc, &render_opts).ok()?;
        Some(create_job(&raster))
    }

    fn start_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scanning = true;
        self.found.clear();
        cx.notify();
        cx.spawn_in(window, async move |view, window| {
            let found = runtime()
                .spawn(label_device::scan(Duration::from_secs(5)))
                .await
                .ok()
                .and_then(|r| r.ok());
            window
                .update(|_, cx| {
                    view.update(cx, |this, cx| {
                        this.scanning = false;
                        if let Some(found) = found {
                            this.found = found;
                            let compatible = this
                                .found
                                .iter()
                                .find(|d| d.name.as_deref().is_some_and(is_compatible_printer_name))
                                .map(|d| (d.id.clone(), d.name.clone()));
                            if let Some((id, name)) = compatible {
                                this.select_device(id, name, cx);
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    fn select_device(&mut self, id: String, name: Option<String>, cx: &mut Context<Self>) {
        self.device_id = Some(id);
        self.device_name = name;
        cx.notify();
    }


    fn selected_element(&self) -> Option<&BuilderElement> {
        let id = self.builder_selected?;
        self.builder_elements.iter().find(|e| e.id == id)
    }

    fn select_element(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.builder_selected = Some(id);
        // Selecting an element (a plain click on a div) doesn't itself
        // grant keyboard focus; without this, Delete/Backspace on the
        // root's on_key_down never fires because nothing holds focus.
        window.focus(&self.focus_handle, cx);
        if let Some(el) = self.builder_elements.iter().find(|e| e.id == id) {
            match &el.kind {
                BuilderKind::Text { text, size, bold, font_path } => {
                    let (text, size, bold, font_path) = (text.clone(), *size, *bold, font_path.clone());
                    self.builder_text_edit.update(cx, |s, cx| {
                        s.set_value(text, window, cx);
                    });
                    self.builder_size_slider.update(cx, |s, cx| {
                        s.set_value(size, window, cx);
                    });
                    self.builder_bold = bold;
                    let family = font_path
                        .as_ref()
                        .and_then(|p| self.builder_font_choices.iter().find(|f| &f.path == p))
                        .map(|f| f.family.clone())
                        .unwrap_or_else(|| "Default".to_string());
                    self.builder_font_select.update(cx, |s, cx| {
                        s.set_selected_value(&family, window, cx);
                    });
                }
                BuilderKind::Image { height, .. } => {
                    let height = *height;
                    self.builder_size_slider.update(cx, |s, cx| {
                        s.set_value(height, window, cx);
                    });
                }
                BuilderKind::Shape { size, style, .. } => {
                    let (size, style) = (*size, *style);
                    self.builder_size_slider.update(cx, |s, cx| {
                        s.set_value(size, window, cx);
                    });
                    self.builder_shape_style = style;
                }
            }
        }
        cx.notify();
    }

    fn update_selected_text(&mut self, cx: &mut Context<Self>) {
        let text = self.builder_text_edit.read(cx).value().to_string();
        let Some(id) = self.builder_selected else { return };
        if let Some(el) = self.builder_elements.iter_mut().find(|e| e.id == id) {
            if let BuilderKind::Text { text: t, .. } = &mut el.kind {
                *t = text;
            }
        }
        cx.notify();
    }

    fn update_selected_size(&mut self, value: f32, cx: &mut Context<Self>) {
        let Some(id) = self.builder_selected else { return };
        if let Some(el) = self.builder_elements.iter_mut().find(|e| e.id == id) {
            match &mut el.kind {
                BuilderKind::Text { size, .. } => *size = value,
                BuilderKind::Image { width, height, aspect, .. } => {
                    *height = value;
                    *width = value * *aspect;
                }
                BuilderKind::Shape { size, .. } => *size = value,
            }
        }
        cx.notify();
    }

    /// Hands every font a text element currently references to GPUI's text
    /// system, so `.font_family(...)` in the canvas can actually find it —
    /// otherwise the editor silently falls back to the default UI font
    /// while only the offline-rendered print preview picks up the choice.
    fn ensure_fonts_loaded(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self
            .builder_elements
            .iter()
            .filter_map(|el| match &el.kind {
                BuilderKind::Text { font_path: Some(p), .. } if !self.loaded_fonts.contains(p) => {
                    Some(p.clone())
                }
                _ => None,
            })
            .collect();
        for path in paths {
            if let Ok(bytes) = std::fs::read(&path) {
                let _ = cx.text_system().add_fonts(vec![std::borrow::Cow::Owned(bytes)]);
            }
            self.loaded_fonts.insert(path);
        }
    }

    /// Re-scans installed system fonts so a font installed after the app
    /// launched (e.g. dropped into `~/Library/Fonts` while running) shows
    /// up without restarting — `fontdb`'s scan only happens when asked.
    fn refresh_fonts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.builder_font_choices = available_fonts();
        let font_options = SearchableVec::new(
            std::iter::once("Default".to_string())
                .chain(self.builder_font_choices.iter().map(|f| f.family.clone()))
                .collect::<Vec<_>>(),
        );
        let previous = self.builder_font_select.read(cx).selected_value().cloned();
        self.builder_font_select.update(cx, |s, cx| {
            s.set_items(font_options, window, cx);
            if let Some(value) = previous {
                s.set_selected_value(&value, window, cx);
            }
        });
        cx.notify();
    }

    fn update_selected_font(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.builder_selected else { return };
        let family = self.builder_font_select.read(cx).selected_value().cloned();
        let font_path = family
            .filter(|f| f != "Default")
            .and_then(|f| self.builder_font_choices.iter().find(|c| c.family == f))
            .map(|c| c.path.clone());
        if let Some(el) = self.builder_elements.iter_mut().find(|e| e.id == id) {
            if let BuilderKind::Text { font_path: p, .. } = &mut el.kind {
                *p = font_path;
            }
        }
        cx.notify();
    }

    fn toggle_selected_bold(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.builder_selected else { return };
        self.builder_bold = !self.builder_bold;
        let bold = self.builder_bold;
        if let Some(el) = self.builder_elements.iter_mut().find(|e| e.id == id) {
            if let BuilderKind::Text { bold: b, .. } = &mut el.kind {
                *b = bold;
            }
        }
        cx.notify();
    }

    fn set_selected_shape_style(&mut self, style: ShapeStyle, cx: &mut Context<Self>) {
        let Some(id) = self.builder_selected else { return };
        self.builder_shape_style = style;
        if let Some(el) = self.builder_elements.iter_mut().find(|e| e.id == id) {
            if let BuilderKind::Shape { style: s, .. } = &mut el.kind {
                *s = style;
            }
        }
        cx.notify();
    }

    fn add_shape_element(&mut self, shape: ShapeKind, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.builder_next_id;
        self.builder_next_id += 1;
        self.builder_elements.push(BuilderElement {
            id,
            x: 10.0,
            y: 4.0,
            kind: BuilderKind::Shape { shape, size: 20.0, style: ShapeStyle::Filled },
        });
        self.select_element(id, window, cx);
    }

    fn add_text_element(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.builder_next_id;
        self.builder_next_id += 1;
        self.builder_elements.push(BuilderElement {
            id,
            x: 10.0,
            y: 10.0,
            kind: BuilderKind::Text {
                text: "Text".to_string(),
                size: DEFAULT_TEXT_SIZE,
                bold: false,
                font_path: None,
            },
        });
        self.select_element(id, window, cx);
    }

    fn add_image_element(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select an image".into()),
        });
        cx.spawn_in(window, async move |view, window| {
            let Some(path) = paths
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|p| p.into_iter().next())
            else {
                return;
            };
            let aspect = label_render::document::image_aspect_ratio(&path).unwrap_or(1.0);
            window
                .update(|window, cx| {
                    view.update(cx, |this, cx| {
                        let id = this.builder_next_id;
                        this.builder_next_id += 1;
                        let height = 24.0;
                        this.builder_elements.push(BuilderElement {
                            id,
                            x: 10.0,
                            y: 4.0,
                            kind: BuilderKind::Image {
                                path,
                                width: height * aspect,
                                height,
                                aspect,
                            },
                        });
                        this.select_element(id, window, cx);
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    fn toggle_icon_picker(&mut self, cx: &mut Context<Self>) {
        self.icon_picker_open = !self.icon_picker_open;
        cx.notify();
    }

    /// Adds a bundled icon (extracted to a cache file on first use) as an
    /// `Image` element — from there it's identical to any other image:
    /// draggable, resizable, printed through the same SVG rasterization
    /// path as a user-supplied icon.
    fn add_icon_element(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = icons::icon_cache_path(name) else { return };
        let aspect = label_render::document::image_aspect_ratio(&path).unwrap_or(1.0);
        let id = self.builder_next_id;
        self.builder_next_id += 1;
        let height = 24.0;
        self.builder_elements.push(BuilderElement {
            id,
            x: 10.0,
            y: 4.0,
            kind: BuilderKind::Image {
                path,
                width: height * aspect,
                height,
                aspect,
            },
        });
        self.icon_picker_open = false;
        self.select_element(id, window, cx);
    }

    fn remove_selected_element(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.builder_selected.take() else { return };
        self.builder_elements.retain(|e| e.id != id);
        cx.notify();
    }

    fn center_selected_vertically(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.builder_selected else { return };
        let canvas_h = PRINT_HEAD_DOTS as f32 * PREVIEW_SCALE;
        if let Some(el) = self.builder_elements.iter_mut().find(|e| e.id == id) {
            let el_h = match &el.kind {
                BuilderKind::Text { size, .. } => *size * PREVIEW_SCALE,
                BuilderKind::Image { height, .. } => *height * PREVIEW_SCALE,
                BuilderKind::Shape { size, .. } => *size * PREVIEW_SCALE,
            };
            el.y = ((canvas_h - el_h) / 2.0).max(0.0);
        }
        cx.notify();
    }

    fn builder_document(&self) -> LabelDocument {
        let mut doc = LabelDocument::new(self.builder_feed_dots, PRINT_HEAD_DOTS);
        for el in &self.builder_elements {
            let kind = match &el.kind {
                BuilderKind::Text { text, size, bold, font_path } => DocElementKind::Text {
                    lines: text.lines().map(str::to_string).collect(),
                    size: *size,
                    bold: *bold,
                    align: TextAlign::Left,
                    font_path: font_path.clone(),
                },
                BuilderKind::Image { path, width, height, .. } => DocElementKind::Image {
                    path: path.clone(),
                    width: *width,
                    height: *height,
                },
                BuilderKind::Shape { shape, size, style } => DocElementKind::Shape {
                    shape: *shape,
                    size: *size,
                    style: *style,
                },
            };
            doc.elements.push(DocElement {
                id: el.id,
                x: el.x / PREVIEW_SCALE,
                y: el.y / PREVIEW_SCALE,
                kind,
            });
        }
        doc
    }

    fn save_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let path_task = cx.prompt_for_new_path(&dir, Some("label.json"));
        let saved = SavedLabel {
            feed_dots: self.builder_feed_dots,
            elements: self.builder_elements.clone(),
        };
        cx.spawn_in(window, async move |_view, window| {
            let Some(path) = path_task.await.ok().and_then(|r| r.ok()).flatten() else {
                return;
            };
            let result = serde_json::to_string_pretty(&saved)
                .map_err(|e| e.to_string())
                .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()));
            window
                .update(|window, cx| match result {
                    Ok(()) => window.push_notification((NotificationType::Success, "Label saved."), cx),
                    Err(e) => {
                        window.push_notification((NotificationType::Error, format!("Save failed: {e}")), cx)
                    }
                })
                .ok();
        })
        .detach();
    }

    fn load_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open a saved label".into()),
        });
        cx.spawn_in(window, async move |view, window| {
            let Some(path) = paths
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|p| p.into_iter().next())
            else {
                return;
            };
            let loaded = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<SavedLabel>(&s).ok());
            window
                .update(|window, cx| {
                    let Some(saved) = loaded else {
                        window.push_notification((NotificationType::Error, "Could not load that file."), cx);
                        return;
                    };
                    view.update(cx, |this, cx| {
                        let max_id = saved.elements.iter().map(|e| e.id).max().unwrap_or(0);
                        this.builder_elements = saved.elements;
                        this.builder_next_id = max_id + 1;
                        this.builder_selected = None;
                        this.builder_feed_dots = saved.feed_dots;
                        this.builder_feed_input.update(cx, |s, cx| {
                            s.set_value(saved.feed_dots.to_string(), window, cx);
                        });
                        cx.notify();
                    })
                    .ok();
                    window.push_notification((NotificationType::Success, "Label loaded."), cx);
                })
                .ok();
        })
        .detach();
    }

    fn print(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(device_id) = self.device_id.clone() else {
            window.push_notification((NotificationType::Warning, "Pick a printer first."), cx);
            return;
        };
        let Some(job) = self.current_job(cx) else {
            window.push_notification((NotificationType::Warning, "Nothing to print."), cx);
            return;
        };
        let copies = self.copies.max(1);
        self.printing = true;
        cx.notify();

        cx.spawn_in(window, async move |view, window| {
            let result: Result<(), String> = runtime()
                .spawn(async move {
                    let printer = label_device::Printer::connect(&device_id)
                        .await
                        .map_err(|e| e.to_string())?;
                    for _ in 0..copies {
                        printer.print_job(&job).await.map_err(|e| e.to_string())?;
                    }
                    printer.disconnect().await.ok();
                    Ok(())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));

            window
                .update(|window, cx| {
                    view.update(cx, |this, cx| {
                        this.printing = false;
                        cx.notify();
                    })
                    .ok();
                    match result {
                        Ok(()) => window.push_notification(
                            (NotificationType::Success, "Printed."),
                            cx,
                        ),
                        Err(e) => window.push_notification(
                            (NotificationType::Error, format!("Print failed: {e}")),
                            cx,
                        ),
                    }
                })
                .ok();
        })
        .detach();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (compatible, other): (Vec<_>, Vec<_>) = self
            .found
            .iter()
            .partition(|d| d.name.as_deref().is_some_and(is_compatible_printer_name));

        v_flex()
            .w(px(240.))
            .h_full()
            .p_3()
            .gap_3()
            .border_r_1()
            .border_color(gpui::rgb(0xe5e5e5))
            .child(
                Button::new("scan")
                    .primary()
                    .label(if self.scanning { "Scanning..." } else { "Scan for Printers" })
                    .disabled(self.scanning)
                    .on_click(cx.listener(|this, _, window, cx| this.start_scan(window, cx))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .children(compatible.iter().map(|d| self.render_device_row(d, true, cx)))
                    .when(!other.is_empty(), |flex| {
                        flex.child(
                            div()
                                .id("toggle-other-devices")
                                .text_sm()
                                .text_color(gpui::rgb(0x888888))
                                .mt_2()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.other_devices_expanded = !this.other_devices_expanded;
                                    cx.notify();
                                }))
                                .child(format!(
                                    "{} Other devices ({})",
                                    if self.other_devices_expanded { "▾" } else { "▸" },
                                    other.len()
                                )),
                        )
                        .when(self.other_devices_expanded, |flex| {
                            flex.children(other.iter().map(|d| self.render_device_row(d, false, cx)))
                        })
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0x666666))
                    .child(match &self.device_name {
                        Some(n) => format!("Selected: {n}"),
                        None => "No printer selected".into(),
                    }),
            )
    }

    fn render_device_row(
        &self,
        dev: &label_device::DiscoveredPrinter,
        show_icon: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = dev.id.clone();
        let name = dev.name.clone();
        let label = name.clone().unwrap_or_else(|| id.clone());
        let selected = self.device_id.as_deref() == Some(id.as_str());

        h_flex()
            .id(SharedString::from(format!("dev-{}", dev.id)))
            .gap_2()
            .items_center()
            .px_2()
            .py_1()
            .rounded_md()
            .when(selected, |d| d.bg(gpui::rgb(0xdbeafe)))
            .on_click(cx.listener(move |this, _, _, cx| this.select_device(id.clone(), name.clone(), cx)))
            .when(show_icon, |d| {
                d.child(gpui_kit::img(dymo_icon_image()).w(px(28.)).h(px(28.)).object_fit(ObjectFit::Contain))
            })
            .child(div().text_sm().child(label))
    }

    fn render_builder_element(&self, el: &BuilderElement, cx: &mut Context<Self>) -> impl IntoElement {
        let id = el.id;
        let (ex, ey) = (el.x, el.y);
        let selected = self.builder_selected == Some(id);

        let content: AnyElement = match &el.kind {
            BuilderKind::Text { text, size, bold, font_path } => div()
                .text_size(px(*size * PREVIEW_SCALE))
                // Tight, 1:1 line height: GPUI's default leading made the
                // element's bounding box noticeably taller than the glyphs
                // themselves, which made vertically centering text in the
                // (very short) cross-feed direction guesswork.
                .line_height(px(*size * PREVIEW_SCALE))
                .h(px(*size * PREVIEW_SCALE))
                .when(*bold, |d| d.font_weight(gpui::FontWeight::BOLD))
                .when_some(
                    font_path.as_ref().and_then(|p| {
                        self.builder_font_choices.iter().find(|f| &f.path == p)
                    }),
                    |d, f| d.font_family(f.family.clone()),
                )
                .child(if text.is_empty() {
                    " ".to_string()
                } else {
                    text.clone()
                })
                .into_any_element(),
            BuilderKind::Image { path, width, height, .. } => div()
                .w(px(*width * PREVIEW_SCALE))
                .h(px(*height * PREVIEW_SCALE))
                .child(
                    gpui_kit::img(path.clone())
                        .w_full()
                        .h_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            BuilderKind::Shape { shape, size, style } => {
                let s = *size * PREVIEW_SCALE;
                let filled = *style == ShapeStyle::Filled;
                let dotted = *style == ShapeStyle::Dotted;
                match shape {
                    ShapeKind::Circle => div()
                        .w(px(s))
                        .h(px(s))
                        .rounded_full()
                        .when(filled, |d| d.bg(gpui::black()))
                        .when(!filled, |d| {
                            d.border_2().border_color(gpui::black()).when(dotted, |d| d.border_dashed())
                        })
                        .into_any_element(),
                    ShapeKind::Square => div()
                        .w(px(s))
                        .h(px(s))
                        .when(filled, |d| d.bg(gpui::black()))
                        .when(!filled, |d| {
                            d.border_2().border_color(gpui::black()).when(dotted, |d| d.border_dashed())
                        })
                        .into_any_element(),
                    ShapeKind::Triangle => div()
                        .w(px(s))
                        .h(px(s))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(s))
                        .line_height(px(s))
                        .child(match style {
                            ShapeStyle::Filled => "▲",
                            ShapeStyle::Outline => "△",
                            ShapeStyle::Dotted => "△",
                        })
                        .into_any_element(),
                }
            }
        };

        div()
            .id(("builder-el", id))
            .absolute()
            .left(px(el.x))
            .top(px(el.y))
            .when(selected, |d| {
                d.border_1().border_color(gpui::rgb(0x3b82f6)).p(px(1.))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_element(id, window, cx);
            }))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(move |this, event: &gpui_kit::MouseDownEvent, _window, _cx| {
                    let origin = this.canvas_origin.get();
                    let local_x = f32::from(event.position.x - origin.x);
                    let local_y = f32::from(event.position.y - origin.y);
                    this.drag_grab = Some((id, local_x - ex, local_y - ey));
                }),
            )
            .child(content)
    }

    /// A visually distinct card grouping a few related toolbar controls,
    /// each wrapped in its own small bordered chip — without this, e.g.
    /// the length input and the "dots" label next to it read as one
    /// control instead of two.
    fn toolbar_group(&self, children: impl IntoIterator<Item = AnyElement>) -> impl IntoElement {
        h_flex().gap_1().items_center().children(children.into_iter().map(|child| {
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(gpui::rgb(0xe0e0e0))
                .bg(gpui::rgb(0xf7f7f7))
                .child(child)
        }))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let insert_group = self.toolbar_group([h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("add-text")
                    .label("T  Text")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.add_text_element(window, cx))),
            )
            .child(
                Button::new("add-image")
                    .label("🖼 Image")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.add_image_element(window, cx))),
            )
            .child(Button::new("add-circle").label("○").small().on_click(cx.listener(
                |this, _, window, cx| this.add_shape_element(ShapeKind::Circle, window, cx),
            )))
            .child(Button::new("add-square").label("□").small().on_click(cx.listener(
                |this, _, window, cx| this.add_shape_element(ShapeKind::Square, window, cx),
            )))
            .child(Button::new("add-triangle").label("△").small().on_click(cx.listener(
                |this, _, window, cx| this.add_shape_element(ShapeKind::Triangle, window, cx),
            )))
            .child(
                Button::new("add-icon")
                    .label("✦ Icon")
                    .small()
                    .when(self.icon_picker_open, |b| b.primary())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_icon_picker(cx))),
            )
            .child(
                Button::new("remove")
                    .label("🗑")
                    .small()
                    .disabled(self.builder_selected.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.remove_selected_element(cx))),
            )
            .into_any_element()]);

        let document_group = self.toolbar_group([
            h_flex()
                .gap_2()
                .items_center()
                .child(div().text_sm().child("Length"))
                .child(
                    div()
                        .w(px(70.))
                        .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "up" => this.step_feed_dots(1, window, cx),
                                "down" => this.step_feed_dots(-1, window, cx),
                                _ => {}
                            }
                        }))
                        .child(Input::new(&self.builder_feed_input)),
                )
                .child(div().text_sm().text_color(gpui::rgb(0x666666)).ml_1().child("dots"))
                .into_any_element(),
            h_flex()
                .gap_1()
                .items_center()
                .child(Switch::new("invert").checked(self.invert).on_change(cx.listener(
                    |this, checked, _, cx| {
                        this.invert = *checked;
                        cx.notify();
                    },
                )))
                .child(div().text_sm().child("Invert"))
                .into_any_element(),
            h_flex()
                .gap_2()
                .items_center()
                .child(div().text_sm().child("Contrast"))
                .child(div().w(px(100.)).child(Slider::new(&self.contrast)))
                .into_any_element(),
        ]);

        let file_group = self.toolbar_group([h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("save")
                    .label("💾 Save")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.save_label(window, cx))),
            )
            .child(
                Button::new("open")
                    .label("📂 Open")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.load_label(window, cx))),
            )
            .into_any_element()]);

        v_flex()
            .gap_2()
            .p_3()
            .bg(gpui::rgb(0xfafafa))
            .border_b_1()
            .border_color(gpui::rgb(0xe5e5e5))
            .child(h_flex().gap_3().items_center().child(insert_group).child(document_group).child(file_group))
            .child(
                div().text_sm().text_color(gpui::rgb(0x999999)).child(format!(
                    "Editor is zoomed in {PREVIEW_SCALE:.0}x — printable area is about {:.0}mm long x {:.0}mm tall, plus a ~10mm blank leader/trailer the printer adds to every job",
                    self.builder_feed_dots as f32 / CROSS_DOTS_PER_MM,
                    PRINT_HEAD_DOTS as f32 / CROSS_DOTS_PER_MM,
                )),
            )
    }

    fn render_icon_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.icon_picker_open {
            return None;
        }
        let query = self.icon_search.read(cx).value().to_lowercase();
        let matches: Vec<String> = icons::icon_names()
            .into_iter()
            .filter(|n| query.is_empty() || n.contains(&query))
            .take(60)
            .collect();

        Some(
            v_flex()
                .gap_2()
                .p_3()
                .bg(gpui::rgb(0xfafaff))
                .border_b_1()
                .border_color(gpui::rgb(0xe0e5f5))
                .child(div().w(px(240.)).child(Input::new(&self.icon_search).cleanable(true)))
                .child(
                    h_flex().flex_wrap().gap_1().children(matches.into_iter().map(|name| {
                        let label = name.clone();
                        Button::new(SharedString::from(format!("icon-{name}")))
                            .label(label)
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_icon_element(&name, window, cx)
                            }))
                    })),
                ),
        )
    }

    fn render_property_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let el = self.selected_element()?.clone();
        Some(
            h_flex()
                .gap_4()
                .items_center()
                .px_3()
                .py_2()
                .bg(gpui::rgb(0xeef2ff))
                .border_b_1()
                .border_color(gpui::rgb(0xe0e5f5))
                .when(matches!(el.kind, BuilderKind::Text { .. }), |row| {
                    row.child(div().w(px(200.)).child(Input::new(&self.builder_text_edit)))
                        .child(div().w(px(160.)).child(Select::new(&self.builder_font_select).placeholder("Font")))
                        .child(
                            Button::new("refresh-fonts")
                                .label("Refresh fonts")
                                .xsmall()
                                .on_click(cx.listener(|this, _, window, cx| this.refresh_fonts(window, cx))),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Switch::new("builder-bold").checked(self.builder_bold).on_change(
                                    cx.listener(|this, _, _, cx| this.toggle_selected_bold(cx)),
                                ))
                                .child(div().text_sm().child("Bold")),
                        )
                })
                .when(matches!(el.kind, BuilderKind::Shape { .. }), |row| {
                    let style = self.builder_shape_style;
                    row.child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                Button::new("style-filled")
                                    .label("Filled")
                                    .xsmall()
                                    .when(style == ShapeStyle::Filled, |b| b.primary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_selected_shape_style(ShapeStyle::Filled, cx)
                                    })),
                            )
                            .child(
                                Button::new("style-outline")
                                    .label("Outline")
                                    .xsmall()
                                    .when(style == ShapeStyle::Outline, |b| b.primary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_selected_shape_style(ShapeStyle::Outline, cx)
                                    })),
                            )
                            .child(
                                Button::new("style-dotted")
                                    .label("Dotted")
                                    .xsmall()
                                    .when(style == ShapeStyle::Dotted, |b| b.primary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_selected_shape_style(ShapeStyle::Dotted, cx)
                                    })),
                            ),
                    )
                })
                .child(div().text_sm().child("Size"))
                .child(div().w(px(120.)).child(Slider::new(&self.builder_size_slider)))
                .child(
                    Button::new("center-v")
                        .label("Center Vertically")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.center_selected_vertically(cx))),
                ),
        )
    }

    fn render_canvas(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let canvas_w = self.builder_feed_dots as f32 * PREVIEW_SCALE;
        let canvas_h = PRINT_HEAD_DOTS as f32 * PREVIEW_SCALE;
        let origin_cell = self.canvas_origin.clone();

        // Previous attempts at `overflow_x_scrollbar()` never scrolled —
        // but those nested it inside a separate `overflow_hidden()`
        // wrapper, which fights the scroll area it manages itself. Per
        // `ScrollableElement`'s own doc ("the wrapped element is the
        // scroll area itself, rather than being inserted as a child of a
        // new scroll area"), it has to be called directly on this div,
        // which also needs a stable `.id()` for its scroll position to
        // persist across renders.
        div()
            .id("builder-canvas-scroll")
            .flex_1()
            .w_full()
            .p_6()
            .bg(gpui::rgb(0xf5f5f7))
            .overflow_x_scrollbar()
            .child(
                div()
                    .id("builder-canvas")
                    .relative()
                    .w(px(canvas_w))
                    .h(px(canvas_h))
                    .flex_shrink_0()
                    .bg(gpui::white())
                    .border_2()
                    .border_color(gpui::rgb(0xb0b0b0))
                    .shadow_md()
                    .on_mouse_move(cx.listener(
                        |this, event: &gpui_kit::MouseMoveEvent, _window, cx| {
                            let Some((id, dx, dy)) = this.drag_grab else {
                                return;
                            };
                            let origin = this.canvas_origin.get();
                            let local_x = f32::from(event.position.x - origin.x);
                            let local_y = f32::from(event.position.y - origin.y);
                            let canvas_h = PRINT_HEAD_DOTS as f32 * PREVIEW_SCALE;
                            if let Some(el) = this.builder_elements.iter_mut().find(|e| e.id == id) {
                                let el_h = match &el.kind {
                                    BuilderKind::Text { size, .. } => *size * PREVIEW_SCALE,
                                    BuilderKind::Image { height, .. } => *height * PREVIEW_SCALE,
                                    BuilderKind::Shape { size, .. } => *size * PREVIEW_SCALE,
                                };
                                // Only the element's own midpoint is
                                // kept on the tape; it can overhang
                                // the top/bottom edge freely, same
                                // as oversizing it is the user's call.
                                el.x = (local_x - dx).max(0.0);
                                el.y = (local_y - dy).clamp(-el_h / 2.0, canvas_h - el_h / 2.0);
                            }
                            cx.notify();
                        },
                    ))
                    .on_mouse_up(
                        gpui_kit::MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.drag_grab = None;
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        gpui_kit::MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.drag_grab = None;
                            cx.notify();
                        }),
                    )
                    .child(gpui_kit::canvas(
                        move |bounds, _window, _cx| {
                            origin_cell.set(bounds.origin);
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full())
                    .children(
                        self.builder_elements
                            .iter()
                            .map(|el| self.render_builder_element(el, cx).into_any_element()),
                    ),
            )
    }

    /// Refreshes `preview_cache` from the current document, but no more
    /// often than every `THROTTLE` and never while actively dragging an
    /// element — recomputing the full raster (font shaping, resize,
    /// dithering) on every single render made typing and dragging
    /// visibly laggy. When a change arrives inside the throttle window,
    /// a single catch-up redraw is scheduled just past it so the final
    /// state always lands, instead of staying stale until some other
    /// edit happens to fall outside the window.
    fn maybe_recompute_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const THROTTLE: std::time::Duration = std::time::Duration::from_millis(400);
        if self.drag_grab.is_some() {
            return;
        }
        let elapsed = self.last_preview_compute.elapsed();
        if elapsed < THROTTLE {
            if !self.preview_catchup_scheduled {
                self.preview_catchup_scheduled = true;
                let remaining = THROTTLE - elapsed + std::time::Duration::from_millis(20);
                cx.spawn_in(window, async move |view, window| {
                    futures_timer::Delay::new(remaining).await;
                    window
                        .update(|_, cx| {
                            view.update(cx, |this, cx| {
                                this.preview_catchup_scheduled = false;
                                cx.notify();
                            })
                            .ok();
                        })
                        .ok();
                })
                .detach();
            }
            return;
        }
        self.last_preview_compute = std::time::Instant::now();

        if self.builder_elements.is_empty() {
            self.preview_cache = None;
            return;
        }
        let render_opts = self.render_opts(cx);
        let doc = self.builder_document();
        let Ok(raster) = raster_from_document(&doc, &render_opts) else {
            self.preview_cache = None;
            return;
        };
        self.preview_cache = label_render::physical_preview_png(&raster, &label_render::TapeProfile::default(), 14.0)
            .ok()
            .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)));
    }

    /// Renders the actual composited-and-dithered label as it will
    /// physically look on tape: true proportions, the printable band
    /// centered in the full tape height, and the blank leader/trailer the
    /// printer adds to every job. Reads from `preview_cache`; call
    /// `maybe_recompute_preview` first to keep it current.
    fn render_dither_preview(&self, _cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let png = self.preview_cache.clone()?;

        Some(
            v_flex()
                .gap_2()
                .p_3()
                .items_start()
                .border_t_1()
                .border_color(gpui::rgb(0xe5e5e5))
                .child(div().text_sm().text_color(gpui::rgb(0x888888)).child(
                    "Approximate print preview",
                ))
                .child(
                    // `items_start()` above keeps this at the image's own
                    // size instead of flexbox's default of stretching a
                    // column's children to the full container width,
                    // which turned the border into one big gray block
                    // regardless of how short the actual label is.
                    div()
                        .border_1()
                        .border_color(gpui::rgb(0x999999))
                        .border_dashed()
                        .child(gpui_kit::img(png)),
                ),
        )
    }
}

impl Render for LabelApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.maybe_recompute_preview(window, cx);
        self.ensure_fonts_loaded(cx);
        h_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _window, cx| {
                // A focused text input consumes backspace/delete itself
                // (and stops propagation), so this only fires when
                // nothing is being typed into.
                if matches!(event.keystroke.key.as_str(), "backspace" | "delete") {
                    this.remove_selected_element(cx);
                }
            }))
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    // Without this, this column refuses to shrink below
                    // the canvas's unclipped intrinsic width (a flex item
                    // only drops that minimum when its own overflow isn't
                    // Visible) — which silences the canvas's horizontal
                    // scrollbar entirely, since nothing downstream of it
                    // ever actually overflows its allocated space.
                    .min_w_0()
                    .h_full()
                    .child(self.render_toolbar(cx))
                    .children(self.render_icon_picker(cx))
                    .children(self.render_property_panel(cx))
                    .child(self.render_canvas(cx))
                    .children(self.render_dither_preview(cx))
                    .child(
                        h_flex()
                            .p_3()
                            .gap_3()
                            .items_center()
                            .justify_end()
                            .border_t_1()
                            .border_color(gpui::rgb(0xe5e5e5))
                            .child(
                                Button::new("minus")
                                    .label("-")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copies = this.copies.saturating_sub(1).max(1);
                                        cx.notify();
                                    })),
                            )
                            .child(div().w(px(30.)).child(format!("{}", self.copies)))
                            .child(
                                Button::new("plus")
                                    .label("+")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copies += 1;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("print")
                                    .primary()
                                    .label(if self.printing { "Printing..." } else { "Print" })
                                    .disabled(self.printing)
                                    .on_click(cx.listener(|this, _, window, cx| this.print(window, cx))),
                            ),
                    ),
            )
    }
}

fn main() {
    application().with_assets(assets::Assets).run(|cx| {
        gpui_kit::init(cx);
        // Default scrollbar behavior only draws the thumb on hover/scroll;
        // the canvas's horizontal scrollbar should stay visible once the
        // label is wider than the viewport so its existence (and current
        // position) is obvious without having to interact first.
        Theme::set_scrollbar_mode(gpui_kit::component::scroll::ScrollbarMode::Always, cx);
        open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| LabelApp::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
