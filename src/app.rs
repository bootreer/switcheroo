use eframe::egui::{self, TextFormat, text::LayoutJob};
use egui::{Layout, TextureHandle, UiBuilder};
use nucleo_matcher::{Config, Matcher, Utf32String};
use std::collections::HashMap;

use crate::macos;

pub struct App {
    query: String,
    windows: HashMap<i32, macos::App>,
    selected: Option<usize>,
    matcher: Matcher,
    max_width: usize,
    icon_cache: HashMap<i32, TextureHandle>,
}

impl App {
    pub fn new(windows: HashMap<i32, macos::App>) -> Self {
        let max_width = windows
            .values()
            .map(|app| app.name.len())
            .max()
            .unwrap_or(10);

        Self {
            windows,
            max_width,
            ..Default::default()
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self {
            query: String::new(),
            windows: HashMap::new(),
            selected: None,
            matcher: Matcher::new(Config::DEFAULT),
            max_width: 0,
            icon_cache: HashMap::new(),
        }
    }
}

impl eframe::App for App {
    // TODO: clean up AI slop
    // NOTE: egui can't render emojis
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let frame = egui::Frame::default()
            .fill(egui::Color32::from_black_alpha(150))
            .corner_radius(10.0)
            .inner_margin(20.0);

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            // Consume arrow keys so text edit doesn't see them
            let arrow_down =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
            let arrow_up =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
            let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
            let escape_pressed = ctx.input(|i| i.key_pressed(egui::Key::Escape));

            ui.vertical(|ui| {
                let text_edit = egui::TextEdit::singleline(&mut self.query)
                    .hint_text("windows")
                    .desired_width(ui.available_width())
                    .font(egui::TextStyle::Heading);

                let response = ui.add(text_edit);
                response.request_focus();

                ui.add_space(10.0);

                let mut filtered_items: Vec<(i32, &macos::App, &macos::Window, u32, Vec<u32>)> =
                    if self.query.is_empty() {
                        self.windows
                            .iter()
                            .flat_map(|(pid, app)| {
                                app.windows
                                    .iter()
                                    .map(move |win| (*pid, app, win, 0, vec![]))
                            })
                            .collect()
                    } else {
                        let items: Vec<_> = self
                            .windows
                            .iter()
                            .flat_map(|(pid, app)| {
                                app.windows.iter().map(move |win| {
                                    let search_text = format!("{} {}", app.name, win.title);
                                    (*pid, app, win, search_text)
                                })
                            })
                            .collect();

                        let needle = Utf32String::from(self.query.as_str());
                        items
                            .into_iter()
                            .filter_map(|(pid, app, win, search_text)| {
                                let haystack = Utf32String::from(search_text.as_str());
                                let mut indices = Vec::new();
                                self.matcher
                                    .fuzzy_indices(
                                        haystack.slice(..),
                                        needle.slice(..),
                                        &mut indices,
                                    )
                                    .map(|score| (pid, app, win, score as u32, indices.clone()))
                            })
                            .collect()
                    };

                filtered_items.sort_by(|a, b| {
                    b.3.cmp(&a.3)
                        .then_with(|| a.1.name.cmp(&b.1.name))
                        .then_with(|| a.2.title.cmp(&b.2.title))
                });

                if let Some(idx) = self.selected
                    && idx >= filtered_items.len()
                {
                    self.selected = None;
                }

                if escape_pressed {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }

                if arrow_down && !filtered_items.is_empty() {
                    self.selected = Some(match self.selected {
                        Some(idx) => (idx + 1).min(filtered_items.len() - 1),
                        None => 0,
                    });
                }

                if arrow_up && let Some(idx) = self.selected {
                    self.selected = if idx > 0 { Some(idx - 1) } else { None };
                }

                if enter_pressed
                    && let Some(idx) = self.selected
                    && let Some(&(_, app, window, _, _)) = filtered_items.get(idx)
                {
                    let _ = window.focus(&app.app);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }

                // Display filtered windows
                let icon_size = 24.0;
                let app_name_width = self.max_width as f32 * 8.0;

                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        for (idx, (_pid, app, window, _, indices)) in
                            filtered_items.into_iter().enumerate()
                        {
                            let is_selected = self.selected == Some(idx);
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), icon_size + 8.0),
                                egui::Sense::click(),
                            );

