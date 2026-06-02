use std::{
    cmp::Ordering as CmpOrdering,
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

use anyhow::Result;
use diskloom_core::{EntryFlags, EntryId, FileGraph};
use diskloom_ntfs::{NtfsScanControl, NtfsScanProgress, NtfsScanner};
use diskloom_scan::{FallbackScanner, ScanControl, ScanOptions, ScanSummary};
use diskloom_windows::{
    VolumeKind, discover_volumes, is_process_elevated, relaunch_current_process_elevated,
};
use slint::{ComponentHandle, SharedString, VecModel};

slint::include_modules!();

const UI_PROGRESS_EVERY: u64 = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiScannerMode {
    Auto,
    Ntfs,
    Fallback,
}

impl UiScannerMode {
    fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Ntfs,
            2 => Self::Fallback,
            _ => Self::Auto,
        }
    }

    fn index(self) -> i32 {
        match self {
            Self::Auto => 0,
            Self::Ntfs => 1,
            Self::Fallback => 2,
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
struct AppState {
    graph: Option<Arc<FileGraph>>,
    index: Option<TreeIndex>,
    expanded: HashSet<EntryId>,
    selected: Option<EntryId>,
    cancel: Option<Arc<AtomicBool>>,
    scanner_mode: UiScannerMode,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            graph: None,
            index: None,
            expanded: HashSet::new(),
            selected: None,
            cancel: None,
            scanner_mode: UiScannerMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ChildRange {
    start: u32,
    len: u32,
}

#[derive(Debug)]
struct TreeIndex {
    roots: Vec<EntryId>,
    child_ids: Vec<EntryId>,
    child_ranges: Vec<ChildRange>,
}

impl TreeIndex {
    fn build(graph: &FileGraph) -> Self {
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

        Self {
            roots,
            child_ids,
            child_ranges,
        }
    }

    fn children(&self, id: EntryId) -> &[EntryId] {
        let Some(range) = self.child_ranges.get(id.0 as usize).copied() else {
            return &[];
        };
        let start = range.start as usize;
        let end = start
            .saturating_add(range.len as usize)
            .min(self.child_ids.len());
        &self.child_ids[start..end]
    }

    fn child_count(&self, id: EntryId) -> usize {
        self.child_ranges
            .get(id.0 as usize)
            .map_or(0, |range| range.len as usize)
    }
}

#[derive(Debug, Clone)]
struct VolumeShortcut {
    root: String,
    label: String,
    is_ntfs: bool,
}

#[derive(Debug)]
struct StartupArgs {
    path: Option<String>,
    scanner_mode: UiScannerMode,
    scan: bool,
}

#[derive(Debug)]
struct UiScanOutcome {
    graph: FileGraph,
    summary: ScanSummary,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
}

pub fn run_from_env_args() -> Result<()> {
    let startup = parse_startup_args(std::env::args().skip(1));
    let volumes = discover_volume_shortcuts();
    let system_drive = std::env::var("SystemDrive").ok();
    let scan_path = startup
        .path
        .clone()
        .unwrap_or_else(|| default_scan_path_from(system_drive.as_deref(), &volumes));

    let ui = AppWindow::new()?;
    ui.set_scan_path(scan_path.into());
    ui.set_scanner_mode(startup.scanner_mode.index());
    ui.set_drives(VecModel::from_slice(&drive_items_from_shortcuts(&volumes)));
    ui.set_rows(VecModel::from_slice(&[]));
    ui.set_visible_count("0 visible".into());
    ui.set_status_title("Ready".into());
    ui.set_status_detail("Choose a drive or folder and start a scan.".into());
    ui.set_scan_size("-".into());
    ui.set_scan_allocated("-".into());
    ui.set_scan_entries("-".into());
    ui.set_scan_elapsed("-".into());
    ui.set_selected_path("No row selected".into());
    ui.set_scanning(false);

    let state = Arc::new(Mutex::new(AppState {
        scanner_mode: startup.scanner_mode,
        ..AppState::default()
    }));

    wire_callbacks(&ui, Arc::clone(&state));

    if startup.scan {
        start_scan(
            &ui,
            Arc::clone(&state),
            ui.get_scan_path().to_string(),
            startup.scanner_mode,
        );
    }

    ui.run()?;
    Ok(())
}

fn wire_callbacks(ui: &AppWindow, state: Arc<Mutex<AppState>>) {
    let ui_weak = ui.as_weak();
    ui.on_drive_selected(move |path| {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_scan_path(path);
        }
    });

    let ui_weak = ui.as_weak();
    let state_for_mode = Arc::clone(&state);
    ui.on_scanner_selected(move |index| {
        let mode = UiScannerMode::from_index(index);
        lock_state(&state_for_mode).scanner_mode = mode;
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_scanner_mode(mode.index());
        }
    });

    let ui_weak = ui.as_weak();
    let state_for_scan = Arc::clone(&state);
    ui.on_scan_requested(move |path| {
        if let Some(ui) = ui_weak.upgrade() {
            let mode = lock_state(&state_for_scan).scanner_mode;
            start_scan(&ui, Arc::clone(&state_for_scan), path.to_string(), mode);
        }
    });

    let ui_weak = ui.as_weak();
    let state_for_cancel = Arc::clone(&state);
    ui.on_cancel_requested(move || {
        let cancel = lock_state(&state_for_cancel).cancel.clone();
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_status_title("Cancelling".into());
            ui.set_status_detail("Stopping the current scan at the next safe checkpoint.".into());
        }
    });

    let ui_weak = ui.as_weak();
    let state_for_select = Arc::clone(&state);
    ui.on_row_selected(move |row_id| {
        let Some(id) = entry_id_from_i32(row_id) else {
            return;
        };
        lock_state(&state_for_select).selected = Some(id);
        if let Some(ui) = ui_weak.upgrade() {
            refresh_rows(&ui, &state_for_select);
        }
    });

    let ui_weak = ui.as_weak();
    let state_for_toggle = state;
    ui.on_row_toggled(move |row_id| {
        let Some(id) = entry_id_from_i32(row_id) else {
            return;
        };
        {
            let mut state = lock_state(&state_for_toggle);
            if !state.expanded.insert(id) {
                state.expanded.remove(&id);
            }
        }
        if let Some(ui) = ui_weak.upgrade() {
            refresh_rows(&ui, &state_for_toggle);
        }
    });
}

