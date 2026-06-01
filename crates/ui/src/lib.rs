use std::{
    borrow::Cow,
    fs::File,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Instant,
};

use diskloom_core::{EntryFlags, EntryId, FileGraph};
use diskloom_dupes::{DuplicateCandidate, find_duplicate_candidates};
use diskloom_export::{CsvExportOptions, export_csv};
use diskloom_ntfs::{NtfsScanControl, NtfsScanProgress, NtfsScanner};
use diskloom_query::{
    FileTypeStat, NameMatcher, QueryFilter, TreemapBounds, TreemapItem, TreemapRect,
    file_type_stats, layout_treemap, top_entries_by_own_size, top_entries_by_total_size,
};
use diskloom_scan::{FallbackScanner, ScanControl, ScanOptions, ScanSummary};
use diskloom_windows::{
    VolumeKind, discover_volumes, is_process_elevated, open_in_explorer, recycle_delete,
    relaunch_current_process_elevated, rename_path, show_properties,
};

const UI_PROGRESS_EVERY: u64 = 1_024;
const TREE_ROW_LIMIT: usize = 500;
const DUPLICATE_GROUP_LIMIT: usize = 100;
const DUPLICATE_PATH_LIMIT: usize = 20;
const TABLE_ROW_HEIGHT: f32 = 26.0;
const TABLE_HEADER_HEIGHT: f32 = 24.0;
const TABLE_PAD_X: f32 = 8.0;
const SIZE_COL_WIDTH: f32 = 104.0;
const KIND_COL_WIDTH: f32 = 64.0;
const COUNT_COL_WIDTH: f32 = 72.0;
const MODIFIED_COL_WIDTH: f32 = 112.0;

#[derive(Debug)]
pub struct DiskLoomApp {
    path: String,
    volumes: Vec<VolumeShortcut>,
    scanner_mode: UiScannerMode,
    filters: FilterInputs,
    view_cache: Option<ViewCache>,
    selected_id: Option<EntryId>,
    selected_path: Option<PathBuf>,
    rename_target: String,
    export_path: String,
    export_include_directories: bool,
    action_status: Option<ActionStatus>,
    action_receiver: Option<Receiver<ActionStatus>>,
    duplicate_state: DuplicateState,
    duplicate_receiver: Option<Receiver<DuplicateMessage>>,
    active_tab: ActiveTab,
    state: UiState,
    receiver: Option<Receiver<ScanMessage>>,
    scan_cancel: Option<Arc<AtomicBool>>,
    start_on_launch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeShortcut {
    root: String,
    label: String,
    is_ntfs: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Tree,
    Files,
    Types,
    Treemap,
    Duplicates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiScannerMode {
    Auto,
    Ntfs,
    Fallback,
}

impl UiScannerMode {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Ntfs => "NTFS",
            Self::Fallback => "Fallback",
        }
    }

    fn arg_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ntfs => "ntfs",
            Self::Fallback => "fallback",
        }
    }

    fn from_arg(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "ntfs" => Some(Self::Ntfs),
            "fallback" => Some(Self::Fallback),
            _ => None,
        }
    }
}

#[derive(Debug)]
enum UiState {
    Idle,
    Scanning(Option<UiScanProgress>),
    Complete(Box<ScanResult>),
    Error(String),
}

#[derive(Debug)]
enum ScanMessage {
    Progress(UiScanProgress),
    Complete(Box<ScanResult>),
    Error(String),
}

#[derive(Debug)]
enum DuplicateMessage {
    Complete(Vec<DuplicateGroup>),
}

#[derive(Debug, Clone, Copy)]
struct UiScanProgress {
    summary: ScanSummary,
    elapsed_ms: u128,
}

#[derive(Debug)]
struct ScanResult {
    graph: Arc<FileGraph>,
    summary: ScanSummary,
    elapsed_ms: u128,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
    total_size: u64,
    total_allocated: u64,
    tree_rows: Vec<TreeRow>,
    file_types: Vec<FileTypeStat>,
    treemap_items: Vec<TreemapItem>,
}

#[derive(Debug, Clone, Copy)]
struct TreeRow {
    id: EntryId,
    depth: usize,
    kind: &'static str,
    size: u64,
    allocated: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ChildRange {
    start: u32,
    len: u32,
}

#[derive(Debug, Clone)]
struct ResultRow {
    id: EntryId,
    path_text: String,
    kind: &'static str,
    size: u64,
    allocated: u64,
    modified_unix: i64,
}

#[derive(Debug, Clone)]
struct DuplicateGroup {
    name: String,
    size: u64,
    modified_unix: i64,
    count: usize,
    wasted_bytes: u64,
    paths: Vec<DuplicatePath>,
}

#[derive(Debug, Clone)]
struct DuplicatePath {
    id: EntryId,
    path_text: String,
}

#[derive(Debug)]
enum DuplicateState {
    Idle,
    Running,
    Ready(Vec<DuplicateGroup>),
}

struct CellTextStyle {
    font: egui::FontId,
    color: egui::Color32,
    middle: bool,
}

#[derive(Debug, Clone, Default)]
struct FilterInputs {
    name: String,
    extension: String,
    path: String,
    min_size: String,
    max_size: String,
    min_allocated: String,
    max_allocated: String,
    modified_after: String,
    modified_before: String,
    regex: bool,
    include_directories: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilterSignature {
    name: String,
    extension: String,
    path: String,
    min_size: String,
    max_size: String,
    min_allocated: String,
    max_allocated: String,
    modified_after: String,
    modified_before: String,
    regex: bool,
    include_directories: bool,
}

#[derive(Debug)]
struct ViewCache {
    signature: FilterSignature,
    matched: usize,
    rows: Vec<ResultRow>,
}

#[derive(Debug, Clone)]
struct ActionStatus {
    message: String,
    is_error: bool,
}

impl Default for DiskLoomApp {
    fn default() -> Self {
        let volumes = discover_volume_shortcuts();
        let system_drive = std::env::var("SystemDrive").ok();
        Self {
            path: default_scan_path_from(system_drive.as_deref(), &volumes),
            volumes,
            scanner_mode: UiScannerMode::Auto,
            filters: FilterInputs {
                include_directories: true,
                ..FilterInputs::default()
            },
            view_cache: None,
            selected_id: None,
            selected_path: None,
            rename_target: String::new(),
            export_path: "diskloom-export.csv".to_owned(),
            export_include_directories: true,
            action_status: None,
            action_receiver: None,
            duplicate_state: DuplicateState::Idle,
            duplicate_receiver: None,
            active_tab: ActiveTab::Tree,
            state: UiState::Idle,
            receiver: None,
            scan_cancel: None,
            start_on_launch: false,
        }
    }
}

impl DiskLoomApp {
    #[must_use]
    pub fn from_env_args() -> Self {
        Self::from_launch_args(std::env::args().skip(1))
    }

    fn from_launch_args(args: impl IntoIterator<Item = String>) -> Self {
        let mut app = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--path" => {
                    if let Some(path) = args.next() {
                        app.path = path;
                    }
                }
                "--scanner" => {
                    if let Some(scanner) = args
                        .next()
                        .and_then(|value| UiScannerMode::from_arg(&value))
                    {
                        app.scanner_mode = scanner;
                    }
                }
                "--scan" => {
                    app.start_on_launch = true;
                }
                _ => {}
            }
        }
        app
    }
}

impl eframe::App for DiskLoomApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        apply_app_style(ctx);
        self.receive_scan();
        self.receive_action();
        self.receive_duplicates();
        if self.start_on_launch {
            self.start_on_launch = false;
            self.start_scan();
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("DiskLoom");
                ui.separator();
                ui.label("See your disk clearly.");
            });
            ui.add_space(6.0);
        });

        egui::SidePanel::left("control_panel")
            .resizable(true)
            .default_width(320.0)
            .width_range(280.0..=380.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.add_space(8.0);
                        self.scan_setup_controls(ui);
                        ui.separator();
                        self.status_line(ui);
                        ui.separator();
                        self.filter_controls(ui);
                        ui.separator();
                        self.action_controls(ui);
                    });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            self.result_summary(ui);
            ui.add_space(4.0);
            self.tabs(ui);
            ui.separator();
            self.active_view(ui);
        });

        if matches!(self.state, UiState::Scanning(_))
            || matches!(self.duplicate_state, DuplicateState::Running)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

