use std::collections::HashSet;

use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{
    center, column, container, image, rich_text, row, scrollable, span, text_input,
};
use iced::window;
use iced::{Element, Length, Subscription, Task, Theme, color};
use nucleo_matcher::{Config, Matcher, Utf32String};

use crate::windows;

const SEARCH_INPUT_ID: &str = "search_input";

#[derive(Debug, Clone)]
pub enum Message {
    ShowPicker,
    HidePicker,
    QueryChanged(String),
    SelectNext,
    SelectPrev,
    Confirm,
    WindowClosed(window::Id),
    NoOp,
}

pub struct Switcheroo {
    query: String,
    selected: Option<usize>,
    filtered_count: usize,
    manager: windows::Manager,
    picker_window: Option<window::Id>,
}

pub fn boot() -> (Switcheroo, Task<Message>) {
    (
        Switcheroo {
            query: String::new(),
            selected: None,
            filtered_count: 0,
            manager: windows::Manager::new().unwrap_or_default(),
            picker_window: None,
        },
        Task::none(),
    )
}

pub fn title(_state: &Switcheroo, _window: window::Id) -> String {
    String::from("switcheroo")
}

pub fn update(state: &mut Switcheroo, message: Message) -> Task<Message> {
    match message {
        Message::ShowPicker => {
            if state.picker_window.is_some() {
                return Task::none();
            }

            crate::macos::activate_application();

            if let Err(e) = state.manager.refresh() {
                eprintln!("Failed to refresh windows: {e}");
            }
            state.query.clear();
            state.filtered_count = get_filtered_items(state).len();
            state.selected = if state.filtered_count > 0 {
                Some(0)
            } else {
                None
            };

            let (id, open_task) = window::open(window::Settings {
                size: iced::Size::new(800.0, 400.0),
                position: window::Position::Centered,
                decorations: false,
                transparent: true,
                level: window::Level::AlwaysOnTop,
                exit_on_close_request: false,
                ..Default::default()
            });
            state.picker_window = Some(id);

            open_task.then(|id| {
                Task::batch([
                    window::gain_focus(id),
                    iced::widget::operation::focus_next(),
                ])
            })
        }
        Message::HidePicker => {
            if let Some(id) = state.picker_window.take() {
                state.query.clear();
                state.selected = None;
                crate::macos::hide_application();
                window::close(id)
            } else {
                Task::none()
            }
        }
        Message::QueryChanged(query) => {
            state.query = query;
            state.filtered_count = get_filtered_items(state).len();
            state.selected = if state.filtered_count > 0 {
                Some(0)
            } else {
                None
            };
            Task::none()
        }
        Message::SelectNext => {
            if state.filtered_count == 0 {
                return Task::none();
            }
            state.selected = Some(match state.selected {
                Some(idx) => (idx + 1).min(state.filtered_count - 1),
                None => 0,
            });
            Task::none()
        }
        Message::SelectPrev => {
            if state.filtered_count == 0 {
                state.selected = None;
            } else {
                state.selected = match state.selected {
                    Some(idx) if idx > 0 => Some(idx - 1),
                    _ => Some(0),
                };
            }
            Task::none()
        }
        Message::Confirm => {
            let items = get_filtered_items(state);
            if let Some(idx) = state.selected
                && let Some((_, app, window, _, _)) = items.get(idx)
            {
                let _ = window.focus(&app.app);
            }
            if let Some(id) = state.picker_window.take() {
                state.query.clear();
                state.selected = None;
                window::close(id)
            } else {
                Task::none()
            }
        }
        Message::WindowClosed(id) => {
            if state.picker_window == Some(id) {
                state.picker_window = None;
            }
            Task::none()
        }
        Message::NoOp => Task::none(),
    }
}

