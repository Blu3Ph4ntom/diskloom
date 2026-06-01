#[derive(Default)]
pub struct DiskLoomApp;

impl eframe::App for DiskLoomApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("DiskLoom");
            ui.label("See your disk clearly.");
        });
    }
}