impl DiskLoomApp {
    fn start_scan(&mut self) {
        let trimmed = self.path.trim();
        let path = PathBuf::from(if trimmed.is_empty() { "." } else { trimmed });
        let scanner_mode = self.scanner_mode;
        let cancel = Arc::new(AtomicBool::new(false));
        match maybe_relaunch_ui_scan_elevated(&path, scanner_mode) {
            Ok(true) => {
                self.action_status = Some(ActionStatus {
                    message: "administrator access requested for direct NTFS scanning".to_owned(),
                    is_error: false,
                });
                return;
            }
            Ok(false) => {}
            Err(error) => {
                self.action_status = Some(ActionStatus {
                    message: error,
                    is_error: true,
                });
                return;
            }
        }

        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.scan_cancel = Some(Arc::clone(&cancel));
        self.state = UiState::Scanning(None);
        self.view_cache = None;
        self.selected_id = None;
        self.selected_path = None;
        self.rename_target.clear();
        self.action_status = None;
        self.duplicate_state = DuplicateState::Idle;
        self.duplicate_receiver = None;

        thread::spawn(move || {
            let started = Instant::now();
            let progress_sender = sender.clone();
            let mut on_progress = |summary| {
                let _ = progress_sender.send(ScanMessage::Progress(UiScanProgress {
                    summary,
                    elapsed_ms: started.elapsed().as_millis(),
                }));
            };
            let result = scan_path(path, scanner_mode, &cancel, &mut on_progress).map(|outcome| {
                let graph = Arc::new(outcome.graph);
                let tree_rows = tree_rows_from_graph(&graph, TREE_ROW_LIMIT);
                let file_types = file_type_stats(&graph, 50);
                let treemap_items = treemap_items_from_graph(&graph, 120);
                let (total_size, total_allocated) = graph_totals(&graph);
                ScanResult {
                    graph,
                    summary: outcome.summary,
                    elapsed_ms: started.elapsed().as_millis(),
                    scanner_label: outcome.scanner_label,
                    fallback_reason: outcome.fallback_reason,
                    total_size,
                    total_allocated,
                    tree_rows,
                    file_types,
                    treemap_items,
                }
            });

            let message = match result {
                Ok(result) => ScanMessage::Complete(Box::new(result)),
                Err(error) => ScanMessage::Error(error),
            };
            let _ = sender.send(message);
        });
    }

