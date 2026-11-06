use std::path::PathBuf;
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

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::notification::NotificationType;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::select::SelectEvent;
use gpui_kit::component::WindowExt;
use gpui_kit::component::*;
use gpui_kit::base::Disableable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{Image, ImageFormat, PathPromptOptions};
use gpui_kit::*;
use label_core::create_job;
use label_render::{
    raster_from_dynamic_image, raster_from_text, Dither, RenderOptions, TextAlign, TextOptions,
};

const DITHER_OPTIONS: [&str; 3] = ["Threshold", "Floyd-Steinberg", "Atkinson"];
const ALIGN_OPTIONS: [&str; 3] = ["Left", "Center", "Right"];

fn dither_from_label(label: &str) -> Dither {
    match label {
        "Floyd-Steinberg" => Dither::FloydSteinberg,
        "Atkinson" => Dither::Atkinson,
        _ => Dither::Threshold(128),
    }
}

fn align_from_label(label: &str) -> TextAlign {
    match label {
        "Center" => TextAlign::Center,
        "Right" => TextAlign::Right,
        _ => TextAlign::Left,
    }
}

struct LabelApp {
    // device
    scanning: bool,
    found: Vec<label_device::DiscoveredPrinter>,
    device_id: Option<String>,
    device_name: Option<String>,

    // tabs
    active_tab: usize,

    // text options
    text_input: Entity<InputState>,
    bold: bool,
    frame: bool,
    text_align: Entity<SelectState<Vec<String>>>,
    font_size: Entity<SliderState>,

    // image options
    image_path: Option<PathBuf>,
    contrast: Entity<SliderState>,

    // shared
    dither: Entity<SelectState<Vec<String>>>,
    invert: bool,
    copies: u32,

    // preview
    preview_png: Option<Arc<Image>>,
    feed_dots: u32,

    printing: bool,

    _subscriptions: Vec<Subscription>,
}

