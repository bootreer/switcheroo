use global_hotkey::{
    GlobalHotKeyManager,
    hotkey::{Code, HotKey, Modifiers},
};
use objc2_application_services::AXUIElement;

mod dock_swipe;
mod macos;
mod ui;
mod windows;

fn main() -> iced::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    if !macos::request_permissions() {
        std::process::exit(1);
    }

    // SAFETY: objc2 marks these unsafe by default; they take no pointers
    // beyond the live system-wide element.
    unsafe {
        let system_wide = AXUIElement::new_system_wide();
        AXUIElement::set_messaging_timeout(&system_wide, 0.5);
    }

    macos::set_accessory_mode();

    let hotkey_manager = GlobalHotKeyManager::new().expect("Could not create GlobalHotKeyManager");
    let hotkey = HotKey::new(Some(Modifiers::META), Code::KeyD);
    hotkey_manager
        .register(hotkey)
        .expect("Could not register hot key");

    // Leak the hotkey manager
    std::mem::forget(hotkey_manager);

    iced::daemon(ui::boot, ui::update, ui::view)
        .title(ui::title)
        .subscription(ui::subscription)
        .style(
            |_state: &ui::Switcheroo, _theme: &iced::Theme| iced::theme::Style {
                background_color: iced::Color::TRANSPARENT,
                text_color: iced::Color::WHITE,
            },
        )
        .run()
}