pub fn view(state: &Switcheroo, _window_id: window::Id) -> Element<'_, Message> {
    let items = get_filtered_items(state);

    let search = text_input("Search windows...", &state.query)
        .id(SEARCH_INPUT_ID)
        .on_input(Message::QueryChanged)
        .on_submit(Message::Confirm)
        .padding(10)
        .size(18);

    let mut result_rows: Vec<Element<'_, Message>> = Vec::new();

    for (idx, (pid, app, window, _, indices)) in items.iter().enumerate() {
        let is_selected = state.selected == Some(idx);
        let indices_set: HashSet<usize> = indices.iter().map(|&i| i as usize).collect();

        let normal_color = if is_selected {
            color!(0xffffff)
        } else {
            color!(0xcccccc)
        };
        let highlight_color = if is_selected {
            color!(0xffff96)
        } else {
            color!(0x64c8ff)
        };

        // App icon
        let icon_elem: Element<'_, Message> = if let Some(icon_data) = state.manager.get_icon(*pid)
        {
            image(image::Handle::from_rgba(
                icon_data.width,
                icon_data.height,
                icon_data.rgba.clone(),
            ))
            .width(24)
            .height(24)
            .into()
        } else {
            iced::widget::Space::new().width(24).height(24).into()
        };

        // App name with highlighted spans
        let mut app_name_spans: Vec<iced::widget::text::Span<'_>> = Vec::new();
        for (i, ch) in app.name.chars().enumerate() {
            let c = if indices_set.contains(&i) {
                highlight_color
            } else {
                normal_color
            };
            app_name_spans.push(span(ch.to_string()).color(c));
        }

        // Window title with highlighted spans (truncate to avoid multi-line rows)
        let max_title_chars = 80;
        let title_offset = app.name.len() + 1;
        let mut title_spans: Vec<iced::widget::text::Span<'_>> = Vec::new();
        let title_len = window.title.chars().count();
        for (i, ch) in window.title.chars().take(max_title_chars).enumerate() {
            let c = if indices_set.contains(&(i + title_offset)) {
                highlight_color
            } else {
                normal_color
            };
            title_spans.push(span(ch.to_string()).color(c));
        }
        if title_len > max_title_chars {
            title_spans.push(span("…").color(normal_color));
        }

        let row_content = row![
            icon_elem,
            container(rich_text(app_name_spans).size(14)).width(200),
            container(rich_text(title_spans).size(14)).width(Length::Fill),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let bg_color = if is_selected {
            color!(0x4682c8)
        } else {
            iced::Color::TRANSPARENT
        };

        let row_container = container(row_content)
            .padding([6, 10])
            .width(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(iced::Background::Color(bg_color)),
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });

        result_rows.push(row_container.into());
    }

    let results = scrollable(column(result_rows).spacing(2)).height(Length::Fill);

    let content = column![search, results].spacing(10).padding(20);

    let main_container = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color {
                r: 0.1,
                g: 0.1,
                b: 0.1,
                a: 0.9,
            })),
            border: iced::Border {
                radius: 10.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    center(main_container).into()
}

pub fn subscription(state: &Switcheroo) -> Subscription<Message> {
    let mut subs = vec![
        iced::time::every(iced::time::Duration::from_millis(16)).map(check_hotkey),
        window::close_events().map(Message::WindowClosed),
    ];

    if state.picker_window.is_some() {
        subs.push(iced::event::listen_with(
            |event, status, _window| match event {
                iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(Named::Escape),
                    ..
                }) => Some(Message::HidePicker),
                iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(Named::ArrowDown),
                    ..
                }) if status == iced::event::Status::Ignored => Some(Message::SelectNext),
                iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(Named::ArrowUp),
                    ..
                }) if status == iced::event::Status::Ignored => Some(Message::SelectPrev),
                _ => None,
            },
        ));
    }

    Subscription::batch(subs)
}

fn check_hotkey(_instant: std::time::Instant) -> Message {
    let receiver = GlobalHotKeyEvent::receiver();
    match receiver.try_recv() {
        Ok(event) if event.state() == HotKeyState::Released => Message::ShowPicker,
        _ => Message::NoOp,
    }
}

fn get_filtered_items(
    state: &Switcheroo,
) -> Vec<(i32, &windows::App, &windows::Window, u32, Vec<u32>)> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut items: Vec<(i32, &windows::App, &windows::Window, u32, Vec<u32>)> = Vec::new();

    let app_map = state.manager.app_map();
    if state.query.is_empty() {
        for (pid, app) in app_map {
            for win in &app.windows {
                items.push((*pid, app, win, 0, vec![]));
            }
        }
    } else {
        let needle = Utf32String::from(state.query.as_str());
        for (pid, app) in app_map {
            for win in &app.windows {
                let search_text = format!("{} {}", app.name, win.title);
                let haystack = Utf32String::from(search_text.as_str());
                let mut indices = Vec::new();
                if let Some(score) =
                    matcher.fuzzy_indices(haystack.slice(..), needle.slice(..), &mut indices)
                {
                    items.push((*pid, app, win, score as u32, indices));
                }
            }
        }
    }

    items.sort_by(|a, b| {
        b.3.cmp(&a.3)
            .then_with(|| a.1.name.cmp(&b.1.name))
            .then_with(|| a.2.title.cmp(&b.2.title))
    });

    items
}