impl LabelApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Type your label text...")
                .default_value("Hello World")
        });
        let text_align = cx.new(|cx| {
            SelectState::new(
                ALIGN_OPTIONS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let dither = cx.new(|cx| {
            SelectState::new(
                DITHER_OPTIONS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let font_size = cx.new(|_| {
            SliderState::new().min(16.0).max(160.0).default_value(64.0).step(1.0)
        });
        let contrast = cx.new(|_| {
            SliderState::new().min(-100.0).max(100.0).default_value(0.0).step(1.0)
        });

        let mut subs = Vec::new();
        subs.push(cx.subscribe(&text_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.rebuild_preview(cx);
            }
        }));
        subs.push(cx.subscribe(&text_align, |this, _, _event: &SelectEvent<Vec<String>>, cx| {
            this.rebuild_preview(cx);
        }));
        subs.push(cx.subscribe(&dither, |this, _, _event: &SelectEvent<Vec<String>>, cx| {
            this.rebuild_preview(cx);
        }));
        subs.push(cx.subscribe(&font_size, |this, _, event, cx| {
            if let SliderEvent::Change(_) = event {
                this.rebuild_preview(cx);
            }
        }));
        subs.push(cx.subscribe(&contrast, |this, _, event, cx| {
            if let SliderEvent::Change(_) = event {
                this.rebuild_preview(cx);
            }
        }));

        let mut app = Self {
            scanning: false,
            found: Vec::new(),
            device_id: None,
            device_name: None,
            active_tab: 0,
            text_input,
            bold: false,
            frame: false,
            text_align,
            font_size,
            image_path: None,
            contrast,
            dither,
            invert: false,
            copies: 1,
            preview_png: None,
            feed_dots: 0,
            printing: false,
            _subscriptions: subs,
        };
        app.rebuild_preview(cx);
        app
    }

    fn render_opts(&self, cx: &App) -> RenderOptions {
        let dither_label = self
            .dither
            .read(cx)
            .selected_value()
            .cloned()
            .unwrap_or_else(|| "Threshold".to_string());
        RenderOptions {
            dither: dither_from_label(&dither_label),
            invert: self.invert,
            contrast: self.contrast.read(cx).value().start(),
            ..Default::default()
        }
    }

    fn rebuild_preview(&mut self, cx: &mut Context<Self>) {
        let render_opts = self.render_opts(cx);
        let raster = if self.active_tab == 0 {
            let text = self.text_input.read(cx).value().to_string();
            if text.trim().is_empty() {
                self.preview_png = None;
                self.feed_dots = 0;
                cx.notify();
                return;
            }
            let align_label = self
                .text_align
                .read(cx)
                .selected_value()
                .cloned()
                .unwrap_or_else(|| "Left".to_string());
            let text_opts = TextOptions {
                font_path: None,
                bold: self.bold,
                size: self.font_size.read(cx).value().start(),
                align: align_from_label(&align_label),
                frame: self.frame,
            };
            let lines: Vec<&str> = text.lines().collect();
            match raster_from_text(&lines, &text_opts, &render_opts) {
                Ok(r) => r,
                Err(_) => {
                    self.preview_png = None;
                    cx.notify();
                    return;
                }
            }
        } else {
            let Some(path) = &self.image_path else {
                self.preview_png = None;
                self.feed_dots = 0;
                cx.notify();
                return;
            };
            match image::open(path) {
                Ok(img) => raster_from_dynamic_image(&img, &render_opts),
                Err(_) => {
                    self.preview_png = None;
                    cx.notify();
                    return;
                }
            }
        };

        self.feed_dots = raster.feed;
        if let Ok(png) = label_render::preview_png(&raster, 4) {
            self.preview_png = Some(Arc::new(Image::from_bytes(ImageFormat::Png, png)));
        }
        cx.notify();
    }

    fn current_job(&self, cx: &App) -> Option<Vec<Vec<u8>>> {
        let render_opts = self.render_opts(cx);
        let raster = if self.active_tab == 0 {
            let text = self.text_input.read(cx).value().to_string();
            if text.trim().is_empty() {
                return None;
            }
            let align_label = self.text_align.read(cx).selected_value().cloned().unwrap_or_default();
            let text_opts = TextOptions {
                font_path: None,
                bold: self.bold,
                size: self.font_size.read(cx).value().start(),
                align: align_from_label(&align_label),
                frame: self.frame,
            };
            let lines: Vec<&str> = text.lines().collect();
            raster_from_text(&lines, &text_opts, &render_opts).ok()?
        } else {
            let path = self.image_path.as_ref()?;
            let img = image::open(path).ok()?;
            raster_from_dynamic_image(&img, &render_opts)
        };
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

    fn pick_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            window
                .update(|_, cx| {
                    view.update(cx, |this, cx| {
                        this.image_path = Some(path);
                        this.rebuild_preview(cx);
                    })
                    .ok();
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
                    .children(self.found.iter().map(|d| {
                        let id = d.id.clone();
                        let name = d.name.clone();
                        let label = name.clone().unwrap_or_else(|| id.clone());
                        let selected = self.device_id.as_deref() == Some(id.as_str());
                        Button::new(SharedString::from(id.clone()))
                            .label(label)
                            .when(selected, |b| b.primary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_device(id.clone(), name.clone(), cx)
                            }))
                    })),
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

    fn render_text_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(Input::new(&self.text_input).cleanable(true))
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(div().text_sm().child("Size"))
                    .child(div().w(px(160.)).child(Slider::new(&self.font_size)))
                    .child(div().text_sm().child(format!("{:.0}px", self.font_size.read(cx).value().start())))
                    .child(
                        div().w(px(140.)).child(
                            Select::new(&self.text_align).placeholder("Align"),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Switch::new("bold").checked(self.bold).on_change(cx.listener(
                                |this, checked, _, cx| {
                                    this.bold = *checked;
                                    this.rebuild_preview(cx);
                                },
                            )))
                            .child("Bold"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Switch::new("frame").checked(self.frame).on_change(cx.listener(
                                |this, checked, _, cx| {
                                    this.frame = *checked;
                                    this.rebuild_preview(cx);
                                },
                            )))
                            .child("Frame"),
                    ),
            )
    }

    fn render_image_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(Button::new("load").label("Load Image...").on_click(cx.listener(
                        |this, _, window, cx| this.pick_image(window, cx),
                    )))
                    .child(div().text_sm().child(
                        self.image_path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "No image loaded".into()),
                    )),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(div().text_sm().child("Contrast"))
                    .child(div().w(px(160.)).child(Slider::new(&self.contrast)))
                    .child(div().text_sm().child(format!("{:.0}", self.contrast.read(cx).value().start()))),
            )
    }

    fn render_shared_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_4()
            .items_center()
            .child(div().text_sm().child("Dither"))
            .child(div().w(px(160.)).child(Select::new(&self.dither)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Switch::new("invert").checked(self.invert).on_change(cx.listener(
                        |this, checked, _, cx| {
                            this.invert = *checked;
                            this.rebuild_preview(cx);
                        },
                    )))
                    .child("Invert"),
            )
    }

    fn render_preview(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let mm = self.feed_dots as f32 / 8.0; // rough estimate; printer's exact feed DPI is unverified
        v_flex()
            .flex_1()
            .h_full()
            .p_4()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(gpui::rgb(0xfafafa))
            .child(match &self.preview_png {
                Some(img) => div()
                    .border_1()
                    .border_color(gpui::rgb(0xdddddd))
                    .bg(gpui::white())
                    .child(
                        gpui_kit::img(img.clone())
                            .object_fit(ObjectFit::Contain)
                            .max_h(px(400.)),
                    )
                    .into_any_element(),
                None => div().text_color(gpui::rgb(0x999999)).child("Nothing to preview").into_any_element(),
            })
            .child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0x888888))
                    .child(format!("~{:.0} dots long (~{mm:.0}mm, estimated)", self.feed_dots)),
            )
    }
}

impl Render for LabelApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .child(
                        TabBar::new("tabs")
                            .selected_index(self.active_tab)
                            .on_click(cx.listener(|this, index, _, cx| {
                                this.active_tab = *index;
                                this.rebuild_preview(cx);
                            }))
                            .child(Tab::new().label("Text"))
                            .child(Tab::new().label("Image")),
                    )
                    .child(
                        v_flex()
                            .p_4()
                            .gap_3()
                            .child(if self.active_tab == 0 {
                                self.render_text_tab(cx).into_any_element()
                            } else {
                                self.render_image_tab(cx).into_any_element()
                            })
                            .child(self.render_shared_controls(cx)),
                    )
                    .child(self.render_preview(cx))
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
        open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| LabelApp::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
