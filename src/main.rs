use eframe::egui;
use global_hotkey::{
    GlobalHotKeyManager,
    hotkey::{Code, HotKey, Modifiers},
};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_application_services::AXUIElement;

mod macos;
mod ui;

fn main() -> eframe::Result {
    let mtm = MainThreadMarker::new().expect("App not started in main thread");

    unsafe {
        let system_wide = AXUIElement::new_system_wide();
        AXUIElement::set_messaging_timeout(&system_wide, 0.5);
    }

    let manager = GlobalHotKeyManager::new().expect("Could not create GlobalHotKeyManager");
    let hotkey = HotKey::new(Some(Modifiers::META), Code::KeyD);
    manager
        .register(hotkey)
        .expect("Could not register hot key");

    let app = NSApplication::sharedApplication(mtm);
    if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
        println!("Could not set application as Accessory");
    }

    let window_size = egui::vec2(800.0, 400.0);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_inner_size(window_size)
            .with_transparent(true)
            .with_always_on_top(),
        ..Default::default()
    };

    let windows = macos::get_open_app_windows().expect("Couldn't get open windows");
    eframe::run_native(
        "switcheroo",
        options,
        Box::new(|_| {
            let app = ui::App::new(windows);
            Ok(Box::new(app))
        }),
    )
}
