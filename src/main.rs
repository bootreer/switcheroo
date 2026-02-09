use eframe::egui;
use objc2_application_services::AXUIElement;

mod app;
mod macos;

fn main() -> eframe::Result {
    let window_size = egui::vec2(800.0, 400.0);

    unsafe {
        let system_wide = AXUIElement::new_system_wide();
        AXUIElement::set_messaging_timeout(&system_wide, 0.5);
    }

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
        Box::new(|_cc| {
            // let ctx = &cc.egui_ctx;
            // let monitor_size = ctx.input(|i| i.viewport().monitor_size);
            // if let Some(monitor_size) = monitor_size {
            //     let centered_pos = egui::pos2(
            //         (monitor_size.x - window_size.x) / 2.0,
            //         (monitor_size.y - window_size.y) / 2.0,
            //     );
            //     ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(centered_pos));
            // }

            let app = app::App::new(windows);
            Ok(Box::new(app))
        }),
    )
}