    fn receive_scan(&mut self) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };

        let mut keep_receiver = true;
        loop {
            match receiver.try_recv() {
                Ok(ScanMessage::Progress(progress)) => {
                    self.state = UiState::Scanning(Some(progress));
                }
                Ok(ScanMessage::Complete(result)) => {
                    self.state = UiState::Complete(result);
                    self.scan_cancel = None;
                    self.view_cache = None;
                    self.selected_id = None;
                    self.selected_path = None;
                    self.rename_target.clear();
                    self.duplicate_state = DuplicateState::Idle;
                    self.duplicate_receiver = None;
                    keep_receiver = false;
                    break;
                }
                Ok(ScanMessage::Error(error)) => {
                    self.state = UiState::Error(error);
                    self.scan_cancel = None;
                    self.view_cache = None;
                    self.selected_id = None;
                    self.selected_path = None;
                    self.rename_target.clear();
                    self.duplicate_state = DuplicateState::Idle;
                    self.duplicate_receiver = None;
                    keep_receiver = false;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.state = UiState::Error("scan worker stopped".to_owned());
                    self.scan_cancel = None;
                    self.view_cache = None;
                    self.selected_id = None;
                    self.selected_path = None;
                    self.rename_target.clear();
                    self.duplicate_state = DuplicateState::Idle;
                    self.duplicate_receiver = None;
                    keep_receiver = false;
                    break;
                }
            }
        }

        if keep_receiver {
            self.receiver = Some(receiver);
        }
    }

    fn receive_action(&mut self) {
        let Some(receiver) = self.action_receiver.take() else {
            return;
        };

        match receiver.try_recv() {
            Ok(status) => {
                self.action_status = Some(status);
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.action_receiver = Some(receiver);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.action_status = Some(ActionStatus {
                    message: "background action stopped".to_owned(),
                    is_error: true,
                });
            }
        }
    }

    fn receive_duplicates(&mut self) {
        let Some(receiver) = self.duplicate_receiver.take() else {
            return;
        };

        match receiver.try_recv() {
            Ok(DuplicateMessage::Complete(groups)) => {
                self.duplicate_state = DuplicateState::Ready(groups);
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.duplicate_receiver = Some(receiver);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.duplicate_state = DuplicateState::Idle;
                self.action_status = Some(ActionStatus {
                    message: "duplicate analysis stopped".to_owned(),
                    is_error: true,
                });
            }
        }
    }

    fn filter_controls(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.strong("Filter");
        ui.add_space(4.0);

        ui.vertical(|ui| {
            ui.label("Search");
            changed |= ui
                .add_sized(
                    [ui.available_width(), 24.0],
                    egui::TextEdit::singleline(&mut self.filters.name),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut self.filters.regex, "Regex").changed();
            ui.label("Ext");
            changed |= ui
                .add_sized(
                    [80.0, 24.0],
                    egui::TextEdit::singleline(&mut self.filters.extension),
                )
                .changed();
        });
        ui.vertical(|ui| {
            ui.label("Path");
            changed |= ui
                .add_sized(
                    [ui.available_width(), 24.0],
                    egui::TextEdit::singleline(&mut self.filters.path),
                )
                .changed();
        });

        ui.columns(2, |columns| {
            columns[0].label("Min size");
            changed |= columns[0]
                .add_sized(
                    [columns[0].available_width(), 24.0],
                    egui::TextEdit::singleline(&mut self.filters.min_size),
                )
                .changed();
            columns[1].label("Max size");
            changed |= columns[1]
                .add_sized(
                    [columns[1].available_width(), 24.0],
                    egui::TextEdit::singleline(&mut self.filters.max_size),
                )
                .changed();
        });

        ui.columns(2, |columns| {
            columns[0].label("Min allocated");
            changed |= columns[0]
                .add_sized(
                    [columns[0].available_width(), 24.0],
                    egui::TextEdit::singleline(&mut self.filters.min_allocated),
                )
                .changed();
            columns[1].label("Max allocated");
            changed |= columns[1]
                .add_sized(
                    [columns[1].available_width(), 24.0],
                    egui::TextEdit::singleline(&mut self.filters.max_allocated),
                )
                .changed();
        });

        ui.columns(2, |columns| {
            columns[0].label("Modified after");
            changed |= columns[0]
                .add_sized(
                    [columns[0].available_width(), 24.0],
                    egui::TextEdit::singleline(&mut self.filters.modified_after),
                )
                .changed();
            columns[1].label("Modified before");
            changed |= columns[1]
                .add_sized(
                    [columns[1].available_width(), 24.0],
                    egui::TextEdit::singleline(&mut self.filters.modified_before),
                )
                .changed();
        });
        changed |= ui
            .checkbox(&mut self.filters.include_directories, "Dirs")
            .changed();

        if changed {
            self.view_cache = None;
        }
    }

    fn scan_controls(&mut self, ui: &mut egui::Ui) {
        if !matches!(self.state, UiState::Scanning(_)) {
            return;
        }

        ui.horizontal(|ui| {
            if ui.button("Cancel scan").clicked() {
                if let Some(cancel) = &self.scan_cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.action_status = Some(ActionStatus {
                    message: "cancelling scan".to_owned(),
                    is_error: false,
                });
            }
        });
    }

    fn scan_setup_controls(&mut self, ui: &mut egui::Ui) {
        ui.strong("Scan");
        ui.add_space(4.0);
        ui.label("Path");
        ui.add_sized(
            [ui.available_width(), 26.0],
            egui::TextEdit::singleline(&mut self.path),
        );

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Scanner");
            ui.selectable_value(
                &mut self.scanner_mode,
                UiScannerMode::Auto,
                UiScannerMode::Auto.label(),
            );
            ui.selectable_value(
                &mut self.scanner_mode,
                UiScannerMode::Ntfs,
                UiScannerMode::Ntfs.label(),
            );
            ui.selectable_value(
                &mut self.scanner_mode,
                UiScannerMode::Fallback,
                UiScannerMode::Fallback.label(),
            );
        });

        if !self.volumes.is_empty() {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("Drives");
                for volume in &self.volumes {
                    if ui
                        .button(&volume.label)
                        .on_hover_text(&volume.root)
                        .clicked()
                    {
                        self.path = volume.root.clone();
                    }
                }
                if ui.button("Refresh").clicked() {
                    self.volumes = discover_volume_shortcuts();
                }
            });
        }

        ui.add_space(8.0);
        let scanning = matches!(self.state, UiState::Scanning(_));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !scanning,
                    egui::Button::new("Scan").min_size(egui::vec2(96.0, 30.0)),
                )
                .clicked()
            {
                self.start_scan();
            }
            self.scan_controls(ui);
        });
    }

    fn action_controls(&mut self, ui: &mut egui::Ui) {
        ui.strong("Actions");
        ui.add_space(4.0);

        ui.vertical(|ui| {
            ui.label("CSV path");
            ui.add_sized(
                [ui.available_width(), 24.0],
                egui::TextEdit::singleline(&mut self.export_path),
            );
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.export_include_directories, "Dirs");
            let export_enabled =
                matches!(self.state, UiState::Complete(_)) && self.action_receiver.is_none();
            if ui
                .add_enabled(export_enabled, egui::Button::new("Export"))
                .clicked()
            {
                self.start_export();
            }
        });

        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.label("Selected");
            let selected = self
                .selected_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            let mut selected_text = selected;
            ui.add_enabled(
                false,
                egui::TextEdit::singleline(&mut selected_text).desired_width(ui.available_width()),
            );
        });

        ui.horizontal(|ui| {
            let enabled = self.selected_path.is_some();
            if ui
                .add_enabled(enabled, egui::Button::new("Explorer"))
                .clicked()
            {
                self.run_shell_action("opened in Explorer", |path| open_in_explorer(path));
            }
            if ui
                .add_enabled(enabled, egui::Button::new("Properties"))
                .clicked()
            {
                self.run_shell_action("properties opened", |path| show_properties(path));
            }
            if ui
                .add_enabled(enabled, egui::Button::new("Recycle"))
                .clicked()
            {
                self.run_shell_action("sent to Recycle Bin", |path| recycle_delete(path));
            }
        });

        ui.horizontal(|ui| {
            ui.label("Rename");
            ui.add_enabled(
                self.selected_path.is_some(),
                egui::TextEdit::singleline(&mut self.rename_target).desired_width(180.0),
            );
            if ui
                .add_enabled(self.selected_path.is_some(), egui::Button::new("Apply"))
                .clicked()
            {
                self.rename_selected();
            }
        });

        if let Some(status) = &self.action_status {
            let color = if status.is_error {
                egui::Color32::from_rgb(255, 128, 104)
            } else {
                egui::Color32::from_rgb(132, 204, 153)
            };
            ui.colored_label(color, &status.message);
        }
    }

    fn start_export(&mut self) {
        let Some((graph, output_path, include_directories)) = self.export_request() else {
            return;
        };

        let (sender, receiver) = mpsc::channel();
        self.action_receiver = Some(receiver);
        self.action_status = Some(ActionStatus {
            message: "exporting CSV".to_owned(),
            is_error: false,
        });

        thread::spawn(move || {
            let status = match export_graph_to_csv(&graph, &output_path, include_directories) {
                Ok(()) => ActionStatus {
                    message: format!("CSV exported to {}", output_path.display()),
                    is_error: false,
                },
                Err(error) => ActionStatus {
                    message: error,
                    is_error: true,
                },
            };
            let _ = sender.send(status);
        });
    }

    fn export_request(&mut self) -> Option<(Arc<FileGraph>, PathBuf, bool)> {
        let graph = match &self.state {
            UiState::Complete(result) => Arc::clone(&result.graph),
            _ => return None,
        };
        let trimmed = self.export_path.trim();
        if trimmed.is_empty() {
            self.action_status = Some(ActionStatus {
                message: "CSV path is empty".to_owned(),
                is_error: true,
            });
            return None;
        }

        Some((
            graph,
            PathBuf::from(trimmed),
            self.export_include_directories,
        ))
    }

    fn start_duplicate_scan(&mut self) {
        let graph = match &self.state {
            UiState::Complete(result) => Arc::clone(&result.graph),
            _ => return,
        };
        if matches!(self.duplicate_state, DuplicateState::Running) {
            return;
        }

        let (sender, receiver) = mpsc::channel();
        self.duplicate_receiver = Some(receiver);
        self.duplicate_state = DuplicateState::Running;

        thread::spawn(move || {
            let groups =
                duplicate_groups_from_graph(&graph, DUPLICATE_GROUP_LIMIT, DUPLICATE_PATH_LIMIT);
            let _ = sender.send(DuplicateMessage::Complete(groups));
        });
    }

    fn status_line(&self, ui: &mut egui::Ui) {
        ui.strong("Status");
        ui.add_space(4.0);
        match &self.state {
            UiState::Idle => {
                ui.label("Ready");
            }
            UiState::Scanning(progress) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Scanning");
                });
                if let Some(progress) = progress {
                    metric_grid(
                        ui,
                        [
                            ("Entries", format_count(progress.summary.entries)),
                            ("Files", format_count(progress.summary.files)),
                            ("Dirs", format_count(progress.summary.directories)),
                            ("Elapsed", format!("{} ms", progress.elapsed_ms)),
                        ],
                    );
                    if progress.summary.inaccessible > 0 {
                        ui.colored_label(
                            egui::Color32::from_rgb(224, 164, 82),
                            format!("{} inaccessible", progress.summary.inaccessible),
                        );
                    }
                }
            }
            UiState::Complete(result) => {
                ui.label(result.scanner_label);
                metric_grid(
                    ui,
                    [
                        ("Size", format_bytes(result.total_size)),
                        ("Allocated", format_bytes(result.total_allocated)),
                        ("Entries", format_count(result.summary.entries)),
                        ("Elapsed", format!("{} ms", result.elapsed_ms)),
                    ],
                );
                ui.label(format!(
                    "{} files, {} directories",
                    format_count(result.summary.files),
                    format_count(result.summary.directories)
                ));
                if let Some(reason) = &result.fallback_reason {
                    ui.colored_label(
                        egui::Color32::from_rgb(224, 164, 82),
                        format!("Fallback: {reason}"),
                    );
                }
            }
            UiState::Error(error) => {
                ui.colored_label(egui::Color32::from_rgb(255, 128, 104), error);
            }
        }
    }

    fn result_summary(&self, ui: &mut egui::Ui) {
        match &self.state {
            UiState::Idle => {
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Ready");
                    ui.separator();
                    ui.label("No scan loaded");
                });
            }
            UiState::Scanning(progress) => {
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    ui.strong("Scanning");
                    if let Some(progress) = progress {
                        ui.separator();
                        ui.monospace(format!(
                            "{} entries",
                            format_count(progress.summary.entries)
                        ));
                        ui.monospace(format!("{} files", format_count(progress.summary.files)));
                        ui.monospace(format!("{} ms", progress.elapsed_ms));
                    }
                });
            }
            UiState::Complete(result) => {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(result.scanner_label);
                    ui.separator();
                    ui.monospace(format!("{} entries", format_count(result.summary.entries)));
                    ui.monospace(format!("{} files", format_count(result.summary.files)));
                    ui.monospace(format!("{} dirs", format_count(result.summary.directories)));
                    ui.monospace(format!("size {}", format_bytes(result.total_size)));
                    ui.monospace(format!(
                        "allocated {}",
                        format_bytes(result.total_allocated)
                    ));
                    ui.monospace(format!("{} ms", result.elapsed_ms));
                });
            }
            UiState::Error(error) => {
                ui.colored_label(egui::Color32::from_rgb(255, 128, 104), error);
            }
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.active_tab, ActiveTab::Tree, "Tree");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Files, "Files");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Types, "Types");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Treemap, "Treemap");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Duplicates, "Duplicates");
        });
    }

    fn active_view(&mut self, ui: &mut egui::Ui) {
        match self.active_tab {
            ActiveTab::Tree => self.tree(ui),
            ActiveTab::Files => self.results(ui),
            ActiveTab::Types => self.type_stats(ui),
            ActiveTab::Treemap => self.treemap(ui),
            ActiveTab::Duplicates => self.duplicates(ui),
        }
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        let (graph, graph_len, rows) = match &self.state {
            UiState::Complete(result) => (
                Arc::clone(&result.graph),
                result.graph.len(),
                result.tree_rows.clone(),
            ),
            _ => return,
        };

        ui.label(format!("Showing {} of {} entries", rows.len(), graph_len));
        table_header(
            ui,
            &[
                ("Size", SIZE_COL_WIDTH),
                ("Allocated", SIZE_COL_WIDTH),
                ("Kind", KIND_COL_WIDTH),
            ],
            "Name",
        );

        let mut clicked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for (idx, row) in rows.iter().enumerate() {
                    let selected = self.selected_id == Some(row.id);
                    let name = graph.name(row.id).unwrap_or_default();
                    let response = paint_tree_row(ui, idx, selected, row, name);
                    let row_clicked = response.clicked();
                    if response.hovered()
                        && let Some(path) = graph.reconstruct_path(row.id)
                    {
                        response.on_hover_text(path.display().to_string());
                    }
                    if row_clicked {
                        clicked = Some(row.id);
                    }
                }
            });

        if let Some(id) = clicked {
            self.select_entry(&graph, id);
        }
    }

    fn results(&mut self, ui: &mut egui::Ui) {
        let graph = match &self.state {
            UiState::Complete(result) => Arc::clone(&result.graph),
            _ => return,
        };
        let graph_len = graph.len();
        let cache = match self.ensure_view_cache() {
            Ok(cache) => cache,
            Err(error) => {
                ui.colored_label(egui::Color32::from_rgb(255, 128, 104), error);
                return;
            }
        };
        let matched = cache.matched;
        let rows = cache.rows.clone();

        ui.label(format!("{matched} of {graph_len} entries"));
        table_header(
            ui,
            &[
                ("Size", SIZE_COL_WIDTH),
                ("Allocated", SIZE_COL_WIDTH),
                ("Kind", KIND_COL_WIDTH),
                ("Modified", MODIFIED_COL_WIDTH),
            ],
            "Path",
        );

        let mut clicked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for (idx, row) in rows.iter().enumerate() {
                    let selected = self.selected_id == Some(row.id);
                    let response = paint_result_row(ui, idx, selected, row);
                    if response.clicked() {
                        clicked = Some(row.id);
                    }
                }
            });

        if let Some(id) = clicked {
            self.select_entry(&graph, id);
        }
    }

    fn type_stats(&self, ui: &mut egui::Ui) {
        let UiState::Complete(result) = &self.state else {
            return;
        };

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("types_grid")
                .striped(true)
                .min_col_width(96.0)
                .show(ui, |ui| {
                    ui.strong("Size");
                    ui.strong("Allocated");
                    ui.strong("Files");
                    ui.strong("Extension");
                    ui.end_row();

                    for stat in &result.file_types {
                        ui.monospace(format_bytes(stat.size));
                        ui.monospace(format_bytes(stat.allocated));
                        ui.monospace(format_count(stat.files));
                        ui.label(&stat.extension);
                        ui.end_row();
                    }
                });
        });
    }

    fn treemap(&self, ui: &mut egui::Ui) {
        let UiState::Complete(result) = &self.state else {
            return;
        };

        let available = ui.available_size();
        let height = available.y.max(280.0);
        let (response, painter) =
            ui.allocate_painter(egui::vec2(available.x, height), egui::Sense::hover());
        let rect = response.rect;
        let treemap = layout_treemap(
            &result.treemap_items,
            TreemapBounds {
                x: rect.left(),
                y: rect.top(),
                width: rect.width(),
                height: rect.height(),
            },
        );

        for (idx, item) in treemap.iter().enumerate() {
            paint_treemap_rect(&painter, item, idx);
        }
    }

    fn duplicates(&mut self, ui: &mut egui::Ui) {
        let graph = match &self.state {
            UiState::Complete(result) => Arc::clone(&result.graph),
            _ => return,
        };

        ui.horizontal(|ui| {
            let can_start = !matches!(self.duplicate_state, DuplicateState::Running);
            if ui
                .add_enabled(can_start, egui::Button::new("Find candidates"))
                .clicked()
            {
                self.start_duplicate_scan();
            }
            match &self.duplicate_state {
                DuplicateState::Idle => {
                    ui.label("Not run");
                }
                DuplicateState::Running => {
                    ui.spinner();
                    ui.label("Finding duplicate candidates");
                }
                DuplicateState::Ready(groups) => {
                    ui.label(format!("{} groups", groups.len()));
                }
            }
        });

        let groups = match &self.duplicate_state {
            DuplicateState::Ready(groups) => groups.clone(),
            _ => return,
        };

        table_header(
            ui,
            &[
                ("Wasted", SIZE_COL_WIDTH),
                ("Size", SIZE_COL_WIDTH),
                ("Count", COUNT_COL_WIDTH),
                ("Modified", MODIFIED_COL_WIDTH),
            ],
            "Name / Path",
        );

        let mut clicked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let mut row_idx = 0;
                for group in &groups {
                    paint_duplicate_group_row(ui, row_idx, group);
                    row_idx += 1;

                    for duplicate_path in &group.paths {
                        let selected = self.selected_id == Some(duplicate_path.id);
                        let response =
                            paint_duplicate_path_row(ui, row_idx, selected, duplicate_path);
                        if response.clicked() {
                            clicked = Some(duplicate_path.id);
                        }
                        row_idx += 1;
                    }
                }
            });

        if let Some(id) = clicked {
            self.select_entry(&graph, id);
        }
    }

    fn ensure_view_cache(&mut self) -> Result<&ViewCache, String> {
        let signature = self.filters.signature();
        let is_fresh = self
            .view_cache
            .as_ref()
            .is_some_and(|cache| cache.signature == signature);
        if !is_fresh {
            let UiState::Complete(result) = &self.state else {
                return Err("no scan result".to_owned());
            };
            let filtered = filtered_rows_from_graph(&result.graph, &self.filters, 500)?;
            self.view_cache = Some(ViewCache {
                signature,
                matched: filtered.matched,
                rows: filtered.rows,
            });
        }

        self.view_cache
            .as_ref()
            .ok_or_else(|| "view cache is empty".to_owned())
    }

    fn select_entry(&mut self, graph: &FileGraph, id: EntryId) {
        let Some(path) = graph.reconstruct_path(id) else {
            self.action_status = Some(ActionStatus {
                message: "selected path is unavailable".to_owned(),
                is_error: true,
            });
            return;
        };
        self.rename_target = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.selected_id = Some(id);
        self.selected_path = Some(path);
        self.action_status = None;
    }

    fn run_shell_action(
        &mut self,
        success_message: &'static str,
        action: impl FnOnce(&Path) -> Result<(), diskloom_windows::ShellActionError>,
    ) {
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        match action(&path) {
            Ok(()) => {
                self.action_status = Some(ActionStatus {
                    message: success_message.to_owned(),
                    is_error: false,
                });
            }
            Err(error) => {
                self.action_status = Some(ActionStatus {
                    message: error.to_string(),
                    is_error: true,
                });
            }
        }
    }

    fn rename_selected(&mut self) {
        let Some(from) = self.selected_path.clone() else {
            return;
        };
        let target = self.rename_target.trim();
        if target.is_empty() {
            self.action_status = Some(ActionStatus {
                message: "rename target is empty".to_owned(),
                is_error: true,
            });
            return;
        }
        let Some(parent) = from.parent() else {
            self.action_status = Some(ActionStatus {
                message: "selected path has no parent".to_owned(),
                is_error: true,
            });
            return;
        };
        let to = parent.join(target);

        match rename_path(&from, &to) {
            Ok(()) => {
                self.selected_path = Some(to);
                self.selected_id = None;
                self.action_status = Some(ActionStatus {
                    message: "renamed; scan data is stale".to_owned(),
                    is_error: false,
                });
            }
            Err(error) => {
                self.action_status = Some(ActionStatus {
                    message: error.to_string(),
                    is_error: true,
                });
            }
        }
    }
}

