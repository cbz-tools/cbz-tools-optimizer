//! About window.

use eframe::egui;

const PRODUCT_NAME: &str = "CBZ Optimizer";
const GITHUB_URL: &str = "https://github.com/cbz-tools/cbz-tools-optimizer";
const LATEST_RELEASE_URL: &str = "https://github.com/cbz-tools/cbz-tools-optimizer/releases/latest";
const ABOUT_WINDOW_DEFAULT_SIZE: egui::Vec2 = egui::vec2(320.0, 180.0);

pub fn show(ctx: &egui::Context, open: &mut bool) {
    if *open && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        *open = false;
        return;
    }

    let mut close_requested = false;
    egui::Window::new(PRODUCT_NAME)
        .open(open)
        .resizable(false)
        .collapsible(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.screen_rect().center())
        .default_size(ABOUT_WINDOW_DEFAULT_SIZE)
        .show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(PRODUCT_NAME).strong());
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            });

            ui.add_space(12.0);
            if ui.link("GitHub").clicked() {
                ctx.open_url(egui::OpenUrl::new_tab(GITHUB_URL));
            }
            if ui.link("Check for latest version").clicked() {
                ctx.open_url(egui::OpenUrl::new_tab(LATEST_RELEASE_URL));
            }

            ui.separator();
            if ui.button("Close").clicked() {
                close_requested = true;
            }
        });

    if close_requested {
        *open = false;
    }
}
