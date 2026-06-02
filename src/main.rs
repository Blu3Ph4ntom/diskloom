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
    VolumeKind, discover_volumes, is_process_elevated, open_in_explorer, recycle_delete,
    relaunch_current_process_elevated, show_properties,
};
use serde::Serialize;
use tauri::{Emitter, Manager, State, Window};

const UI_PROGRESS_EVERY: u64 = 1_024;
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(120);
const DEFAULT_ROW_LIMIT: usize = 120;
const MAX_ROW_LIMIT: usize = 600;
const MAX_SCAN_CACHE_ITEMS: usize = 2;
const MAX_SCAN_CACHE_ENTRIES: usize = 3_000_000;
const SKIP_STARTUP_ELEVATION_ENV: &str = "DISKLOOM_SKIP_STARTUP_ELEVATION";

fn main() {
    if relaunch_elevated_at_startup() {
        return;
    }

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
            delete_entry,
            open_path,
            show_path_properties,
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
    sort: SortSpec,
    visible_roots: Vec<EntryId>,
    current_path_key: Option<String>,
    cache: Vec<CachedScan>,
    expanded: HashSet<EntryId>,
    selected: Option<EntryId>,
    cancel: Option<Arc<AtomicBool>>,
    scanning: bool,
}

#[derive(Debug, Clone)]
struct CachedScan {
    path_key: String,
    graph: Arc<FileGraph>,
    visible_roots: Vec<EntryId>,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
    summary: ScanSummary,
    size_bytes: u64,
    allocated_bytes: u64,
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
    sort: SortSpec,
}