fn start_scan(
    ui: &AppWindow,
    state: Arc<Mutex<AppState>>,
    path_text: String,
    scanner_mode: UiScannerMode,
) {
    let path_text = path_text.trim().to_owned();
    if path_text.is_empty() {
        ui.set_status_title("Path required".into());
        ui.set_status_detail("Enter a drive or folder path before scanning.".into());
        return;
    }

    let path = PathBuf::from(&path_text);
    match maybe_relaunch_ui_scan_elevated(&path, scanner_mode) {
        Ok(true) => {
            ui.set_status_title("Administrator scan requested".into());
            ui.set_status_detail("Approve the UAC prompt to scan the NTFS volume directly.".into());
            return;
        }
        Ok(false) => {}
        Err(error) => {
            ui.set_status_title("Elevation check failed".into());
            ui.set_status_detail(error.into());
            return;
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut state = lock_state(&state);
        state.graph = None;
        state.index = None;
        state.expanded.clear();
        state.selected = None;
        state.cancel = Some(Arc::clone(&cancel));
        state.scanner_mode = scanner_mode;
    }
    refresh_rows(ui, &state);
    ui.set_scanning(true);
    ui.set_status_title("Scanning".into());
    ui.set_status_detail(format!("Scanning {path_text}").into());
    ui.set_scan_size("-".into());
    ui.set_scan_allocated("-".into());
    ui.set_scan_entries("0".into());
    ui.set_scan_elapsed("0 ms".into());

    let ui_weak = ui.as_weak();
    thread::spawn(move || {
        let started = Instant::now();
        let mut on_progress = |summary: ScanSummary| {
            let elapsed_ms = started.elapsed().as_millis();
            let ui_weak = ui_weak.clone();
            post_to_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    apply_progress(&ui, summary, elapsed_ms);
                }
            });
        };

        let result = scan_path(path, scanner_mode, &cancel, &mut on_progress);
        let elapsed_ms = started.elapsed().as_millis();
        post_to_event_loop(move || {
            if let Some(ui) = ui_weak.upgrade() {
                match result {
                    Ok(outcome) => apply_scan_complete(&ui, &state, outcome, elapsed_ms),
                    Err(error) => apply_scan_error(&ui, &state, error),
                }
            }
        });
    });
}

fn apply_progress(ui: &AppWindow, summary: ScanSummary, elapsed_ms: u128) {
    ui.set_status_title("Scanning".into());
    ui.set_status_detail(summary_detail(summary).into());
    ui.set_scan_entries(format_count(summary.entries).into());
    ui.set_scan_elapsed(format!("{elapsed_ms} ms").into());
}

