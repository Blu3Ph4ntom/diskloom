use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
    time::Instant,
};

use diskloom_core::{EntryFlags, EntryId, FileGraph};
use diskloom_ntfs::NtfsScanner;
use diskloom_query::{
    FileTypeStat, NameMatcher, QueryFilter, SortKey, SortOrder, TreemapBounds, TreemapItem,
    TreemapRect, file_type_stats, layout_treemap, sort_entries,
};
use diskloom_scan::{FallbackScanner, ScanOptions, ScanSummary};
use diskloom_windows::{open_in_explorer, recycle_delete, rename_path, show_properties};

const UI_PROGRESS_EVERY: u64 = 1_024;
const TREE_ROW_LIMIT: usize = 500;

#[derive(Debug)]
pub struct DiskLoomApp {
    path: String,
    scanner_mode: UiScannerMode,
    filters: FilterInputs,
    view_cache: Option<ViewCache>,
    selected_path: Option<PathBuf>,
    rename_target: String,
    action_status: Option<ActionStatus>,
    active_tab: ActiveTab,
    state: UiState,
    receiver: Option<Receiver<ScanMessage>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Tree,
    Files,
    Types,
    Treemap,
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

#[derive(Debug, Clone, Copy)]
struct UiScanProgress {
    summary: ScanSummary,
    elapsed_ms: u128,
}

#[derive(Debug)]
struct ScanResult {
    graph: FileGraph,
    summary: ScanSummary,
    elapsed_ms: u128,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
    tree_rows: Vec<TreeRow>,
    file_types: Vec<FileTypeStat>,
    treemap_items: Vec<TreemapItem>,
}

#[derive(Debug, Clone)]
struct TreeRow {
    path: PathBuf,
    path_text: String,
    name: String,
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
    path: PathBuf,
    path_text: String,
    kind: &'static str,
    size: u64,
    allocated: u64,
}

#[derive(Debug, Clone, Default)]
struct FilterInputs {
    name: String,
    extension: String,
    path: String,
    min_size: String,
    min_allocated: String,
    regex: bool,
    include_directories: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilterSignature {
    name: String,
    extension: String,
    path: String,
    min_size: String,
    min_allocated: String,
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
        Self {
            path: ".".to_owned(),
            scanner_mode: UiScannerMode::Auto,
            filters: FilterInputs {
                include_directories: true,
                ..FilterInputs::default()
            },
            view_cache: None,
            selected_path: None,
            rename_target: String::new(),
            action_status: None,
            active_tab: ActiveTab::Tree,
            state: UiState::Idle,
            receiver: None,
        }
    }
}

impl eframe::App for DiskLoomApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        ctx.set_visuals(egui::Visuals::dark());
        self.receive_scan();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered_justified(|ui| {
                ui.heading("DiskLoom");
            });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label("Path");
                let input_width = (ui.available_width() - 96.0).max(120.0);
                ui.add_sized(
                    [input_width, 24.0],
                    egui::TextEdit::singleline(&mut self.path),
                );
                let scanning = matches!(self.state, UiState::Scanning(_));
                if ui
                    .add_enabled(!scanning, egui::Button::new("Scan"))
                    .clicked()
                {
                    self.start_scan();
                }
            });

            ui.add_space(8.0);
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

            ui.add_space(8.0);
            self.filter_controls(ui);

            ui.add_space(8.0);
            self.action_controls(ui);

            ui.add_space(8.0);
            self.status_line(ui);
            ui.separator();
            self.tabs(ui);
            ui.separator();
            self.active_view(ui);
        });

        if matches!(self.state, UiState::Scanning(_)) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

