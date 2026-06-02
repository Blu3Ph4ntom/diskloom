#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    cmp::Ordering as CmpOrdering,
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use diskloom_core::{EntryFlags, EntryId, FileGraph};
use diskloom_ntfs::{NtfsScanControl, NtfsScanProgress, NtfsScanner};
use diskloom_scan::{FallbackScanner, ScanControl, ScanOptions, ScanSummary};
use diskloom_windows::{
    VolumeKind, discover_volumes, is_process_elevated, relaunch_current_process_elevated,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, Window};

const UI_PROGRESS_EVERY: u64 = 1_024;
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(120);
const DEFAULT_ROW_LIMIT: usize = 120;
const MAX_ROW_LIMIT: usize = 600;

fn main() {
    let startup = parse_startup_args(std::env::args().skip(1));

    tauri::Builder::default()
        .manage(SharedState::default())
        .manage(StartupState(startup))
        .invoke_handler(tauri::generate_handler![
            get_startup,
            discover_drives,
            start_scan,
            cancel_scan,
            get_visible_rows,
            toggle_entry,
            select_entry,
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("DiskLoom");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run DiskLoom");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScannerMode {
    Auto,
    Ntfs,
    Fallback,
}

impl ScannerMode {
    fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "ntfs" => Self::Ntfs,
            "fallback" => Self::Fallback,
            _ => Self::Auto,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ntfs => "ntfs",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone)]
struct StartupArgs {
    path: Option<String>,
    scanner: ScannerMode,
    scan: bool,
}

#[derive(Debug)]
struct StartupState(StartupArgs);

#[derive(Debug, Default)]
struct SharedState {
    inner: Arc<Mutex<AppState>>,
}

#[derive(Debug, Default)]
struct AppState {
    graph: Option<Arc<FileGraph>>,
    index: Option<TreeIndex>,
    expanded: HashSet<EntryId>,
    selected: Option<EntryId>,
    cancel: Option<Arc<AtomicBool>>,
    scanning: bool,
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

#[derive(Debug)]
struct ScanOutcome {
    graph: FileGraph,
    summary: ScanSummary,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupDto {
    path: Option<String>,
    scanner: &'static str,
    scan: bool,
    elevated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DriveDto {
    path: String,
    label: String,
    is_ntfs: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProgressDto {
    entries: u64,
    files: u64,
    directories: u64,
    inaccessible: u64,
    elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanCompleteDto {
    scanner_label: &'static str,
    fallback_reason: Option<String>,
    entries: u64,
    files: u64,
    directories: u64,
    inaccessible: u64,
    elapsed_ms: u128,
    size_bytes: u64,
    allocated_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeRowDto {
    id: u32,
    name: String,
    depth: u32,
    is_dir: bool,
    expanded: bool,
    child_count: u32,
    size_bytes: u64,
    allocated_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeViewportDto {
    offset: usize,
    total: usize,
    rows: Vec<TreeRowDto>,
}

#[tauri::command]
fn get_startup(startup: State<'_, StartupState>) -> StartupDto {
    StartupDto {
        path: startup.0.path.clone(),
        scanner: startup.0.scanner.as_str(),
        scan: startup.0.scan,
        elevated: is_process_elevated().unwrap_or(false),
    }
}

#[tauri::command]
fn discover_drives() -> Vec<DriveDto> {
    discover_volume_shortcuts()
        .into_iter()
        .map(|volume| DriveDto {
            path: volume.root,
            label: volume.label,
            is_ntfs: volume.is_ntfs,
        })
        .collect()
}

#[tauri::command]
fn start_scan(
    path: String,
    scanner: String,
    state: State<'_, SharedState>,
    window: Window,
    app: AppHandle,
) -> Result<(), String> {
    let path = path.trim().to_owned();
    if path.is_empty() {
        return Err("enter a drive or folder path".to_owned());
    }
    let scanner = ScannerMode::parse(&scanner);
    let path_buf = PathBuf::from(&path);

    if maybe_relaunch_elevated(&path_buf, scanner)? {
        app.exit(0);
        return Ok(());
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut state = lock_state(&state.inner);
        if state.scanning {
            return Err("a scan is already running".to_owned());
        }
        state.graph = None;
        state.index = None;
        state.expanded.clear();
        state.selected = None;
        state.cancel = Some(Arc::clone(&cancel));
        state.scanning = true;
    }

    let shared = Arc::clone(&state.inner);
    let window_for_start = window.clone();
    let window_for_thread = window;
    emit_or_log(&window_for_start, "scan-started", path.clone());

    thread::spawn(move || {
        let started = Instant::now();
        let mut last_progress_emit = Instant::now()
            .checked_sub(PROGRESS_EMIT_INTERVAL)
            .unwrap_or_else(Instant::now);
        let mut on_progress = |summary: ScanSummary| {
            if last_progress_emit.elapsed() < PROGRESS_EMIT_INTERVAL {
                return;
            }
            last_progress_emit = Instant::now();
            emit_or_log(
                &window_for_thread,
                "scan-progress",
                progress_dto(summary, started.elapsed().as_millis()),
            );
        };

        let result = scan_path(path_buf, scanner, &cancel, &mut on_progress);
        let elapsed_ms = started.elapsed().as_millis();
        match result {
            Ok(outcome) => apply_scan_complete(&shared, &window_for_thread, outcome, elapsed_ms),
            Err(error) => apply_scan_error(&shared, &window_for_thread, error),
        }
    });

    Ok(())
}

#[tauri::command]
fn cancel_scan(state: State<'_, SharedState>) {
    let cancel = lock_state(&state.inner).cancel.clone();
    if let Some(cancel) = cancel {
        cancel.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
fn get_visible_rows(
    offset: usize,
    limit: Option<usize>,
    state: State<'_, SharedState>,
) -> TreeViewportDto {
    let state = lock_state(&state.inner);
    viewport_from_state(&state, offset, normalized_limit(limit))
}

#[tauri::command]
fn toggle_entry(
    id: u32,
    offset: usize,
    limit: Option<usize>,
    state: State<'_, SharedState>,
) -> TreeViewportDto {
    let mut state = lock_state(&state.inner);
    let id = EntryId(id);
    let can_expand = state
        .index
        .as_ref()
        .is_some_and(|index| index.child_count(id) > 0);
    if can_expand && !state.expanded.insert(id) {
        state.expanded.remove(&id);
    }
    viewport_from_state(&state, offset, normalized_limit(limit))
}

#[tauri::command]
fn select_entry(id: u32, state: State<'_, SharedState>) -> Option<String> {
    let mut state = lock_state(&state.inner);
    let id = EntryId(id);
    state.selected = Some(id);
    selected_path_from_state(&state)
}

fn apply_scan_complete(
    shared: &Arc<Mutex<AppState>>,
    window: &Window,
    outcome: ScanOutcome,
    elapsed_ms: u128,
) {
    let graph = Arc::new(outcome.graph);
    let index = TreeIndex::build(&graph);
    let (size_bytes, allocated_bytes) = graph_totals(&graph);
    let dto = ScanCompleteDto {
        scanner_label: outcome.scanner_label,
        fallback_reason: outcome.fallback_reason,
        entries: outcome.summary.entries,
        files: outcome.summary.files,
        directories: outcome.summary.directories,
        inaccessible: outcome.summary.inaccessible,
        elapsed_ms,
        size_bytes,
        allocated_bytes,
    };

    {
        let mut state = lock_state(shared);
        state.expanded.clear();
        for root in &index.roots {
            state.expanded.insert(*root);
        }
        state.graph = Some(graph);
        state.index = Some(index);
        state.selected = None;
        state.cancel = None;
        state.scanning = false;
    }

    emit_or_log(window, "scan-complete", dto);
}

fn apply_scan_error(shared: &Arc<Mutex<AppState>>, window: &Window, error: String) {
    {
        let mut state = lock_state(shared);
        state.cancel = None;
        state.scanning = false;
    }
    emit_or_log(window, "scan-error", error);
}

fn viewport_from_state(state: &AppState, offset: usize, limit: usize) -> TreeViewportDto {
    let Some(graph) = state.graph.as_deref() else {
        return TreeViewportDto {
            offset: 0,
            total: 0,
            rows: Vec::new(),
        };
    };
    let Some(index) = state.index.as_ref() else {
        return TreeViewportDto {
            offset: 0,
            total: 0,
            rows: Vec::new(),
        };
    };

    let mut cursor = 0;
    let mut rows = Vec::with_capacity(limit.min(DEFAULT_ROW_LIMIT));
    for root in &index.roots {
        collect_visible_rows(
            graph,
            index,
            state,
            *root,
            0,
            offset,
            limit,
            &mut cursor,
            &mut rows,
        );
    }

    TreeViewportDto {
        offset: offset.min(cursor),
        total: cursor,
        rows,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Tree traversal keeps hot-path state explicit and allocation-free"
)]
fn collect_visible_rows(
    graph: &FileGraph,
    index: &TreeIndex,
    state: &AppState,
    id: EntryId,
    depth: usize,
    offset: usize,
    limit: usize,
    cursor: &mut usize,
    rows: &mut Vec<TreeRowDto>,
) {
    if *cursor >= offset
        && rows.len() < limit
        && let Some(row) = tree_row_from_graph(graph, index, state, id, depth)
    {
        rows.push(row);
    }
    *cursor += 1;

    if !state.expanded.contains(&id) {
        return;
    }
    for child in index.children(id) {
        collect_visible_rows(
            graph,
            index,
            state,
            *child,
            depth + 1,
            offset,
            limit,
            cursor,
            rows,
        );
    }
}

fn tree_row_from_graph(
    graph: &FileGraph,
    index: &TreeIndex,
    state: &AppState,
    id: EntryId,
    depth: usize,
) -> Option<TreeRowDto> {
    let entry = graph.entry(id)?;
    let stats = graph.stats(id)?;
    let child_count = index.child_count(id);
    Some(TreeRowDto {
        id: id.0,
        name: graph.name(id).unwrap_or_default().to_owned(),
        depth: depth as u32,
        is_dir: entry.flags.contains(EntryFlags::DIRECTORY),
        expanded: state.expanded.contains(&id),
        child_count: child_count as u32,
        size_bytes: stats.total_size.bytes(),
        allocated_bytes: stats.total_allocated.bytes(),
    })
}

fn selected_path_from_state(state: &AppState) -> Option<String> {
    let graph = state.graph.as_deref()?;
    let selected = state.selected?;
    graph
        .reconstruct_path(selected)
        .map(|path| path.to_string_lossy().into_owned())
}

fn scan_path(
    path: PathBuf,
    mode: ScannerMode,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<ScanOutcome, String> {
    match mode {
        ScannerMode::Fallback => scan_fallback(path, None, cancel, on_progress),
        ScannerMode::Ntfs => scan_ntfs(&path, cancel, on_progress),
        ScannerMode::Auto => {
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
) -> Result<ScanOutcome, String> {
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

    Ok(ScanOutcome {
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
) -> Result<ScanOutcome, String> {
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

    Ok(ScanOutcome {
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

fn maybe_relaunch_elevated(path: &Path, scanner: ScannerMode) -> Result<bool, String> {
    if !scan_needs_elevation(path, scanner) || !should_request_elevation()? {
        return Ok(false);
    }

    let args = [
        "--path".to_owned(),
        path.to_string_lossy().into_owned(),
        "--scanner".to_owned(),
        scanner.as_str().to_owned(),
        "--scan".to_owned(),
    ];
    relaunch_current_process_elevated(args)
        .map_err(|error| format!("failed to request administrator access: {error}"))?;
    Ok(true)
}

fn scan_needs_elevation(path: &Path, scanner: ScannerMode) -> bool {
    scanner != ScannerMode::Fallback && drive_volume(path).is_some()
}

fn should_request_elevation() -> Result<bool, String> {
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

#[derive(Debug, Clone)]
struct VolumeShortcut {
    root: String,
    label: String,
    is_ntfs: bool,
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

fn progress_dto(summary: ScanSummary, elapsed_ms: u128) -> ScanProgressDto {
    ScanProgressDto {
        entries: summary.entries,
        files: summary.files,
        directories: summary.directories,
        inaccessible: summary.inaccessible,
        elapsed_ms,
    }
}

fn normalized_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_ROW_LIMIT).clamp(1, MAX_ROW_LIMIT)
}

fn parse_startup_args(args: impl IntoIterator<Item = String>) -> StartupArgs {
    let mut path = None;
    let mut scanner = ScannerMode::Auto;
    let mut scan = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => path = args.next(),
            "--scanner" => {
                if let Some(value) = args.next() {
                    scanner = ScannerMode::parse(&value);
                }
            }
            "--scan" => scan = true,
            _ if !arg.starts_with('-') && path.is_none() => path = Some(arg),
            _ => {}
        }
    }

    StartupArgs {
        path,
        scanner,
        scan,
    }
}

fn lock_state(state: &Arc<Mutex<AppState>>) -> MutexGuard<'_, AppState> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}

fn emit_or_log<T>(window: &Window, event: &str, payload: T)
where
    T: Serialize + Clone,
{
    if let Err(error) = window.emit(event, payload) {
        eprintln!("failed to emit DiskLoom event `{event}`: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use diskloom_core::{FileGraph, FileGraphBuilder, FileKind};

    use super::{
        AppState, ScannerMode, TreeIndex, drive_volume, normalized_limit, parse_startup_args,
        scan_needs_elevation, viewport_from_state,
    };

    fn sample_graph() -> FileGraph {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "C:\\", FileKind::Directory, 0, 0, 0)
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

    #[test]
    fn startup_args_should_parse_elevated_scan_request() {
        let args = parse_startup_args([
            "--path".to_owned(),
            "C:\\".to_owned(),
            "--scanner".to_owned(),
            "ntfs".to_owned(),
            "--scan".to_owned(),
        ]);

        assert_eq!(args.path.as_deref(), Some("C:\\"));
        assert_eq!(args.scanner, ScannerMode::Ntfs);
        assert!(args.scan);
    }

    #[test]
    fn drive_volume_should_only_accept_drive_roots() {
        assert_eq!(drive_volume(Path::new("c:\\")).as_deref(), Some("C:"));
        assert_eq!(drive_volume(Path::new("D:")).as_deref(), Some("D:"));
        assert_eq!(drive_volume(Path::new("C:\\Users")), None);
    }

    #[test]
    fn scan_needs_elevation_should_match_direct_drive_scans_only() {
        assert!(scan_needs_elevation(Path::new("C:\\"), ScannerMode::Auto));
        assert!(scan_needs_elevation(Path::new("C:\\"), ScannerMode::Ntfs));
        assert!(!scan_needs_elevation(
            Path::new("C:\\"),
            ScannerMode::Fallback
        ));
        assert!(!scan_needs_elevation(
            Path::new("C:\\Users"),
            ScannerMode::Auto
        ));
    }

    #[test]
    fn viewport_should_return_visible_slice_from_expanded_tree() {
        let graph = Arc::new(sample_graph());
        let index = TreeIndex::build(&graph);
        let root = index.roots[0];
        let mut state = AppState {
            graph: Some(Arc::clone(&graph)),
            index: Some(index),
            ..AppState::default()
        };
        state.expanded.insert(root);

        let viewport = viewport_from_state(&state, 1, 2);

        assert_eq!(viewport.total, 3);
        assert_eq!(viewport.rows.len(), 2);
        assert_eq!(viewport.rows[0].name, "big");
    }

    #[test]
    fn normalized_limit_should_bound_tree_slices() {
        assert_eq!(normalized_limit(None), super::DEFAULT_ROW_LIMIT);
        assert_eq!(normalized_limit(Some(0)), 1);
        assert_eq!(normalized_limit(Some(10_000)), super::MAX_ROW_LIMIT);
    }
}