fn apply_scan_complete(
    ui: &AppWindow,
    state: &Arc<Mutex<AppState>>,
    outcome: UiScanOutcome,
    elapsed_ms: u128,
) {
    let graph = Arc::new(outcome.graph);
    let index = TreeIndex::build(&graph);
    let (total_size, total_allocated) = graph_totals(&graph);
    let fallback_reason = outcome.fallback_reason;
    let scanner_label = outcome.scanner_label;
    let summary = outcome.summary;

    {
        let mut state = lock_state(state);
        state.expanded.clear();
        for root in &index.roots {
            state.expanded.insert(*root);
        }
        state.selected = None;
        state.cancel = None;
        state.graph = Some(graph);
        state.index = Some(index);
    }

    ui.set_scanning(false);
    ui.set_status_title("Scan complete".into());
    ui.set_status_detail(scan_complete_detail(scanner_label, fallback_reason, summary).into());
    ui.set_scan_size(format_bytes(total_size).into());
    ui.set_scan_allocated(format_bytes(total_allocated).into());
    ui.set_scan_entries(format_count(summary.entries).into());
    ui.set_scan_elapsed(format!("{elapsed_ms} ms").into());
    refresh_rows(ui, state);
}

fn apply_scan_error(ui: &AppWindow, state: &Arc<Mutex<AppState>>, error: String) {
    lock_state(state).cancel = None;
    ui.set_scanning(false);
    ui.set_status_title("Scan failed".into());
    ui.set_status_detail(error.into());
}

fn refresh_rows(ui: &AppWindow, state: &Arc<Mutex<AppState>>) {
    let (rows, selected_path) = {
        let state = lock_state(state);
        let rows = build_visible_rows(&state);
        let selected_path = selected_path_from_state(&state);
        (rows, selected_path)
    };
    ui.set_visible_count(format!("{} visible", format_count(rows.len() as u64)).into());
    ui.set_selected_path(selected_path.into());
    ui.set_rows(VecModel::from_slice(&rows));
}

fn build_visible_rows(state: &AppState) -> Vec<DisplayRow> {
    let Some(graph) = state.graph.as_deref() else {
        return Vec::new();
    };
    let Some(index) = state.index.as_ref() else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    for root in &index.roots {
        append_visible_row(graph, index, state, *root, 0, &mut rows);
    }
    rows
}

fn append_visible_row(
    graph: &FileGraph,
    index: &TreeIndex,
    state: &AppState,
    id: EntryId,
    depth: usize,
    rows: &mut Vec<DisplayRow>,
) {
    if let Some(row) = display_row_from_graph(graph, index, state, id, depth) {
        rows.push(row);
    }

    if !state.expanded.contains(&id) {
        return;
    }

    for child in index.children(id) {
        append_visible_row(graph, index, state, *child, depth + 1, rows);
    }
}

fn display_row_from_graph(
    graph: &FileGraph,
    index: &TreeIndex,
    state: &AppState,
    id: EntryId,
    depth: usize,
) -> Option<DisplayRow> {
    let entry = graph.entry(id)?;
    let stats = graph.stats(id)?;
    let child_count = index.child_count(id);
    let is_directory = entry.flags.contains(EntryFlags::DIRECTORY);
    Some(DisplayRow {
        id: i32::try_from(id.0).unwrap_or(i32::MAX),
        name: graph.name(id).unwrap_or_default().into(),
        size: format_bytes(stats.total_size.bytes()).into(),
        allocated: format_bytes(stats.total_allocated.bytes()).into(),
        kind: if is_directory { "folder" } else { "file" }.into(),
        depth: i32::try_from(depth).unwrap_or(i32::MAX),
        indent_px: i32::try_from(depth.saturating_mul(18)).unwrap_or(i32::MAX),
        children: if child_count == 0 {
            SharedString::default()
        } else {
            format_count(child_count as u64).into()
        },
        folder: child_count > 0,
        expanded: state.expanded.contains(&id),
        selected: state.selected == Some(id),
    })
}

fn selected_path_from_state(state: &AppState) -> String {
    let Some(graph) = state.graph.as_deref() else {
        return "No row selected".to_owned();
    };
    let Some(selected) = state.selected else {
        return "No row selected".to_owned();
    };
    graph
        .reconstruct_path(selected)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Selected path is unavailable".to_owned())
}