fn apply_app_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(24, 26, 28);
    visuals.window_fill = egui::Color32::from_rgb(28, 30, 32);
    visuals.faint_bg_color = egui::Color32::from_rgb(34, 37, 40);
    visuals.extreme_bg_color = egui::Color32::from_rgb(18, 20, 22);
    visuals.selection.bg_fill = egui::Color32::from_rgb(66, 107, 92);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size = egui::vec2(40.0, 28.0);
    ctx.set_style(style);
}

fn metric_grid<const N: usize>(ui: &mut egui::Ui, rows: [(&'static str, String); N]) {
    egui::Grid::new(ui.next_auto_id())
        .num_columns(2)
        .spacing(egui::vec2(16.0, 3.0))
        .show(ui, |ui| {
            for (label, value) in rows {
                ui.label(label);
                ui.monospace(value);
                ui.end_row();
            }
        });
}

fn table_header(ui: &mut egui::Ui, fixed_columns: &[(&str, f32)], tail_label: &str) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TABLE_HEADER_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(31, 34, 37));

    let font = egui::FontId::proportional(12.0);
    let color = egui::Color32::from_rgb(184, 188, 190);
    let mut x = rect.left() + TABLE_PAD_X;
    for (label, width) in fixed_columns {
        paint_text(
            painter,
            rect,
            x,
            *width,
            label,
            cell_text_style(font.clone(), color, false),
        );
        x += *width;
    }
    paint_text(
        painter,
        rect,
        x,
        (rect.right() - x - TABLE_PAD_X).max(24.0),
        tail_label,
        cell_text_style(font, color, false),
    );
}

