fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "DiskLoom",
        native_options,
        Box::new(|_| Ok(Box::new(diskloom_ui::DiskLoomApp::from_env_args()))),
    )
}
