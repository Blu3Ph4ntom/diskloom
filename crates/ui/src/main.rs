fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "DiskLoom",
        native_options,
        Box::new(|_| Ok(Box::<diskloom_ui::DiskLoomApp>::default())),
    )
}