fn paint_tree_row(
    ui: &mut egui::Ui,
    row_idx: usize,
    selected: bool,
    row: &TreeRow,
    name: &str,
) -> egui::Response {
    let (rect, response) = table_row(ui, row_idx, selected);
    let painter = ui.painter();
    let mono = egui::FontId::monospace(12.0);
    let regular = egui::FontId::proportional(12.0);
    let text_color = table_text_color(selected);
    let muted = egui::Color32::from_rgb(150, 154, 156);

    let mut x = rect.left() + TABLE_PAD_X;
    paint_text(
        painter,
        rect,
        x,
        SIZE_COL_WIDTH,
        &format_bytes(row.size),
        cell_text_style(mono.clone(), text_color, false),
    );
    x += SIZE_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        SIZE_COL_WIDTH,
        &format_bytes(row.allocated),
        cell_text_style(mono, text_color, false),
    );
    x += SIZE_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        KIND_COL_WIDTH,
        row.kind,
        cell_text_style(regular.clone(), muted, false),
    );
    x += KIND_COL_WIDTH;

    let indent = (row.depth as f32 * 14.0).min(180.0);
    let name_x = x + indent;
    paint_text(
        painter,
        rect,
        name_x,
        (rect.right() - name_x - TABLE_PAD_X).max(24.0),
        name,
        cell_text_style(regular, text_color, false),
    );

    response
}

fn paint_result_row(
    ui: &mut egui::Ui,
    row_idx: usize,
    selected: bool,
    row: &ResultRow,
) -> egui::Response {
    let (rect, response) = table_row(ui, row_idx, selected);
    let painter = ui.painter();
    let mono = egui::FontId::monospace(12.0);
    let regular = egui::FontId::proportional(12.0);
    let text_color = table_text_color(selected);
    let muted = egui::Color32::from_rgb(150, 154, 156);

    let mut x = rect.left() + TABLE_PAD_X;
    paint_text(
        painter,
        rect,
        x,
        SIZE_COL_WIDTH,
        &format_bytes(row.size),
        cell_text_style(mono.clone(), text_color, false),
    );
    x += SIZE_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        SIZE_COL_WIDTH,
        &format_bytes(row.allocated),
        cell_text_style(mono.clone(), text_color, false),
    );
    x += SIZE_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        KIND_COL_WIDTH,
        row.kind,
        cell_text_style(regular, muted, false),
    );
    x += KIND_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        MODIFIED_COL_WIDTH,
        &row.modified_unix.to_string(),
        cell_text_style(mono, muted, false),
    );
    x += MODIFIED_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        (rect.right() - x - TABLE_PAD_X).max(24.0),
        &row.path_text,
        cell_text_style(egui::FontId::proportional(12.0), text_color, true),
    );

    if response.hovered() {
        response.on_hover_text(&row.path_text)
    } else {
        response
    }
}

fn paint_duplicate_group_row(ui: &mut egui::Ui, row_idx: usize, group: &DuplicateGroup) {
    let (rect, _) = table_row(ui, row_idx, false);
    let painter = ui.painter();
    let mono = egui::FontId::monospace(12.0);
    let regular = egui::FontId::proportional(12.0);
    let text_color = egui::Color32::from_rgb(226, 229, 230);
    let muted = egui::Color32::from_rgb(150, 154, 156);

    let mut x = rect.left() + TABLE_PAD_X;
    paint_text(
        painter,
        rect,
        x,
        SIZE_COL_WIDTH,
        &format_bytes(group.wasted_bytes),
        cell_text_style(mono.clone(), text_color, false),
    );
    x += SIZE_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        SIZE_COL_WIDTH,
        &format_bytes(group.size),
        cell_text_style(mono.clone(), text_color, false),
    );
    x += SIZE_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        COUNT_COL_WIDTH,
        &format_count(group.count as u64),
        cell_text_style(mono.clone(), muted, false),
    );
    x += COUNT_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        MODIFIED_COL_WIDTH,
        &group.modified_unix.to_string(),
        cell_text_style(mono, muted, false),
    );
    x += MODIFIED_COL_WIDTH;
    paint_text(
        painter,
        rect,
        x,
        (rect.right() - x - TABLE_PAD_X).max(24.0),
        &group.name,
        cell_text_style(regular, text_color, false),
    );
}

fn paint_duplicate_path_row(
    ui: &mut egui::Ui,
    row_idx: usize,
    selected: bool,
    duplicate_path: &DuplicatePath,
) -> egui::Response {
    let (rect, response) = table_row(ui, row_idx, selected);
    let painter = ui.painter();
    let text_color = table_text_color(selected);
    let x = rect.left()
        + TABLE_PAD_X
        + SIZE_COL_WIDTH
        + SIZE_COL_WIDTH
        + COUNT_COL_WIDTH
        + MODIFIED_COL_WIDTH
        + 16.0;
    paint_text(
        painter,
        rect,
        x,
        (rect.right() - x - TABLE_PAD_X).max(24.0),
        &duplicate_path.path_text,
        cell_text_style(egui::FontId::proportional(12.0), text_color, true),
    );

    if response.hovered() {
        response.on_hover_text(&duplicate_path.path_text)
    } else {
        response
    }
}

fn table_row(ui: &mut egui::Ui, row_idx: usize, selected: bool) -> (egui::Rect, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TABLE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            egui::Color32::from_rgb(57, 91, 80)
        } else if response.hovered() {
            egui::Color32::from_rgb(37, 41, 44)
        } else if row_idx.is_multiple_of(2) {
            egui::Color32::from_rgb(28, 31, 34)
        } else {
            egui::Color32::from_rgb(23, 26, 28)
        };
        ui.painter().rect_filled(rect, 0.0, fill);
    }
    (rect, response)
}

fn table_text_color(selected: bool) -> egui::Color32 {
    if selected {
        egui::Color32::from_rgb(245, 249, 247)
    } else {
        egui::Color32::from_rgb(214, 218, 220)
    }
}

fn cell_text_style(font: egui::FontId, color: egui::Color32, middle: bool) -> CellTextStyle {
    CellTextStyle {
        font,
        color,
        middle,
    }
}

fn paint_text(
    painter: &egui::Painter,
    rect: egui::Rect,
    x: f32,
    width: f32,
    text: &str,
    style: CellTextStyle,
) {
    let fitted = if style.middle {
        fit_middle_text(text, width)
    } else {
        fit_end_text(text, width)
    };
    painter.text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        fitted.as_ref(),
        style.font,
        style.color,
    );
}

fn fit_end_text(text: &str, width: f32) -> Cow<'_, str> {
    let budget = text_char_budget(width);
    let len = text.chars().count();
    if len <= budget {
        return Cow::Borrowed(text);
    }
    if budget <= 3 {
        return Cow::Owned(".".repeat(budget));
    }

    let mut output = text.chars().take(budget - 3).collect::<String>();
    output.push_str("...");
    Cow::Owned(output)
}

fn fit_middle_text(text: &str, width: f32) -> Cow<'_, str> {
    let budget = text_char_budget(width);
    let len = text.chars().count();
    if len <= budget {
        return Cow::Borrowed(text);
    }
    if budget <= 8 {
        return fit_end_text(text, width);
    }

    let head_len = (budget - 3) / 2;
    let tail_len = budget - 3 - head_len;
    let mut output = text.chars().take(head_len).collect::<String>();
    output.push_str("...");
    let mut tail = text.chars().rev().take(tail_len).collect::<Vec<_>>();
    tail.reverse();
    output.extend(tail);
    Cow::Owned(output)
}

fn text_char_budget(width: f32) -> usize {
    (width.max(0.0) / 7.2).floor() as usize
}

fn graph_totals(graph: &FileGraph) -> (u64, u64) {
    graph
        .ids()
        .filter_map(|id| {
            let entry = graph.entry(id)?;
            if entry.parent.is_some() {
                return None;
            }
            let stats = graph.stats(id)?;
            Some((stats.total_size.bytes(), stats.total_allocated.bytes()))
        })
        .next()
        .unwrap_or((0, 0))
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit_idx = 0;
    while value >= 1024.0 && unit_idx + 1 < UNITS.len() {
        value /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit_idx])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit_idx])
    } else {
        format!("{value:.2} {}", UNITS[unit_idx])
    }
}