impl DiskLoomApp {
    fn start_scan(&mut self) {
        let trimmed = self.path.trim();
        let path = PathBuf::from(if trimmed.is_empty() { "." } else { trimmed });
        let scanner_mode = self.scanner_mode;
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.state = UiState::Scanning(None);
        self.view_cache = None;
        self.selected_path = None;
        self.rename_target.clear();
        self.action_status = None;

        thread::spawn(move || {
            let started = Instant::now();
            let progress_sender = sender.clone();
            let mut on_progress = |summary| {
                let _ = progress_sender.send(ScanMessage::Progress(UiScanProgress {
                    summary,
                    elapsed_ms: started.elapsed().as_millis(),
                }));
            };
            let result = scan_path(path, scanner_mode, &mut on_progress).map(|outcome| {
                let tree_rows = tree_rows_from_graph(&outcome.graph, TREE_ROW_LIMIT);
                let file_types = file_type_stats(&outcome.graph, 50);
                let treemap_items = treemap_items_from_graph(&outcome.graph, 120);
                ScanResult {
                    graph: outcome.graph,
                    summary: outcome.summary,
                    elapsed_ms: started.elapsed().as_millis(),
                    scanner_label: outcome.scanner_label,
                    fallback_reason: outcome.fallback_reason,
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
                    self.view_cache = None;
                    self.selected_path = None;
                    self.rename_target.clear();
                    keep_receiver = false;
                    break;
                }
                Ok(ScanMessage::Error(error)) => {
                    self.state = UiState::Error(error);
                    self.view_cache = None;
                    self.selected_path = None;
                    self.rename_target.clear();
                    keep_receiver = false;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.state = UiState::Error("scan worker stopped".to_owned());
                    self.view_cache = None;
                    self.selected_path = None;
                    self.rename_target.clear();
                    keep_receiver = false;
                    break;
                }
            }
        }

        if keep_receiver {
            self.receiver = Some(receiver);
        }
    }

    fn filter_controls(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label("Search");
            changed |= ui
                .add_sized(
                    [180.0, 24.0],
                    egui::TextEdit::singleline(&mut self.filters.name),
                )
                .changed();
            changed |= ui.checkbox(&mut self.filters.regex, "Regex").changed();
            ui.label("Ext");
            changed |= ui
                .add_sized(
                    [80.0, 24.0],
                    egui::TextEdit::singleline(&mut self.filters.extension),
                )
                .changed();
            ui.label("Path");
            changed |= ui
                .add_sized(
                    [180.0, 24.0],
                    egui::TextEdit::singleline(&mut self.filters.path),
                )
                .changed();
        });

        ui.horizontal(|ui| {
            ui.label("Min size");
            changed |= ui
                .add_sized(
                    [100.0, 24.0],
                    egui::TextEdit::singleline(&mut self.filters.min_size),
                )
                .changed();
            ui.label("Min allocated");
            changed |= ui
                .add_sized(
                    [100.0, 24.0],
                    egui::TextEdit::singleline(&mut self.filters.min_allocated),
                )
                .changed();
            changed |= ui
                .checkbox(&mut self.filters.include_directories, "Dirs")
                .changed();
        });

        if changed {
            self.view_cache = None;
        }
    }

    fn action_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Selected");
            let selected = self
                .selected_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            let mut selected_text = selected;
            ui.add_enabled(
                false,
                egui::TextEdit::singleline(&mut selected_text).desired_width(240.0),
            );

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