impl TreeIndex {
    fn build(graph: &FileGraph, sort: SortSpec) -> Self {
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

        sort_entry_ids(graph, &mut roots, sort);
        child_pairs.sort_by(|(left_parent, left_child), (right_parent, right_child)| {
            left_parent
                .0
                .cmp(&right_parent.0)
                .then_with(|| compare_entry_ids(graph, left_child, right_child, sort))
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
            sort,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SortSpec {
    key: SortKey,
    descending: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            key: SortKey::Size,
            descending: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Allocated,
    Modified,
}

impl SortKey {
    fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or_default().to_ascii_lowercase().as_str() {
            "name" => Self::Name,
            "allocated" => Self::Allocated,
            "modified" => Self::Modified,
            _ => Self::Size,
        }
    }
}

impl SortSpec {
    fn from_parts(key: Option<&str>, descending: Option<bool>) -> Self {
        Self {
            key: SortKey::parse(key),
            descending: descending.unwrap_or_else(|| SortKey::parse(key) != SortKey::Name),
        }
    }
}

#[derive(Debug)]
struct ScanOutcome {
    graph: FileGraph,
    summary: ScanSummary,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
    display_root: Option<PathBuf>,
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
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
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
struct DeleteEventDto {
    path: String,
    permanently: bool,
    elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteErrorDto {
    path: String,
    permanently: bool,
    elapsed_ms: u128,
    error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeRowDto {
    id: u32,
    name: String,
    path: String,
    depth: u32,
    is_dir: bool,
    expanded: bool,
    child_count: u32,
    size_bytes: u64,
    allocated_bytes: u64,
    modified_unix: i64,
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
    let path = startup.0.path.clone().or_else(default_startup_path);
    let scan = startup.0.scan || path.is_some();

    StartupDto {
        path,
        scanner: startup.0.scanner.as_str(),
        scan,
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
            total_bytes: volume.total_bytes,
            free_bytes: volume.free_bytes,
        })
        .collect()
}

#[tauri::command]
fn start_scan(
    path: String,
    scanner: String,
    force: Option<bool>,
    state: State<'_, SharedState>,
    window: Window,
) -> Result<(), String> {
    let path = path.trim().to_owned();
    if path.is_empty() {
        return Err("enter a drive or folder path".to_owned());
    }
    let scanner = ScannerMode::parse(&scanner);
    let path_buf = PathBuf::from(&path);
    let path_key = cache_key_for_path(&path_buf);
    let force = force.unwrap_or(false);

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut state = lock_state(&state.inner);
        if state.scanning {
            return Err("a scan is already running".to_owned());
        }
        if force {
            state.cache.retain(|cached| cached.path_key != path_key);
        } else {
            if let Some(cached) = take_cached_scan(&mut state, &path_key) {
                cache_current_scan(&mut state);
                let dto = restore_cached_scan(&mut state, cached);
                emit_or_log(&window, "scan-started", path);
                emit_or_log(&window, "scan-complete", dto);
                return Ok(());
            }
            cache_current_scan(&mut state);
        }
        if force && state.current_path_key.as_deref() != Some(path_key.as_str()) {
            cache_current_scan(&mut state);
        }
        state.graph = None;
        state.index = None;
        state.visible_roots.clear();
        state.current_path_key = None;
        state.expanded.clear();
        state.selected = None;
        state.cancel = Some(Arc::clone(&cancel));
        state.scanning = true;
    }

    let shared = Arc::clone(&state.inner);
    let window_for_start = window.clone();
    let window_for_thread = window;
    let path_key_for_thread = path_key;
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
            Ok(outcome) => apply_scan_complete(
                &shared,
                &window_for_thread,
                outcome,
                elapsed_ms,
                path_key_for_thread,
            ),
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
    query: Option<String>,
    sort_key: Option<String>,
    sort_descending: Option<bool>,
    state: State<'_, SharedState>,
) -> TreeViewportDto {
    let mut state = lock_state(&state.inner);
    ensure_sort(
        &mut state,
        SortSpec::from_parts(sort_key.as_deref(), sort_descending),
    );
    viewport_from_state(&state, offset, normalized_limit(limit), query.as_deref())
}

#[tauri::command]
fn toggle_entry(
    id: u32,
    offset: usize,
    limit: Option<usize>,
    query: Option<String>,
    sort_key: Option<String>,
    sort_descending: Option<bool>,
    state: State<'_, SharedState>,
) -> TreeViewportDto {
    let mut state = lock_state(&state.inner);
    ensure_sort(
        &mut state,
        SortSpec::from_parts(sort_key.as_deref(), sort_descending),
    );
    let id = EntryId(id);
    let can_expand = state
        .index
        .as_ref()
        .is_some_and(|index| index.child_count(id) > 0);
    if can_expand && !state.expanded.insert(id) {
        state.expanded.remove(&id);
    }
    viewport_from_state(&state, offset, normalized_limit(limit), query.as_deref())
}

#[tauri::command]
fn select_entry(id: u32, state: State<'_, SharedState>) -> Option<String> {
    let mut state = lock_state(&state.inner);
    let id = EntryId(id);
    state.selected = Some(id);
    selected_path_from_state(&state)
}

#[tauri::command]
fn delete_entry(
    id: u32,
    permanently: Option<bool>,
    state: State<'_, SharedState>,
    window: Window,
) -> Result<(), String> {
    let path = selected_entry_path(id, &state)?;
    let permanently = permanently.unwrap_or(false);
    let path_label = path.to_string_lossy().into_owned();
    let shared = Arc::clone(&state.inner);

    emit_or_log(
        &window,
        "delete-started",
        DeleteEventDto {
            path: path_label.clone(),
            permanently,
            elapsed_ms: 0,
        },
    );

    thread::spawn(move || {
        let started = Instant::now();
        let result = if permanently {
            delete_permanently(&path)
                .map_err(|error| format!("failed to permanently delete path: {error}"))
        } else {
            recycle_delete(&path).map_err(|error| format!("failed to move to Recycle Bin: {error}"))
        };
        let elapsed_ms = started.elapsed().as_millis();

        match result {
            Ok(()) => {
                invalidate_scan_cache(&shared);
                emit_or_log(
                    &window,
                    "delete-complete",
                    DeleteEventDto {
                        path: path_label,
                        permanently,
                        elapsed_ms,
                    },
                );
            }
            Err(error) => emit_or_log(
                &window,
                "delete-error",
                DeleteErrorDto {
                    path: path_label,
                    permanently,
                    elapsed_ms,
                    error,
                },
            ),
        }
    });

    Ok(())
}

#[tauri::command]
fn open_path(
    path: Option<String>,
    id: Option<u32>,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let path = command_target_path(path, id, &state)?;
    open_in_explorer(&path).map_err(|error| format!("failed to open Explorer: {error}"))
}

#[tauri::command]
fn show_path_properties(
    path: Option<String>,
    id: Option<u32>,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let path = command_target_path(path, id, &state)?;
    show_properties(&path).map_err(|error| format!("failed to open Properties: {error}"))
}

fn selected_entry_path(id: u32, state: &State<'_, SharedState>) -> Result<PathBuf, String> {
    let state = lock_state(&state.inner);
    selected_entry_path_from_locked_state(EntryId(id), &state)
}

fn selected_entry_path_from_locked_state(id: EntryId, state: &AppState) -> Result<PathBuf, String> {
    let graph = state
        .graph
        .as_deref()
        .ok_or_else(|| "scan results are not loaded".to_owned())?;
    let entry = graph
        .entry(id)
        .ok_or_else(|| "selected entry no longer exists".to_owned())?;
    if entry.parent.is_none() {
        return Err("DiskLoom will not delete the scan root".to_owned());
    }
    graph
        .reconstruct_path(id)
        .ok_or_else(|| "failed to reconstruct selected path".to_owned())
}

fn command_target_path(
    path: Option<String>,
    id: Option<u32>,
    state: &State<'_, SharedState>,
) -> Result<PathBuf, String> {
    if let Some(id) = id {
        let state = lock_state(&state.inner);
        if let Ok(path) = selected_entry_path_from_locked_state(EntryId(id), &state) {
            return Ok(path);
        }
    }

    let path = path
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "select an entry or enter a path".to_owned())?;
    Ok(PathBuf::from(path))
}

fn delete_permanently(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn invalidate_scan_cache(shared: &Arc<Mutex<AppState>>) {
    let mut state = lock_state(shared);
    state.cache.clear();
    state.selected = None;
}

fn apply_scan_complete(
    shared: &Arc<Mutex<AppState>>,
    window: &Window,
    outcome: ScanOutcome,
    elapsed_ms: u128,
    path_key: String,
) {
    let graph = Arc::new(outcome.graph);
    let mut index = TreeIndex::build(&graph, SortSpec::default());
    if let Some(display_root) = outcome.display_root.as_deref()
        && let Some(root) = find_graph_path(&graph, display_root)
    {
        index.roots = vec![root];
    }
    let visible_roots = index.roots.clone();
    let (size_bytes, allocated_bytes) = totals_for_roots(&graph, &visible_roots);
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
        state.sort = index.sort;
        state.visible_roots = visible_roots;
        state.current_path_key = Some(path_key);
        state.graph = Some(graph);
        state.index = Some(index);
        state.selected = None;
        state.cancel = None;
        state.scanning = false;
    }

    emit_or_log(window, "scan-complete", dto);
}

fn ensure_sort(state: &mut AppState, sort: SortSpec) {
    if state.sort == sort {
        return;
    }
    let Some(graph) = state.graph.as_deref() else {
        state.sort = sort;
        return;
    };
    let mut index = TreeIndex::build(graph, sort);
    if !state.visible_roots.is_empty() {
        index.roots = state.visible_roots.clone();
    }
    state.index = Some(index);
    state.sort = sort;
}

fn cache_current_scan(state: &mut AppState) {
    let Some(path_key) = state.current_path_key.clone() else {
        return;
    };
    let Some(graph) = state.graph.as_ref().cloned() else {
        return;
    };
    if graph.len() > MAX_SCAN_CACHE_ENTRIES {
        return;
    }
    let Some(index) = state.index.as_ref() else {
        return;
    };
    let visible_roots = if state.visible_roots.is_empty() {
        index.roots.clone()
    } else {
        state.visible_roots.clone()
    };
    let (size_bytes, allocated_bytes) = totals_for_roots(&graph, &visible_roots);
    let summary = summary_from_graph(&graph);
    state.cache.retain(|cached| cached.path_key != path_key);
    state.cache.insert(
        0,
        CachedScan {
            path_key,
            graph,
            visible_roots,
            scanner_label: "cached scan",
            fallback_reason: None,
            summary,
            size_bytes,
            allocated_bytes,
        },
    );
    trim_scan_cache(state);
}

fn take_cached_scan(state: &mut AppState, path_key: &str) -> Option<CachedScan> {
    let position = state
        .cache
        .iter()
        .position(|cached| cached.path_key == path_key)?;
    Some(state.cache.remove(position))
}

fn restore_cached_scan(state: &mut AppState, cached: CachedScan) -> ScanCompleteDto {
    let mut index = TreeIndex::build(&cached.graph, state.sort);
    index.roots = cached.visible_roots.clone();
    state.expanded.clear();
    for root in &index.roots {
        state.expanded.insert(*root);
    }
    state.visible_roots = cached.visible_roots;
    state.current_path_key = Some(cached.path_key);
    state.graph = Some(cached.graph);
    state.index = Some(index);
    state.selected = None;
    state.cancel = None;
    state.scanning = false;

    ScanCompleteDto {
        scanner_label: cached.scanner_label,
        fallback_reason: cached.fallback_reason,
        entries: cached.summary.entries,
        files: cached.summary.files,
        directories: cached.summary.directories,
        inaccessible: cached.summary.inaccessible,
        elapsed_ms: 0,
        size_bytes: cached.size_bytes,
        allocated_bytes: cached.allocated_bytes,
    }
}

fn trim_scan_cache(state: &mut AppState) {
    while state.cache.len() > MAX_SCAN_CACHE_ITEMS {
        state.cache.pop();
    }
    while cached_entry_count(&state.cache) > MAX_SCAN_CACHE_ENTRIES {
        state.cache.pop();
    }
}

fn cached_entry_count(cache: &[CachedScan]) -> usize {
    cache.iter().map(|cached| cached.graph.len()).sum()
}

fn apply_scan_error(shared: &Arc<Mutex<AppState>>, window: &Window, error: String) {
    {
        let mut state = lock_state(shared);
        state.cancel = None;
        state.scanning = false;
    }
    emit_or_log(window, "scan-error", error);
}

fn viewport_from_state(
    state: &AppState,
    offset: usize,
    limit: usize,
    query: Option<&str>,
) -> TreeViewportDto {
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

    let query = normalized_query(query);
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
            query.as_deref(),
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
    query: Option<&str>,
    cursor: &mut usize,
    rows: &mut Vec<TreeRowDto>,
) {
    let row_matches = query.is_none_or(|query| entry_matches_query(graph, id, query));
    if row_matches {
        if *cursor >= offset
            && rows.len() < limit
            && let Some(row) = tree_row_from_graph(graph, index, state, id, depth)
        {
            rows.push(row);
        }
        *cursor += 1;
    }

    if query.is_none() && !state.expanded.contains(&id) {
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
            query,
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
    let is_dir = entry.flags.contains(EntryFlags::DIRECTORY);
    let display_path = display_path_from_graph(graph, id)?;
    let name = display_name_from_path(graph, id, &display_path);
    let (size_bytes, allocated_bytes) = if is_dir {
        (stats.total_size.bytes(), stats.total_allocated.bytes())
    } else {
        (stats.own_size.bytes(), stats.own_allocated.bytes())
    };
    Some(TreeRowDto {
        id: id.0,
        name,
        path: display_path.to_string_lossy().into_owned(),
        depth: depth as u32,
        is_dir,
        expanded: state.expanded.contains(&id),
        child_count: child_count as u32,
        size_bytes,
        allocated_bytes,
        modified_unix: entry.modified_unix,
    })
}

fn selected_path_from_state(state: &AppState) -> Option<String> {
    let graph = state.graph.as_deref()?;
    let selected = state.selected?;
    display_path_from_graph(graph, selected).map(|path| path.to_string_lossy().into_owned())
}

fn display_path_from_graph(graph: &FileGraph, id: EntryId) -> Option<PathBuf> {
    let path = graph.reconstruct_path(id)?;
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Some(path);
    };
    if !is_probable_short_name(name) {
        return Some(path);
    }
    std::fs::canonicalize(&path)
        .ok()
        .map(strip_verbatim_prefix)
        .or(Some(path))
}

fn display_name_from_path(graph: &FileGraph, id: EntryId, path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| graph.name(id).unwrap_or_default().to_owned())
}

fn is_probable_short_name(name: &str) -> bool {
    name.contains('~')
}

fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(stripped) = value.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path
}

fn find_graph_path(graph: &FileGraph, target: &Path) -> Option<EntryId> {
    let target = normalized_path_key(target);
    graph.ids().find(|id| {
        graph
            .reconstruct_path(*id)
            .is_some_and(|path| normalized_path_key(&path) == target)
    })
}

fn normalized_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .trim()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

fn cache_key_for_path(path: &Path) -> String {
    normalized_path_key(path)
}

fn normalized_query(query: Option<&str>) -> Option<String> {
    let query = query?.trim();
    if query.is_empty() {
        return None;
    }
    Some(query.to_lowercase())
}

fn entry_matches_query(graph: &FileGraph, id: EntryId, query: &str) -> bool {
    let name = graph.name(id).unwrap_or_default();
    if contains_case_insensitive(name, query) {
        return true;
    }
    if !query.contains(['\\', '/']) {
        return false;
    }
    graph
        .reconstruct_path(id)
        .is_some_and(|path| contains_case_insensitive(&path.to_string_lossy(), query))
}

fn contains_case_insensitive(value: &str, lowercase_query: &str) -> bool {
    if value.is_ascii() && lowercase_query.is_ascii() {
        let needle = lowercase_query.as_bytes();
        if needle.len() > value.len() {
            return false;
        }
        return value.as_bytes().windows(needle.len()).any(|window| {
            window
                .iter()
                .zip(needle)
                .all(|(left, right)| left.to_ascii_lowercase() == *right)
        });
    }
    value.to_lowercase().contains(lowercase_query)
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
            if let Some(volume) = drive_for_path(&path) {
                match scan_ntfs_volume(
                    &volume,
                    display_root_for_direct_scan(&path),
                    cancel,
                    on_progress,
                ) {
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
        display_root: None,
    })
}

fn scan_ntfs(
    path: &Path,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<ScanOutcome, String> {
    let volume = drive_volume(path).unwrap_or_else(|| path.to_string_lossy().into_owned());
    scan_ntfs_volume(&volume, None, cancel, on_progress)
}

fn scan_ntfs_volume(
    volume: &str,
    display_root: Option<PathBuf>,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<ScanOutcome, String> {
    let graph = NtfsScanner::scan_volume_with_control(volume, UI_PROGRESS_EVERY, |progress| {
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
        display_root,
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

fn totals_for_roots(graph: &FileGraph, roots: &[EntryId]) -> (u64, u64) {
    roots
        .iter()
        .filter_map(|id| {
            let stats = graph.stats(*id)?;
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

#[cfg(test)]
fn scan_needs_elevation(path: &Path, scanner: ScannerMode) -> bool {
    scanner != ScannerMode::Fallback && drive_volume(path).is_some()
}

fn relaunch_elevated_at_startup() -> bool {
    if std::env::var_os(SKIP_STARTUP_ELEVATION_ENV).is_some() {
        return false;
    }
    match is_process_elevated() {
        Ok(true) => false,
        Ok(false) => {
            let args = std::env::args_os().skip(1).collect::<Vec<_>>();
            match relaunch_current_process_elevated(args) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("failed to request administrator access: {error}");
                    false
                }
            }
        }
        Err(error) => {
            eprintln!("failed to check administrator elevation: {error}");
            false
        }
    }
}

fn default_startup_path() -> Option<String> {
    system_drive_root()
        .or_else(|| {
            discover_volume_shortcuts()
                .into_iter()
                .find(|volume| volume.is_ntfs)
                .map(|volume| volume.root)
        })
        .or_else(|| {
            discover_volume_shortcuts()
                .into_iter()
                .next()
                .map(|volume| volume.root)
        })
}

fn system_drive_root() -> Option<String> {
    let value = std::env::var_os("SystemDrive")?;
    let mut drive = value
        .to_string_lossy()
        .trim()
        .trim_end_matches(['\\', '/'])
        .to_owned();
    if drive.is_empty() {
        return None;
    }
    if !drive.ends_with(':') {
        return None;
    }
    drive.push('\\');
    Some(drive)
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

fn drive_for_path(path: &Path) -> Option<String> {
    if let Some(volume) = drive_volume(path) {
        return Some(volume);
    }
    let value = path.to_string_lossy();
    let mut chars = value.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next()? != ':' {
        return None;
    }
    Some(format!("{}:", letter.to_ascii_uppercase()))
}

fn display_root_for_direct_scan(path: &Path) -> Option<PathBuf> {
    drive_volume(path).is_none().then(|| path.to_path_buf())
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
                total_bytes: volume.total_bytes,
                free_bytes: volume.free_bytes,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
struct VolumeShortcut {
    root: String,
    label: String,
    is_ntfs: bool,
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
}

fn sort_entry_ids(graph: &FileGraph, ids: &mut [EntryId], sort: SortSpec) {
    ids.sort_by(|left, right| compare_entry_ids(graph, left, right, sort));
}

fn compare_entry_ids(
    graph: &FileGraph,
    left: &EntryId,
    right: &EntryId,
    sort: SortSpec,
) -> CmpOrdering {
    let ordering = match sort.key {
        SortKey::Name => {
            let left_name = graph.name(*left).unwrap_or_default();
            let right_name = graph.name(*right).unwrap_or_default();
            left_name
                .to_ascii_lowercase()
                .cmp(&right_name.to_ascii_lowercase())
        }
        SortKey::Size => compare_entry_display_numeric(graph, left, right, |stats, is_dir| {
            if is_dir {
                stats.total_size.bytes()
            } else {
                stats.own_size.bytes()
            }
        }),
        SortKey::Allocated => compare_entry_display_numeric(graph, left, right, |stats, is_dir| {
            if is_dir {
                stats.total_allocated.bytes()
            } else {
                stats.own_allocated.bytes()
            }
        }),
        SortKey::Modified => {
            let left_modified = graph.entry(*left).map_or(0, |entry| entry.modified_unix);
            let right_modified = graph.entry(*right).map_or(0, |entry| entry.modified_unix);
            left_modified.cmp(&right_modified)
        }
    };

    let ordering = if sort.descending {
        ordering.reverse()
    } else {
        ordering
    };

    ordering.then_with(|| {
        let left_name = graph.name(*left).unwrap_or_default();
        let right_name = graph.name(*right).unwrap_or_default();
        left_name.cmp(right_name)
    })
}

fn compare_entry_display_numeric(
    graph: &FileGraph,
    left: &EntryId,
    right: &EntryId,
    value: impl Fn(diskloom_core::NodeStats, bool) -> u64,
) -> CmpOrdering {
    let left_value = entry_display_numeric(graph, *left, &value);
    let right_value = entry_display_numeric(graph, *right, &value);
    left_value.cmp(&right_value)
}

fn entry_display_numeric(
    graph: &FileGraph,
    id: EntryId,
    value: &impl Fn(diskloom_core::NodeStats, bool) -> u64,
) -> u64 {
    let Some(stats) = graph.stats(id) else {
        return 0;
    };
    let is_dir = graph
        .entry(id)
        .is_some_and(|entry| entry.flags.contains(EntryFlags::DIRECTORY));
    value(stats, is_dir)
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
        let index = TreeIndex::build(&graph, super::SortSpec::default());
        let root = index.roots[0];
        let mut state = AppState {
            graph: Some(Arc::clone(&graph)),
            index: Some(index),
            ..AppState::default()
        };
        state.expanded.insert(root);

        let viewport = viewport_from_state(&state, 1, 2, None);

        assert_eq!(viewport.total, 3);
        assert_eq!(viewport.rows.len(), 2);
        assert_eq!(viewport.rows[0].name, "big");
    }

    #[test]
    fn viewport_query_should_search_all_descendants() {
        let graph = Arc::new(sample_graph());
        let index = TreeIndex::build(&graph, super::SortSpec::default());
        let state = AppState {
            graph: Some(Arc::clone(&graph)),
            index: Some(index),
            ..AppState::default()
        };

        let viewport = viewport_from_state(&state, 0, 10, Some("large"));

        assert_eq!(viewport.total, 1);
        assert_eq!(viewport.rows[0].name, "large.bin");
    }

    #[test]
    fn normalized_limit_should_bound_tree_slices() {
        assert_eq!(normalized_limit(None), super::DEFAULT_ROW_LIMIT);
        assert_eq!(normalized_limit(Some(0)), 1);
        assert_eq!(normalized_limit(Some(10_000)), super::MAX_ROW_LIMIT);
    }
}