fn format_count(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::with_capacity(text.len() + text.len() / 3);
    for (idx, ch) in text.chars().rev().enumerate() {
        if idx > 0 && idx.is_multiple_of(3) {
            output.push(',');
        }
        output.push(ch);
    }
    output.chars().rev().collect()
}

fn discover_volume_shortcuts() -> Vec<VolumeShortcut> {
    discover_volumes()
        .unwrap_or_default()
        .into_iter()
        .map(|volume| {
            let is_ntfs = volume.kind == VolumeKind::Ntfs;
            let drive = volume.root.trim_end_matches('\\');
            let label = match volume.kind {
                VolumeKind::Ntfs => format!("{drive} NTFS"),
                VolumeKind::Other(name) if !name.is_empty() => format!("{drive} {name}"),
                VolumeKind::Other(_) | VolumeKind::Unknown => drive.to_owned(),
            };
            VolumeShortcut {
                root: volume.root,
                label,
                is_ntfs,
            }
        })
        .collect()
}

fn default_scan_path_from(system_drive: Option<&str>, volumes: &[VolumeShortcut]) -> String {
    if let Some(root) = system_drive.and_then(normalize_drive_root) {
        return volumes
            .iter()
            .find(|volume| volume.root.eq_ignore_ascii_case(&root))
            .map(|volume| volume.root.clone())
            .unwrap_or(root);
    }

    if let Some(volume) = volumes.iter().find(|volume| volume.is_ntfs) {
        return volume.root.clone();
    }

    volumes
        .first()
        .map(|volume| volume.root.clone())
        .unwrap_or_else(|| ".".to_owned())
}

fn normalize_drive_root(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches(['\\', '/']);
    let mut chars = trimmed.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next() != Some(':') || chars.next().is_some() {
        return None;
    }

    Some(format!("{}:\\", letter.to_ascii_uppercase()))
}

#[derive(Debug)]
struct UiScanOutcome {
    graph: FileGraph,
    summary: ScanSummary,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
}

