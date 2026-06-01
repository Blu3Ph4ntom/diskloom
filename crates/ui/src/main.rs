fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1320.0, 780.0])
            .with_min_inner_size([980.0, 620.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "DiskLoom",
        native_options,
        Box::new(|_| Ok(Box::new(diskloom_ui::DiskLoomApp::from_env_args()))),
    )
}