    fn status_line(&self, ui: &mut egui::Ui) {
        match &self.state {
            UiState::Idle => {
                ui.label("See your disk clearly.");
            }
            UiState::Scanning(progress) => {
                ui.spinner();
                if let Some(progress) = progress {
                    ui.label(format!(
                        "Scanning: {} entries, {} files, {} directories, {} inaccessible, {} ms",
                        progress.summary.entries,
                        progress.summary.files,
                        progress.summary.directories,
                        progress.summary.inaccessible,
                        progress.elapsed_ms
                    ));
                } else {
                    ui.label("Scanning");
                }
            }
            UiState::Complete(result) => {
                ui.label(format!(
                    "{} entries, {} files, {} directories, {} inaccessible, {} ms, {}",
                    result.summary.entries,
                    result.summary.files,
                    result.summary.directories,
                    result.summary.inaccessible,
                    result.elapsed_ms,
                    result.scanner_label
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

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.active_tab, ActiveTab::Tree, "Tree");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Files, "Files");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Types, "Types");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Treemap, "Treemap");
        });
    }

    fn active_view(&mut self, ui: &mut egui::Ui) {
        match self.active_tab {
            ActiveTab::Tree => self.tree(ui),
            ActiveTab::Files => self.results(ui),
            ActiveTab::Types => self.type_stats(ui),
            ActiveTab::Treemap => self.treemap(ui),
        }
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        let (graph_len, rows) = match &self.state {
            UiState::Complete(result) => (result.graph.len(), result.tree_rows.clone()),
            _ => return,
        };

        ui.label(format!("Showing {} of {} entries", rows.len(), graph_len));

        egui::ScrollArea::both()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                egui::Grid::new("tree_grid")
                    .striped(true)
                    .min_col_width(80.0)
                    .show(ui, |ui| {
                        ui.strong("Size");
                        ui.strong("Allocated");
                        ui.strong("Kind");
                        ui.strong("Name");
                        ui.end_row();

                        for row in &rows {
                            ui.monospace(row.size.to_string());
                            ui.monospace(row.allocated.to_string());
                            ui.label(row.kind);
                            ui.horizontal(|ui| {
                                ui.add_space((row.depth as f32 * 14.0).min(180.0));
                                let selected = self.selected_path.as_ref() == Some(&row.path);
                                let response = ui
                                    .selectable_label(selected, &row.name)
                                    .on_hover_text(&row.path_text);
                                if response.clicked() {
                                    self.select_path(row.path.clone());
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
    }

    fn results(&mut self, ui: &mut egui::Ui) {
        let UiState::Complete(result) = &self.state else {
            return;
        };
        let graph_len = result.graph.len();
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

        egui::ScrollArea::both()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                egui::Grid::new("results_grid")
                    .striped(true)
                    .min_col_width(80.0)
                    .show(ui, |ui| {
                        ui.strong("Size");
                        ui.strong("Allocated");
                        ui.strong("Kind");
                        ui.strong("Path");
                        ui.end_row();

                        for row in &rows {
                            ui.monospace(row.size.to_string());
                            ui.monospace(row.allocated.to_string());
                            ui.label(row.kind);
                            let selected = self.selected_path.as_ref() == Some(&row.path);
                            if ui.selectable_label(selected, &row.path_text).clicked() {
                                self.select_path(row.path.clone());
                            }
                            ui.end_row();
                        }
                    });
            });
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
                        ui.monospace(stat.size.to_string());
                        ui.monospace(stat.allocated.to_string());
                        ui.monospace(stat.files.to_string());
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

    fn select_path(&mut self, path: PathBuf) {
        self.rename_target = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
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
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<UiScanOutcome, String> {
    match mode {
        UiScannerMode::Fallback => scan_fallback(path, None, on_progress),
        UiScannerMode::Ntfs => scan_ntfs(&path),
        UiScannerMode::Auto => {
            if drive_volume(&path).is_some() {
                match scan_ntfs(&path) {
                    Ok(outcome) => Ok(outcome),
                    Err(error) => scan_fallback(path, Some(error), on_progress),
                }
            } else {
                scan_fallback(path, None, on_progress)
            }
        }
    }
}

fn scan_fallback(
    path: PathBuf,
    fallback_reason: Option<String>,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<UiScanOutcome, String> {
    let (graph, summary) = FallbackScanner::scan_with_progress(
        ScanOptions {
            root: path,
            follow_symlinks: false,
        },
        UI_PROGRESS_EVERY,
        on_progress,
    )
    .map_err(|error| error.to_string())?;

    Ok(UiScanOutcome {
        graph,
        summary,
        scanner_label: "fallback traversal",
        fallback_reason,
    })
}

fn scan_ntfs(path: &Path) -> Result<UiScanOutcome, String> {
    let volume = drive_volume(path).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let graph = NtfsScanner::scan_volume(&volume).map_err(|error| error.to_string())?;
    let summary = summary_from_graph(&graph);

    Ok(UiScanOutcome {
        graph,
        summary,
        scanner_label: "direct NTFS MFT",
        fallback_reason: None,
    })
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
            min_allocated: self.min_allocated.clone(),
            regex: self.regex,
            include_directories: self.include_directories,
        }
    }

    fn query_filter(&self) -> Result<QueryFilter, String> {
        let name = matcher_from_input(&self.name, self.regex)?;
        let path = matcher_from_input(&self.path, false)?;
        let extension = trimmed_string(&self.extension);

        Ok(QueryFilter {
            name,
            extension,
            path,
            min_size: parse_optional_u64("Min size", &self.min_size)?,
            max_size: None,
            min_allocated: parse_optional_u64("Min allocated", &self.min_allocated)?,
            max_allocated: None,
            modified_after: None,
            modified_before: None,
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
    let mut ids: Vec<_> = filter.matching_ids(graph).collect();
    let matched = ids.len();
    sort_entries(graph, &mut ids, SortKey::Size, SortOrder::Descending);

    let rows = ids
        .into_iter()
        .take(limit)
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
                path,
                path_text,
                kind,
                size: stats.total_size.bytes(),
                allocated: stats.total_allocated.bytes(),
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
    let name = graph.name(id)?.to_owned();
    let path = graph.reconstruct_path(id)?;
    let path_text = path.display().to_string();
    let kind = if entry.flags.contains(EntryFlags::DIRECTORY) {
        "dir"
    } else if entry.flags.contains(EntryFlags::SYMLINK) {
        "link"
    } else {
        "file"
    };

    Some(TreeRow {
        path,
        path_text,
        name,
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
    let mut ids: Vec<_> = graph.ids().collect();
    sort_entries(graph, &mut ids, SortKey::Size, SortOrder::Descending);

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
        .take(limit)
        .collect()
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
    use diskloom_core::{FileGraph, FileGraphBuilder, FileKind};

    use super::{FilterInputs, filtered_rows_from_graph, parse_optional_u64, tree_rows_from_graph};

    fn sample_graph() -> FileGraph {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "trace.log", FileKind::File, 40, 64, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "notes.txt", FileKind::File, 10, 16, 0)
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
    fn parse_optional_u64_should_accept_empty_or_integer_values() {
        assert_eq!(parse_optional_u64("Min size", "").unwrap(), None);
        assert_eq!(parse_optional_u64("Min size", "42").unwrap(), Some(42));
        assert!(parse_optional_u64("Min size", "abc").is_err());
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

        assert_eq!(rows[0].name, "root");
        assert_eq!(rows[1].name, "big");
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].name, "large.bin");
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[3].name, "small.bin");
    }

    #[test]
    fn tree_rows_should_respect_limit() {
        let graph = sample_graph();

        let rows = tree_rows_from_graph(&graph, 2);

        assert_eq!(rows.len(), 2);
    }
}