fn scan_path(
    path: PathBuf,
    mode: UiScannerMode,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<UiScanOutcome, String> {
    match mode {
        UiScannerMode::Fallback => scan_fallback(path, None, cancel, on_progress),
        UiScannerMode::Ntfs => scan_ntfs(&path, cancel, on_progress),
        UiScannerMode::Auto => {
            if drive_volume(&path).is_some() {
                match scan_ntfs(&path, cancel, on_progress) {
                    Ok(outcome) => Ok(outcome),
                    Err(error) => scan_fallback(path, Some(error), cancel, on_progress),
                }
            } else {
                scan_fallback(path, None, cancel, on_progress)
            }
        }
    }
}

fn scan_fallback(
    path: PathBuf,
    fallback_reason: Option<String>,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<UiScanOutcome, String> {
    let (graph, summary) = FallbackScanner::scan_with_control(
        ScanOptions {
            root: path,
            follow_symlinks: false,
        },
        UI_PROGRESS_EVERY,
        |summary| {
            on_progress(summary);
            if cancel.load(Ordering::Relaxed) {
                ScanControl::Cancel
            } else {
                ScanControl::Continue
            }
        },
    )
    .map_err(|error| error.to_string())?;

    Ok(UiScanOutcome {
        graph,
        summary,
        scanner_label: "fallback traversal",
        fallback_reason,
    })
}

fn scan_ntfs(
    path: &Path,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<UiScanOutcome, String> {
    let volume = drive_volume(path).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let graph = NtfsScanner::scan_volume_with_control(&volume, UI_PROGRESS_EVERY, |progress| {
        on_progress(scan_summary_from_ntfs_progress(progress));
        if cancel.load(Ordering::Relaxed) {
            NtfsScanControl::Cancel
        } else {
            NtfsScanControl::Continue
        }
    })
    .map_err(|error| error.to_string())?;
    let summary = summary_from_graph(&graph);

    Ok(UiScanOutcome {
        graph,
        summary,
        scanner_label: "direct NTFS MFT",
        fallback_reason: None,
    })
}

fn scan_summary_from_ntfs_progress(progress: NtfsScanProgress) -> ScanSummary {
    ScanSummary {
        entries: progress.entries,
        inaccessible: progress.skipped,
        directories: progress.directories,
        files: progress.files,
    }
}

fn summary_from_graph(graph: &FileGraph) -> ScanSummary {
    let mut summary = ScanSummary {
        entries: graph.len() as u64,
        ..ScanSummary::default()
    };

    for id in graph.ids() {
        let Some(entry) = graph.entry(id) else {
            continue;
        };
        if entry.flags.contains(EntryFlags::DIRECTORY) {
            summary.directories += 1;
        } else {
            summary.files += 1;
        }
    }

    summary
}

fn drive_volume(path: &Path) -> Option<String> {
    let value = path.to_string_lossy();
    let trimmed = value.trim_end_matches(['\\', '/']);
    let mut chars = trimmed.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next()? != ':' || chars.next().is_some() {
        return None;
    }

    Some(format!("{}:", letter.to_ascii_uppercase()))
}

fn maybe_relaunch_ui_scan_elevated(
    path: &Path,
    scanner_mode: UiScannerMode,
) -> Result<bool, String> {
    if !ui_scan_needs_elevation(path, scanner_mode) || !should_request_elevation()? {
        return Ok(false);
    }

    let args = [
        "--path".to_owned(),
        path.to_string_lossy().into_owned(),
        "--scanner".to_owned(),
        scanner_mode.arg_value().to_owned(),
        "--scan".to_owned(),
    ];
    relaunch_current_process_elevated(args)
        .map_err(|error| format!("failed to request administrator access: {error}"))?;
    Ok(true)
}

fn ui_scan_needs_elevation(path: &Path, scanner_mode: UiScannerMode) -> bool {
    scanner_mode != UiScannerMode::Fallback && drive_volume(path).is_some()
}

#[cfg(windows)]
fn should_request_elevation() -> Result<bool, String> {
    is_process_elevated()
        .map(|is_elevated| !is_elevated)
        .map_err(|error| format!("failed to check administrator elevation: {error}"))
}

#[cfg(not(windows))]
fn should_request_elevation() -> Result<bool, String> {
    Ok(false)
}

#[derive(Debug)]
struct FilteredRows {
    matched: usize,
    rows: Vec<ResultRow>,
}

impl FilterInputs {
    fn signature(&self) -> FilterSignature {
        FilterSignature {
            name: self.name.clone(),
            extension: self.extension.clone(),
            path: self.path.clone(),
            min_size: self.min_size.clone(),
            max_size: self.max_size.clone(),
            min_allocated: self.min_allocated.clone(),
            max_allocated: self.max_allocated.clone(),
            modified_after: self.modified_after.clone(),
            modified_before: self.modified_before.clone(),
            regex: self.regex,
            include_directories: self.include_directories,
        }
    }

    fn query_filter(&self) -> Result<QueryFilter, String> {
        let name = matcher_from_input(&self.name, self.regex)?;
        let path = matcher_from_input(&self.path, self.regex)?;
        let extension = trimmed_string(&self.extension);

        Ok(QueryFilter {
            name,
            extension,
            path,
            min_size: parse_optional_u64("Min size", &self.min_size)?,
            max_size: parse_optional_u64("Max size", &self.max_size)?,
            min_allocated: parse_optional_u64("Min allocated", &self.min_allocated)?,
            max_allocated: parse_optional_u64("Max allocated", &self.max_allocated)?,
            modified_after: parse_optional_unix_seconds("Modified after", &self.modified_after)?,
            modified_before: parse_optional_unix_seconds("Modified before", &self.modified_before)?,
            include_directories: self.include_directories,
        })
    }
}

fn filtered_rows_from_graph(
    graph: &FileGraph,
    filters: &FilterInputs,
    limit: usize,
) -> Result<FilteredRows, String> {
    let filter = filters
        .query_filter()?
        .compile()
        .map_err(|error| error.to_string())?;
    let mut matched = 0;
    let ids = top_entries_by_total_size(
        graph,
        filter.matching_ids(graph).inspect(|_| {
            matched += 1;
        }),
        limit,
    );

    let rows = ids
        .into_iter()
        .filter_map(|id| {
            let stats = graph.stats(id)?;
            let entry = graph.entry(id)?;
            let kind = if entry.flags.contains(EntryFlags::DIRECTORY) {
                "dir"
            } else {
                "file"
            };
            let path = graph.reconstruct_path(id)?;
            let path_text = path.display().to_string();
            Some(ResultRow {
                id,
                path_text,
                kind,
                size: stats.total_size.bytes(),
                allocated: stats.total_allocated.bytes(),
                modified_unix: entry.modified_unix,
            })
        })
        .collect();

    Ok(FilteredRows { matched, rows })
}

fn matcher_from_input(value: &str, regex: bool) -> Result<Option<NameMatcher>, String> {
    let Some(trimmed) = trimmed_string(value) else {
        return Ok(None);
    };
    if regex {
        NameMatcher::regex(&trimmed)
            .map(Some)
            .map_err(|error| error.to_string())
    } else {
        Ok(Some(NameMatcher::contains(trimmed)))
    }
}

fn trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn parse_optional_u64(label: &str, value: &str) -> Result<Option<u64>, String> {
    let Some(trimmed) = trimmed_string(value) else {
        return Ok(None);
    };
    trimmed
        .parse()
        .map(Some)
        .map_err(|_| format!("{label} must be a non-negative integer"))
}

fn parse_optional_unix_seconds(label: &str, value: &str) -> Result<Option<i64>, String> {
    let Some(trimmed) = trimmed_string(value) else {
        return Ok(None);
    };
    if let Ok(value) = trimmed.parse() {
        return Ok(Some(value));
    }
    parse_ymd_to_unix_seconds(&trimmed)
        .map(Some)
        .ok_or_else(|| format!("{label} must be Unix seconds or YYYY-MM-DD"))
}

fn parse_ymd_to_unix_seconds(value: &str) -> Option<i64> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    Some(days.saturating_mul(86_400))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }

    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn export_graph_to_csv(
    graph: &FileGraph,
    output_path: &Path,
    include_directories: bool,
) -> Result<(), String> {
    let file = File::create(output_path)
        .map_err(|error| format!("failed to create {}: {error}", output_path.display()))?;
    export_csv(
        graph,
        file,
        CsvExportOptions {
            include_directories,
        },
    )
    .map_err(|error| format!("failed to export {}: {error}", output_path.display()))
}

fn tree_rows_from_graph(graph: &FileGraph, limit: usize) -> Vec<TreeRow> {
    if limit == 0 {
        return Vec::new();
    }

    let mut child_pairs = Vec::with_capacity(graph.len().saturating_sub(1));
    let mut roots = Vec::new();

    for id in graph.ids() {
        let Some(entry) = graph.entry(id) else {
            continue;
        };
        if let Some(parent) = entry.parent {
            child_pairs.push((parent, id));
        } else {
            roots.push(id);
        }
    }

    sort_entry_ids_by_total_size(graph, &mut roots);
    child_pairs.sort_by(|(left_parent, left_child), (right_parent, right_child)| {
        left_parent
            .0
            .cmp(&right_parent.0)
            .then_with(|| compare_entry_ids_by_total_size(graph, left_child, right_child))
    });

    let mut child_ids = Vec::with_capacity(child_pairs.len());
    let mut child_ranges = vec![ChildRange::default(); graph.len()];
    let mut pair_idx = 0;
    while pair_idx < child_pairs.len() {
        let parent = child_pairs[pair_idx].0;
        let start = child_ids.len();
        while pair_idx < child_pairs.len() && child_pairs[pair_idx].0 == parent {
            child_ids.push(child_pairs[pair_idx].1);
            pair_idx += 1;
        }
        child_ranges[parent.0 as usize] = ChildRange {
            start: start as u32,
            len: (child_ids.len() - start) as u32,
        };
    }

    let mut rows = Vec::with_capacity(limit.min(graph.len()));
    for root in roots {
        append_tree_rows(graph, &child_ids, &child_ranges, root, 0, limit, &mut rows);
        if rows.len() >= limit {
            break;
        }
    }

    rows
}

fn append_tree_rows(
    graph: &FileGraph,
    child_ids: &[EntryId],
    child_ranges: &[ChildRange],
    id: EntryId,
    depth: usize,
    limit: usize,
    rows: &mut Vec<TreeRow>,
) {
    if rows.len() >= limit {
        return;
    }

    if let Some(row) = tree_row_from_graph(graph, id, depth) {
        rows.push(row);
    }

    let Some(range) = child_ranges.get(id.0 as usize).copied() else {
        return;
    };
    let start = range.start as usize;
    let end = start
        .saturating_add(range.len as usize)
        .min(child_ids.len());
    for child in &child_ids[start..end] {
        append_tree_rows(
            graph,
            child_ids,
            child_ranges,
            *child,
            depth + 1,
            limit,
            rows,
        );
        if rows.len() >= limit {
            break;
        }
    }
}

fn tree_row_from_graph(graph: &FileGraph, id: EntryId, depth: usize) -> Option<TreeRow> {
    let stats = graph.stats(id)?;
    let entry = graph.entry(id)?;
    let kind = if entry.flags.contains(EntryFlags::DIRECTORY) {
        "dir"
    } else if entry.flags.contains(EntryFlags::SYMLINK) {
        "link"
    } else {
        "file"
    };

    Some(TreeRow {
        id,
        depth,
        kind,
        size: stats.total_size.bytes(),
        allocated: stats.total_allocated.bytes(),
    })
}

fn sort_entry_ids_by_total_size(graph: &FileGraph, ids: &mut [EntryId]) {
    ids.sort_by(|left, right| compare_entry_ids_by_total_size(graph, left, right));
}

fn compare_entry_ids_by_total_size(
    graph: &FileGraph,
    left: &EntryId,
    right: &EntryId,
) -> std::cmp::Ordering {
    let left_size = graph
        .stats(*left)
        .map_or(0, |stats| stats.total_size.bytes());
    let right_size = graph
        .stats(*right)
        .map_or(0, |stats| stats.total_size.bytes());
    right_size.cmp(&left_size).then_with(|| {
        let left_name = graph.name(*left).unwrap_or_default();
        let right_name = graph.name(*right).unwrap_or_default();
        left_name.cmp(right_name)
    })
}

fn treemap_items_from_graph(graph: &FileGraph, limit: usize) -> Vec<TreemapItem> {
    let ids = top_entries_by_own_size(
        graph,
        graph.ids().filter(|id| {
            let Some(stats) = graph.stats(*id) else {
                return false;
            };
            let Some(entry) = graph.entry(*id) else {
                return false;
            };
            !entry.flags.contains(EntryFlags::DIRECTORY) && stats.own_size.bytes() > 0
        }),
        limit,
    );

    ids.into_iter()
        .filter_map(|id| {
            let stats = graph.stats(id)?;
            let entry = graph.entry(id)?;
            if entry.flags.contains(EntryFlags::DIRECTORY) || stats.own_size.bytes() == 0 {
                return None;
            }
            Some(TreemapItem {
                id,
                label: graph.name(id).unwrap_or_default().to_owned(),
                size: stats.own_size.bytes(),
            })
        })
        .collect()
}

fn duplicate_groups_from_graph(
    graph: &FileGraph,
    group_limit: usize,
    path_limit: usize,
) -> Vec<DuplicateGroup> {
    let mut candidates = find_duplicate_candidates(graph);
    candidates.sort_by(|left, right| {
        duplicate_wasted_bytes(right)
            .cmp(&duplicate_wasted_bytes(left))
            .then_with(|| right.size.cmp(&left.size))
            .then_with(|| left.name.cmp(&right.name))
    });

    candidates
        .into_iter()
        .take(group_limit)
        .map(|candidate| duplicate_group_from_candidate(graph, candidate, path_limit))
        .collect()
}

fn duplicate_group_from_candidate(
    graph: &FileGraph,
    candidate: DuplicateCandidate,
    path_limit: usize,
) -> DuplicateGroup {
    let wasted_bytes = duplicate_wasted_bytes(&candidate);
    let paths = candidate
        .entries
        .iter()
        .take(path_limit)
        .filter_map(|id| {
            let path = graph.reconstruct_path(*id)?;
            let path_text = path.display().to_string();
            Some(DuplicatePath { id: *id, path_text })
        })
        .collect();

    DuplicateGroup {
        name: candidate.name,
        size: candidate.size,
        modified_unix: candidate.modified_unix,
        count: candidate.entries.len(),
        wasted_bytes,
        paths,
    }
}

fn duplicate_wasted_bytes(candidate: &DuplicateCandidate) -> u64 {
    candidate
        .size
        .saturating_mul(candidate.entries.len().saturating_sub(1) as u64)
}

fn paint_treemap_rect(painter: &egui::Painter, item: &TreemapRect, idx: usize) {
    let bounds = egui::Rect::from_min_size(
        egui::pos2(item.bounds.x, item.bounds.y),
        egui::vec2(item.bounds.width.max(0.0), item.bounds.height.max(0.0)),
    )
    .shrink(1.0);
    if bounds.width() <= 1.0 || bounds.height() <= 1.0 {
        return;
    }

    let palette = [
        egui::Color32::from_rgb(86, 137, 111),
        egui::Color32::from_rgb(130, 122, 193),
        egui::Color32::from_rgb(196, 138, 79),
        egui::Color32::from_rgb(79, 148, 189),
        egui::Color32::from_rgb(177, 95, 112),
    ];
    painter.rect_filled(bounds, 2.0, palette[idx % palette.len()]);
    painter.rect_stroke(
        bounds,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(24, 26, 28)),
        egui::StrokeKind::Inside,
    );

    if bounds.width() > 72.0 && bounds.height() > 28.0 {
        painter.text(
            bounds.left_top() + egui::vec2(6.0, 5.0),
            egui::Align2::LEFT_TOP,
            &item.label,
            egui::FontId::monospace(11.0),
            egui::Color32::from_rgb(246, 246, 238),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use diskloom_core::{FileGraph, FileGraphBuilder, FileKind};

    use super::{
        FilterInputs, VolumeShortcut, default_scan_path_from, duplicate_groups_from_graph,
        export_graph_to_csv, filtered_rows_from_graph, format_bytes, format_count,
        normalize_drive_root, parse_optional_u64, parse_optional_unix_seconds,
        tree_rows_from_graph, ui_scan_needs_elevation,
    };

    fn sample_graph() -> FileGraph {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(
                Some(root),
                "trace.log",
                FileKind::File,
                40,
                64,
                1_704_067_200,
            )
            .unwrap();
        builder
            .add_entry(
                Some(root),
                "notes.txt",
                FileKind::File,
                10,
                16,
                1_672_531_200,
            )
            .unwrap();
        builder.finish()
    }

    #[test]
    fn filtered_rows_should_apply_extension_and_size() {
        let graph = sample_graph();
        let filters = FilterInputs {
            extension: "log".to_owned(),
            min_size: "20".to_owned(),
            include_directories: false,
            ..FilterInputs::default()
        };

        let rows = filtered_rows_from_graph(&graph, &filters, 10).unwrap();

        assert_eq!(rows.matched, 1);
        assert_eq!(rows.rows[0].path_text, "root\\trace.log");
    }

    #[test]
    fn filtered_rows_should_apply_upper_bounds_and_modified_dates() {
        let graph = sample_graph();
        let filters = FilterInputs {
            max_size: "20".to_owned(),
            modified_after: "2023-01-01".to_owned(),
            modified_before: "2023-12-31".to_owned(),
            include_directories: false,
            ..FilterInputs::default()
        };

        let rows = filtered_rows_from_graph(&graph, &filters, 10).unwrap();

        assert_eq!(rows.matched, 1);
        assert_eq!(rows.rows[0].path_text, "root\\notes.txt");
    }

    #[test]
    fn parse_optional_u64_should_accept_empty_or_integer_values() {
        assert_eq!(parse_optional_u64("Min size", "").unwrap(), None);
        assert_eq!(parse_optional_u64("Min size", "42").unwrap(), Some(42));
        assert!(parse_optional_u64("Min size", "abc").is_err());
    }

    #[test]
    fn format_bytes_should_scale_binary_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
    }

    #[test]
    fn format_count_should_group_thousands() {
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn normalize_drive_root_should_accept_drive_roots() {
        assert_eq!(normalize_drive_root("c:").as_deref(), Some("C:\\"));
        assert_eq!(normalize_drive_root("d:\\").as_deref(), Some("D:\\"));
        assert_eq!(normalize_drive_root("e:/").as_deref(), Some("E:\\"));
        assert_eq!(normalize_drive_root("c:\\Users"), None);
    }

    #[test]
    fn default_scan_path_should_prefer_system_drive() {
        let volumes = [
            volume("D:\\", false),
            volume("C:\\", true),
            volume("E:\\", true),
        ];

        assert_eq!(default_scan_path_from(Some("c:"), &volumes), "C:\\");
    }

    #[test]
    fn default_scan_path_should_fallback_to_first_ntfs_volume() {
        let volumes = [volume("D:\\", false), volume("E:\\", true)];

        assert_eq!(default_scan_path_from(None, &volumes), "E:\\");
    }

    #[test]
    fn parse_optional_unix_seconds_should_accept_integer_or_date() {
        assert_eq!(parse_optional_unix_seconds("Modified", "").unwrap(), None);
        assert_eq!(
            parse_optional_unix_seconds("Modified", "1704067200").unwrap(),
            Some(1_704_067_200)
        );
        assert_eq!(
            parse_optional_unix_seconds("Modified", "2024-01-01").unwrap(),
            Some(1_704_067_200)
        );
        assert!(parse_optional_unix_seconds("Modified", "2024-02-31").is_err());
    }

    #[test]
    fn launch_args_should_restore_scan_request() {
        let app = super::DiskLoomApp::from_launch_args([
            "--path".to_owned(),
            "C:\\".to_owned(),
            "--scanner".to_owned(),
            "ntfs".to_owned(),
            "--scan".to_owned(),
        ]);

        assert_eq!(app.path, "C:\\");
        assert_eq!(app.scanner_mode, super::UiScannerMode::Ntfs);
        assert!(app.start_on_launch);
    }

    #[test]
    fn ui_scan_needs_elevation_should_match_direct_drive_scans_only() {
        assert!(ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            super::UiScannerMode::Auto
        ));
        assert!(ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            super::UiScannerMode::Ntfs
        ));
        assert!(!ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            super::UiScannerMode::Fallback
        ));
        assert!(!ui_scan_needs_elevation(
            std::path::Path::new("C:\\Users"),
            super::UiScannerMode::Auto
        ));
    }

    #[test]
    fn export_graph_to_csv_should_write_scan_result() {
        let graph = sample_graph();
        let output_path = std::env::temp_dir().join(format!(
            "diskloom-ui-export-{}-{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        export_graph_to_csv(&graph, &output_path, false).unwrap();
        let output = fs::read_to_string(&output_path).unwrap();
        fs::remove_file(output_path).unwrap();

        assert!(output.contains("trace.log"));
        assert!(!output.contains("directory"));
    }

    #[test]
    fn duplicate_groups_should_order_by_wasted_bytes() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "small.bin", FileKind::File, 10, 10, 100)
            .unwrap();
        builder
            .add_entry(Some(root), "SMALL.bin", FileKind::File, 10, 10, 100)
            .unwrap();
        builder
            .add_entry(Some(root), "large.bin", FileKind::File, 100, 100, 100)
            .unwrap();
        builder
            .add_entry(Some(root), "LARGE.bin", FileKind::File, 100, 100, 100)
            .unwrap();
        let graph = builder.finish();

        let groups = duplicate_groups_from_graph(&graph, 10, 10);

        assert_eq!(groups[0].name, "large.bin");
        assert_eq!(groups[0].wasted_bytes, 100);
        assert_eq!(groups[0].paths.len(), 2);
    }

    #[test]
    fn tree_rows_should_sort_children_by_total_size() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let big = builder
            .add_entry(Some(root), "big", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(big), "large.bin", FileKind::File, 100, 128, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "small.bin", FileKind::File, 10, 16, 0)
            .unwrap();
        let graph = builder.finish();

        let rows = tree_rows_from_graph(&graph, 10);

        assert_eq!(graph.name(rows[0].id), Some("root"));
        assert_eq!(graph.name(rows[1].id), Some("big"));
        assert_eq!(rows[1].depth, 1);
        assert_eq!(graph.name(rows[2].id), Some("large.bin"));
        assert_eq!(rows[2].depth, 2);
        assert_eq!(graph.name(rows[3].id), Some("small.bin"));
    }

    #[test]
    fn tree_rows_should_respect_limit() {
        let graph = sample_graph();

        let rows = tree_rows_from_graph(&graph, 2);

        assert_eq!(rows.len(), 2);
    }

    fn volume(root: &str, is_ntfs: bool) -> VolumeShortcut {
        VolumeShortcut {
            root: root.to_owned(),
            label: root.to_owned(),
            is_ntfs,
        }
    }
}