                            if is_selected {
                                ui.painter().rect_filled(
                                    rect,
                                    4.0,
                                    egui::Color32::from_rgb(70, 130, 200),
                                );
                            }

                            let mut content_ui = ui.new_child(
                                UiBuilder::new()
                                    .max_rect(rect)
                                    .layout(Layout::left_to_right(egui::Align::Center)),
                            );

                            content_ui.spacing_mut().item_spacing.x = 8.0;

                            if let Some(icon_image) = &app.icon {
                                let texture = self.icon_cache.entry(app.pid).or_insert_with(|| {
                                    ctx.load_texture(
                                        format!("icon_{}", app.pid),
                                        icon_image.clone(),
                                        egui::TextureOptions::LINEAR,
                                    )
                                });
                                content_ui.image((texture.id(), egui::vec2(icon_size, icon_size)));
                            } else {
                                content_ui.allocate_space(egui::vec2(icon_size, icon_size));
                            }

                            // App name column
                            content_ui.allocate_ui_with_layout(
                                egui::vec2(app_name_width, icon_size),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_width(app_name_width);
                                    let app_name_widget: egui::WidgetText = if indices.is_empty() {
                                        let text = egui::RichText::new(&app.name);
                                        if is_selected {
                                            text.color(egui::Color32::WHITE).into()
                                        } else {
                                            text.into()
                                        }
                                    } else {
                                        // Highlight app name
                                        let indices_set: std::collections::HashSet<_> =
                                            indices.iter().map(|&i| i as usize).collect();
                                        let mut job = LayoutJob::default();
                                        let (normal_color, highlight_color) = if is_selected {
                                            (
                                                egui::Color32::WHITE,
                                                egui::Color32::from_rgb(255, 255, 150),
                                            )
                                        } else {
                                            (
                                                ui.style().visuals.text_color(),
                                                egui::Color32::from_rgb(100, 200, 255),
                                            )
                                        };

                                        for (i, ch) in app.name.chars().enumerate() {
                                            let color = if indices_set.contains(&i) {
                                                highlight_color
                                            } else {
                                                normal_color
                                            };

                                            job.append(
                                                &ch.to_string(),
                                                0.0,
                                                TextFormat {
                                                    color,
                                                    ..Default::default()
                                                },
                                            );
                                        }
                                        job.into()
                                    };
                                    ui.label(app_name_widget);
                                },
                            );

                            // Window title column
                            content_ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_width(ui.available_width());

                                    let window_title_widget: egui::WidgetText =
                                        if indices.is_empty() {
                                            let text = egui::RichText::new(&window.title);
                                            if is_selected {
                                                text.color(egui::Color32::WHITE).into()
                                            } else {
                                                text.into()
                                            }
                                        } else {
                                            let offset = app.name.len() + 1;
                                            let indices_set: std::collections::HashSet<_> =
                                                indices.iter().map(|&i| i as usize).collect();
                                            let mut job = LayoutJob::default();
                                            let (normal_color, highlight_color) = if is_selected {
                                                (
                                                    egui::Color32::WHITE,
                                                    egui::Color32::from_rgb(255, 255, 150),
                                                )
                                            } else {
                                                (
                                                    ui.style().visuals.text_color(),
                                                    egui::Color32::from_rgb(100, 200, 255),
                                                )
                                            };

                                            for (i, ch) in window.title.chars().enumerate() {
                                                let adjusted_idx = i + offset;
                                                let color = if indices_set.contains(&adjusted_idx) {
                                                    highlight_color
                                                } else {
                                                    normal_color
                                                };
                                                job.append(
                                                    &ch.to_string(),
                                                    0.0,
                                                    TextFormat {
                                                        color,
                                                        ..Default::default()
                                                    },
                                                );
                                            }
                                            job.into()
                                        };
                                    ui.add(egui::Label::new(window_title_widget).truncate());
                                },
                            );

                            if response.clicked() {
                                self.selected = Some(idx);
                            }
                        }
                    });
            });
        });
    }
}
