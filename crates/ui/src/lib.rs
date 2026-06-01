use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
    time::Instant,
};

use diskloom_core::{EntryFlags, FileGraph};
use diskloom_ntfs::NtfsScanner;
use diskloom_query::{
    FileTypeStat, SortKey, SortOrder, TreemapBounds, TreemapItem, TreemapRect, file_type_stats,
    layout_treemap, sort_entries,
};
use diskloom_scan::{FallbackScanner, ScanOptions, ScanSummary};

#[derive(Debug)]
pub struct DiskLoomApp {
    path: String,
    scanner_mode: UiScannerMode,
    active_tab: ActiveTab,
    state: UiState,
    receiver: Option<Receiver<ScanMessage>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
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
    Scanning,
    Complete(ScanResult),
    Error(String),
}

#[derive(Debug)]
enum ScanMessage {
    Complete(ScanResult),
    Error(String),
}

#[derive(Debug)]
struct ScanResult {
    summary: ScanSummary,
    elapsed_ms: u128,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
    rows: Vec<ResultRow>,
    file_types: Vec<FileTypeStat>,
    treemap_items: Vec<TreemapItem>,
}

#[derive(Debug)]
struct ResultRow {
    path: String,
    kind: &'static str,
    size: u64,
    allocated: u64,
}

impl Default for DiskLoomApp {
    fn default() -> Self {
        Self {
            path: ".".to_owned(),
            scanner_mode: UiScannerMode::Auto,
            active_tab: ActiveTab::Files,
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
                let scanning = matches!(self.state, UiState::Scanning);
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
            self.status_line(ui);
            ui.separator();
            self.tabs(ui);
            ui.separator();
            self.active_view(ui);
        });

        if matches!(self.state, UiState::Scanning) {
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
        self.state = UiState::Scanning;

        thread::spawn(move || {
            let started = Instant::now();
            let result = scan_path(path, scanner_mode).map(|outcome| ScanResult {
                summary: outcome.summary,
                elapsed_ms: started.elapsed().as_millis(),
                scanner_label: outcome.scanner_label,
                fallback_reason: outcome.fallback_reason,
                rows: rows_from_graph(&outcome.graph, 500),
                file_types: file_type_stats(&outcome.graph, 50),
                treemap_items: treemap_items_from_graph(&outcome.graph, 120),
            });

            let message = match result {
                Ok(result) => ScanMessage::Complete(result),
                Err(error) => ScanMessage::Error(error),
            };
            let _ = sender.send(message);
        });
    }

    fn receive_scan(&mut self) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };

        match receiver.try_recv() {
            Ok(ScanMessage::Complete(result)) => {
                self.state = UiState::Complete(result);
            }
            Ok(ScanMessage::Error(error)) => {
                self.state = UiState::Error(error);
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.receiver = Some(receiver);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.state = UiState::Error("scan worker stopped".to_owned());
            }
        }
    }

    fn status_line(&self, ui: &mut egui::Ui) {
        match &self.state {
            UiState::Idle => {
                ui.label("See your disk clearly.");
            }
            UiState::Scanning => {
                ui.spinner();
                ui.label("Scanning");
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
            ui.selectable_value(&mut self.active_tab, ActiveTab::Files, "Files");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Types, "Types");
            ui.selectable_value(&mut self.active_tab, ActiveTab::Treemap, "Treemap");
        });
    }

    fn active_view(&self, ui: &mut egui::Ui) {
        match self.active_tab {
            ActiveTab::Files => self.results(ui),
            ActiveTab::Types => self.type_stats(ui),
            ActiveTab::Treemap => self.treemap(ui),
        }
    }

    fn results(&self, ui: &mut egui::Ui) {
        let UiState::Complete(result) = &self.state else {
            return;
        };

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

                        for row in &result.rows {
                            ui.monospace(row.size.to_string());
                            ui.monospace(row.allocated.to_string());
                            ui.label(row.kind);
                            ui.label(&row.path);
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
}

#[derive(Debug)]
struct UiScanOutcome {
    graph: FileGraph,
    summary: ScanSummary,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
}

fn scan_path(path: PathBuf, mode: UiScannerMode) -> Result<UiScanOutcome, String> {
    match mode {
        UiScannerMode::Fallback => scan_fallback(path, None),
        UiScannerMode::Ntfs => scan_ntfs(&path),
        UiScannerMode::Auto => {
            if drive_volume(&path).is_some() {
                match scan_ntfs(&path) {
                    Ok(outcome) => Ok(outcome),
                    Err(error) => scan_fallback(path, Some(error)),
                }
            } else {
                scan_fallback(path, None)
            }
        }
    }
}

fn scan_fallback(path: PathBuf, fallback_reason: Option<String>) -> Result<UiScanOutcome, String> {
    let (graph, summary) = FallbackScanner::scan(ScanOptions {
        root: path,
        follow_symlinks: false,
    })
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

fn rows_from_graph(graph: &FileGraph, limit: usize) -> Vec<ResultRow> {
    let mut ids: Vec<_> = graph.ids().collect();
    sort_entries(graph, &mut ids, SortKey::Size, SortOrder::Descending);

    ids.into_iter()
        .take(limit)
        .filter_map(|id| {
            let stats = graph.stats(id)?;
            let entry = graph.entry(id)?;
            let kind = if entry.flags.contains(EntryFlags::DIRECTORY) {
                "dir"
            } else {
                "file"
            };
            let path = graph.reconstruct_path(id)?.display().to_string();
            Some(ResultRow {
                path,
                kind,
                size: stats.total_size.bytes(),
                allocated: stats.total_allocated.bytes(),
            })
        })
        .collect()
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
