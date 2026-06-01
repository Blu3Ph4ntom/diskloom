use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
    time::Instant,
};

use diskloom_core::{EntryFlags, FileGraph};
use diskloom_query::{
    FileTypeStat, SortKey, SortOrder, TreemapBounds, TreemapItem, TreemapRect, file_type_stats,
    layout_treemap, sort_entries,
};
use diskloom_scan::{FallbackScanner, ScanOptions, ScanSummary};

#[derive(Debug)]
pub struct DiskLoomApp {
    path: String,
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
        let path = PathBuf::from(self.path.trim());
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.state = UiState::Scanning;

        thread::spawn(move || {
            let started = Instant::now();
            let result = FallbackScanner::scan(ScanOptions {
                root: path,
                follow_symlinks: false,
            })
            .map(|(graph, summary)| ScanResult {
                summary,
                elapsed_ms: started.elapsed().as_millis(),
                rows: rows_from_graph(&graph, 500),
                file_types: file_type_stats(&graph, 50),
                treemap_items: treemap_items_from_graph(&graph, 120),
            });

            let message = match result {
                Ok(result) => ScanMessage::Complete(result),
                Err(error) => ScanMessage::Error(error.to_string()),
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
                    "{} entries, {} files, {} directories, {} inaccessible, {} ms",
                    result.summary.entries,
                    result.summary.files,
                    result.summary.directories,
                    result.summary.inaccessible,
                    result.elapsed_ms
                ));
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