fn scan_path(
    path: PathBuf,
    mode: UiScannerMode,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> std::result::Result<UiScanOutcome, String> {
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
) -> std::result::Result<UiScanOutcome, String> {
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
) -> std::result::Result<UiScanOutcome, String> {
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
        .fold(
            (0_u64, 0_u64),
            |(size, allocated), (next_size, next_allocated)| {
                (
                    size.saturating_add(next_size),
                    allocated.saturating_add(next_allocated),
                )
            },
        )
}

fn maybe_relaunch_ui_scan_elevated(
    path: &Path,
    scanner_mode: UiScannerMode,
) -> std::result::Result<bool, String> {
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

fn should_request_elevation() -> std::result::Result<bool, String> {
    is_process_elevated()
        .map(|is_elevated| !is_elevated)
        .map_err(|error| format!("failed to check administrator elevation: {error}"))
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

fn drive_items_from_shortcuts(volumes: &[VolumeShortcut]) -> Vec<DriveItem> {
    volumes
        .iter()
        .map(|volume| DriveItem {
            label: volume.label.as_str().into(),
            path: volume.root.as_str().into(),
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

fn sort_entry_ids_by_total_size(graph: &FileGraph, ids: &mut [EntryId]) {
    ids.sort_by(|left, right| compare_entry_ids_by_total_size(graph, left, right));
}

fn compare_entry_ids_by_total_size(
    graph: &FileGraph,
    left: &EntryId,
    right: &EntryId,
) -> CmpOrdering {
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

fn summary_detail(summary: ScanSummary) -> String {
    format!(
        "{} files, {} folders, {} inaccessible",
        format_count(summary.files),
        format_count(summary.directories),
        format_count(summary.inaccessible)
    )
}

fn scan_complete_detail(
    scanner_label: &str,
    fallback_reason: Option<String>,
    summary: ScanSummary,
) -> String {
    let detail = format!("{scanner_label}; {}", summary_detail(summary));
    if let Some(reason) = fallback_reason {
        format!("{detail}; NTFS fallback reason: {reason}")
    } else {
        detail
    }
}

fn entry_id_from_i32(value: i32) -> Option<EntryId> {
    u32::try_from(value).ok().map(EntryId)
}

fn parse_startup_args(args: impl IntoIterator<Item = String>) -> StartupArgs {
    let mut path = None;
    let mut scanner_mode = UiScannerMode::Auto;
    let mut scan = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => path = args.next(),
            "--scanner" => {
                if let Some(value) = args
                    .next()
                    .and_then(|value| UiScannerMode::from_arg(&value))
                {
                    scanner_mode = value;
                }
            }
            "--scan" => scan = true,
            _ if !arg.starts_with('-') && path.is_none() => path = Some(arg),
            _ => {}
        }
    }

    StartupArgs {
        path,
        scanner_mode,
        scan,
    }
}

fn lock_state(state: &Arc<Mutex<AppState>>) -> MutexGuard<'_, AppState> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}

fn post_to_event_loop(f: impl FnOnce() + Send + 'static) {
    if let Err(error) = slint::invoke_from_event_loop(f) {
        eprintln!("failed to update DiskLoom UI: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use diskloom_core::{FileGraph, FileGraphBuilder, FileKind};

    use super::{
        AppState, TreeIndex, UiScannerMode, VolumeShortcut, build_visible_rows,
        default_scan_path_from, format_bytes, format_count, normalize_drive_root,
        parse_startup_args, ui_scan_needs_elevation,
    };

    fn sample_graph() -> FileGraph {
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
        builder.finish()
    }

    fn volume(root: &str, is_ntfs: bool) -> VolumeShortcut {
        VolumeShortcut {
            root: root.to_owned(),
            label: root.to_owned(),
            is_ntfs,
        }
    }

    #[test]
    fn tree_index_should_sort_children_by_total_size() {
        let graph = sample_graph();
        let root = graph.ids().next().unwrap();

        let index = TreeIndex::build(&graph);
        let children = index.children(root);

        assert_eq!(graph.name(children[0]), Some("big"));
        assert_eq!(graph.name(children[1]), Some("small.bin"));
    }

    #[test]
    fn visible_rows_should_expand_only_requested_folders() {
        let graph = Arc::new(sample_graph());
        let root = graph.ids().next().unwrap();
        let index = TreeIndex::build(&graph);
        let mut state = AppState {
            graph: Some(Arc::clone(&graph)),
            index: Some(index),
            ..AppState::default()
        };
        state.expanded.insert(root);

        let rows = build_visible_rows(&state);
        let names = rows
            .iter()
            .map(|row| row.name.to_string())
            .collect::<Vec<_>>();

        assert_eq!(names, ["root", "big", "small.bin"]);
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
    fn ui_scan_needs_elevation_should_match_direct_drive_scans_only() {
        assert!(ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            UiScannerMode::Auto
        ));
        assert!(ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            UiScannerMode::Ntfs
        ));
        assert!(!ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            UiScannerMode::Fallback
        ));
        assert!(!ui_scan_needs_elevation(
            std::path::Path::new("C:\\Users"),
            UiScannerMode::Auto
        ));
    }

    #[test]
    fn parse_startup_args_should_handle_scan_options() {
        let args = parse_startup_args([
            "--path".to_owned(),
            "C:\\".to_owned(),
            "--scanner".to_owned(),
            "ntfs".to_owned(),
            "--scan".to_owned(),
        ]);

        assert_eq!(args.path.as_deref(), Some("C:\\"));
        assert_eq!(args.scanner_mode, UiScannerMode::Ntfs);
        assert!(args.scan);
    }
}
