use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use diskloom_core::{EntryFlags, EntryId, FileGraph};
use diskloom_dupes::find_duplicate_candidates;
use diskloom_export::{CsvExportOptions, export_csv};
use diskloom_ntfs::NtfsScanner;
use diskloom_query::{
    CompiledFilter, FileTypeStat, NameMatcher, QueryFilter, file_type_stats,
    top_entries_by_total_size,
};
use diskloom_scan::{FallbackScanner, ScanOptions, ScanSummary};
use diskloom_windows::{
    VolumeKind, discover_volumes, is_process_elevated, relaunch_current_process_elevated,
    spawn_current_process_elevated_hidden,
};

#[derive(Debug, Parser)]
#[command(name = "dlm")]
#[command(about = "DiskLoom disk analyzer")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[expect(
    clippy::large_enum_variant,
    reason = "CLI arguments are parsed once at startup; keeping the subcommand shape direct avoids clap indirection"
)]
enum Command {
    Scan(ScanCommand),
    NtfsProbe { volume: String },
    Volumes,
}

#[derive(Debug, Parser)]
struct ScanCommand {
    path: PathBuf,

    #[arg(long, value_enum, default_value = "auto")]
    scanner: ScannerMode,

    #[arg(long)]
    csv: Option<PathBuf>,

    #[arg(long)]
    name: Option<String>,

    #[arg(long = "path")]
    path_filter: Option<String>,

    #[arg(long)]
    regex: bool,

    #[arg(long)]
    extension: Option<String>,

    #[arg(long)]
    min_size: Option<u64>,

    #[arg(long)]
    max_size: Option<u64>,

    #[arg(long)]
    min_allocated: Option<u64>,

    #[arg(long)]
    max_allocated: Option<u64>,

    #[arg(long)]
    modified_after: Option<i64>,

    #[arg(long)]
    modified_before: Option<i64>,

    #[arg(long)]
    files_only: bool,

    #[arg(long)]
    follow_symlinks: bool,

    #[arg(long)]
    precise_allocated: bool,

    #[arg(long)]
    duplicates: bool,

    #[arg(long)]
    file_types: bool,

    #[arg(long)]
    deep: bool,

    #[arg(long, default_value_t = 15)]
    limit: usize,

    #[arg(long)]
    summary_only: bool,

    #[arg(long)]
    raw: bool,

    #[arg(long, hide = true)]
    elevated_output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ScannerMode {
    Auto,
    Fallback,
    Ntfs,
}

struct ScanOutcome {
    graph: FileGraph,
    summary: ScanSummary,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
    display_root: Option<PathBuf>,
    volume_usage: Option<VolumeUsage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VolumeUsage {
    used: u64,
    total: u64,
    accounted_allocated: u64,
}

pub fn run() -> Result<()> {
    run_with_args(std::env::args_os())
}

pub fn run_with_args(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    let cli = Cli::parse_from(normalize_args(args));

    match cli.command {
        Command::Scan(command) => run_scan(command),
        Command::NtfsProbe { volume } => {
            if maybe_relaunch_current_command_elevated_for_volume(&volume)? {
                return Ok(());
            }
            run_ntfs_probe(&volume)
        }
        Command::Volumes => run_volumes(),
    }
}

fn normalize_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args = args.into_iter().collect::<Vec<_>>();
    if args.is_empty() {
        return args;
    }
    let Some(first) = args.get(1).and_then(|arg| arg.to_str()).map(str::to_owned) else {
        args.insert(1, OsString::from("scan"));
        args.insert(2, OsString::from("."));
        return args;
    };
    if matches!(
        first.as_str(),
        "scan" | "ntfs-probe" | "volumes" | "--help" | "-h"
    ) {
        return args;
    }
    args.insert(1, OsString::from("scan"));
    if first.starts_with('-') {
        args.insert(2, OsString::from("."));
    }
    args
}

fn run_scan(command: ScanCommand) -> Result<()> {
    if let Some(output_path) = command.elevated_output.clone() {
        return run_elevated_child_scan(command, &output_path);
    }

    if maybe_run_scan_elevated_and_print(&command)? {
        return Ok(());
    }

    let mut stdout = io::stdout().lock();
    run_scan_to_writer(command, &mut stdout, true)
}

fn run_scan_to_writer(
    command: ScanCommand,
    writer: &mut impl Write,
    show_spinner: bool,
) -> Result<()> {
    let display_path = display_scan_path(&command.path);
    let mut spinner = show_spinner
        .then(|| StatusSpinner::start_if_terminal(format!("Scanning {}", display_path.display())))
        .flatten();
    let started = Instant::now();
    let outcome = scan_path(&command)
        .with_context(|| format!("failed to scan {}", display_path.display()))?;
    if let Some(spinner) = spinner.as_mut() {
        spinner.stop();
    }
    let elapsed = started.elapsed();
    let display_root = outcome.display_root.clone();
    let graph = outcome.graph;
    let summary = outcome.summary;

    let filter = query_filter(&command)?.compile()?;
    let ids = ranked_entry_ids(&graph, &filter, &command, display_root.as_deref());

    write_scan_summary(
        writer,
        &display_path,
        outcome.scanner_label,
        &graph,
        &summary,
        outcome.volume_usage,
        elapsed,
    )?;
    if let Some(reason) = outcome.fallback_reason {
        writeln!(writer, "Fallback: {reason}")?;
    }
    if !command.summary_only {
        writeln!(writer)?;
        write_top_entries(writer, &graph, &ids, command.limit, command.raw)?;
    }

    if command.duplicates {
        writeln!(writer)?;
        write_duplicate_candidates(writer, &graph, command.limit, command.raw)?;
    }

    if command.file_types {
        writeln!(writer)?;
        write_file_type_stats(writer, &file_type_stats(&graph, command.limit), command.raw)?;
    }

    if let Some(csv_path) = command.csv {
        let mut file = File::create(&csv_path)
            .with_context(|| format!("failed to create {}", csv_path.display()))?;
        export_csv(
            &graph,
            &mut file,
            CsvExportOptions {
                include_directories: !command.files_only,
            },
        )
        .with_context(|| format!("failed to export {}", csv_path.display()))?;
        writeln!(writer)?;
        writeln!(writer, "CSV exported to {}", csv_path.display())?;
    }

    Ok(())
}

fn run_elevated_child_scan(command: ScanCommand, output_path: &Path) -> Result<()> {
    let mut file = File::create(output_path)
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    match run_scan_to_writer(command, &mut file, false) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = writeln!(file, "Error: {error:#}");
            Err(error)
        }
    }
}

struct StatusSpinner {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StatusSpinner {
    fn start_if_terminal(label: String) -> Option<Self> {
        io::stderr().is_terminal().then(|| Self::start(label))
    }

    fn start(label: String) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let handle = thread::spawn(move || {
            let frames = ['|', '/', '-', '\\'];
            let started = Instant::now();
            let mut idx = 0;
            while thread_running.load(Ordering::Relaxed) {
                let mut stderr = io::stderr().lock();
                let _ = write!(
                    stderr,
                    "\r{} {} ({})",
                    frames[idx % frames.len()],
                    label,
                    format_duration(started.elapsed())
                );
                let _ = stderr.flush();
                idx += 1;
                thread::sleep(Duration::from_millis(110));
            }
        });
        Self {
            running,
            handle: Some(handle),
        }
    }

    fn stop(&mut self) {
        if self.handle.is_none() {
            return;
        }
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "\r{}\r", " ".repeat(120));
        let _ = stderr.flush();
    }
}

impl Drop for StatusSpinner {
    fn drop(&mut self) {
        self.stop();
    }
}

fn elevated_output_path() -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    std::env::temp_dir().join(format!(
        "diskloom-elevated-{}-{millis}.txt",
        std::process::id()
    ))
}

fn scan_path(command: &ScanCommand) -> Result<ScanOutcome> {
    match command.scanner {
        ScannerMode::Fallback => scan_fallback(command, None),
        ScannerMode::Ntfs => scan_ntfs(command).map_err(Into::into),
        ScannerMode::Auto => {
            let resolved_path = resolved_scan_path(&command.path);
            if drive_for_path(&resolved_path).is_some() && is_already_elevated() {
                match scan_ntfs(command) {
                    Ok(outcome) => Ok(outcome),
                    Err(error) => scan_fallback(command, Some(error.to_string())),
                }
            } else {
                scan_fallback(command, None)
            }
        }
    }
}

fn maybe_run_scan_elevated_and_print(command: &ScanCommand) -> Result<bool> {
    if !scan_needs_elevation(&command.path, command.scanner) || !should_request_elevation()? {
        return Ok(false);
    }

    let output_path = elevated_output_path();
    let mut args = std::env::args_os().skip(1).collect::<Vec<_>>();
    args.push(OsString::from("--elevated-output"));
    args.push(output_path.as_os_str().to_os_string());

    eprintln!("DiskLoom requested administrator access for direct NTFS scanning.");
    let mut approval_spinner =
        StatusSpinner::start_if_terminal("Waiting for administrator approval".to_owned());
    let child = spawn_current_process_elevated_hidden(&args)
        .context("failed to start elevated DiskLoom scan")?;
    if let Some(spinner) = approval_spinner.as_mut() {
        spinner.stop();
    }

    let mut scan_spinner =
        StatusSpinner::start_if_terminal("Scanning disk as administrator".to_owned());
    let exit_code = child
        .wait()
        .context("elevated DiskLoom scan did not finish")?;
    if let Some(spinner) = scan_spinner.as_mut() {
        spinner.stop();
    }

    let output = fs::read_to_string(&output_path).with_context(|| {
        format!(
            "failed to read elevated scan output {}",
            output_path.display()
        )
    })?;
    let _ = fs::remove_file(&output_path);
    print!("{output}");
    io::stdout().flush().ok();

    if exit_code != 0 {
        bail!("elevated DiskLoom scan failed with exit code {exit_code}");
    }
    Ok(true)
}

fn maybe_relaunch_current_command_elevated_for_volume(volume: &str) -> Result<bool> {
    if volume_arg_is_drive_root(volume) {
        maybe_relaunch_current_command_elevated("direct NTFS volume probing")
    } else {
        Ok(false)
    }
}

fn scan_needs_elevation(path: &Path, scanner: ScannerMode) -> bool {
    scanner != ScannerMode::Fallback && drive_for_path(&resolved_scan_path(path)).is_some()
}

fn volume_arg_is_drive_root(volume: &str) -> bool {
    let trimmed = volume.trim_end_matches(['\\', '/']);
    let mut chars = trimmed.chars();
    let Some(letter) = chars.next() else {
        return false;
    };
    letter.is_ascii_alphabetic() && chars.next() == Some(':') && chars.next().is_none()
}

fn maybe_relaunch_current_command_elevated(reason: &str) -> Result<bool> {
    if !should_request_elevation()? {
        return Ok(false);
    }

    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    relaunch_current_process_elevated(&args)
        .with_context(|| format!("failed to request administrator access for {reason}"))?;
    eprintln!("DiskLoom requested administrator access for {reason}.");
    Ok(true)
}

#[cfg(windows)]
fn should_request_elevation() -> Result<bool> {
    Ok(!is_process_elevated().context("failed to check administrator elevation")?)
}

#[cfg(windows)]
fn is_already_elevated() -> bool {
    is_process_elevated().unwrap_or(false)
}

#[cfg(not(windows))]
fn should_request_elevation() -> Result<bool> {
    Ok(false)
}

#[cfg(not(windows))]
fn is_already_elevated() -> bool {
    false
}

fn scan_fallback(command: &ScanCommand, fallback_reason: Option<String>) -> Result<ScanOutcome> {
    let (graph, summary) = FallbackScanner::scan(ScanOptions {
        root: scan_input_path(&command.path),
        follow_symlinks: command.follow_symlinks,
        precise_allocated: command.precise_allocated,
    })?;
    Ok(ScanOutcome {
        graph,
        summary,
        scanner_label: "fallback traversal",
        fallback_reason,
        display_root: None,
        volume_usage: None,
    })
}

fn scan_ntfs(command: &ScanCommand) -> Result<ScanOutcome, diskloom_ntfs::NtfsScanError> {
    let resolved_path = resolved_scan_path(&command.path);
    let volume = drive_for_path(&resolved_path)
        .unwrap_or_else(|| command.path.to_string_lossy().into_owned());
    let graph = NtfsScanner::scan_volume(&volume)?;
    let summary = summary_from_graph(&graph);
    let display_root = display_root_for_direct_scan(&resolved_path);
    let volume_usage = display_root
        .is_none()
        .then(|| volume_usage_for_direct_scan(&volume, &graph))
        .flatten();
    Ok(ScanOutcome {
        graph,
        summary,
        scanner_label: "direct NTFS MFT",
        fallback_reason: None,
        display_root,
        volume_usage,
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

fn volume_usage_for_direct_scan(volume: &str, graph: &FileGraph) -> Option<VolumeUsage> {
    let volume_root = format!("{}\\", volume.trim_end_matches(['\\', '/']));
    let volume = discover_volumes()
        .ok()?
        .into_iter()
        .find(|candidate| candidate.root.eq_ignore_ascii_case(&volume_root))?;
    let total = volume.total_bytes?;
    let free = volume.free_bytes?;
    Some(VolumeUsage {
        used: total.saturating_sub(free),
        total,
        accounted_allocated: graph_accounted_allocated(graph),
    })
}

fn graph_accounted_allocated(graph: &FileGraph) -> u64 {
    graph
        .ids()
        .filter(|id| !entry_has_parent(graph, *id))
        .filter_map(|id| graph.stats(id))
        .fold(0_u64, |sum, stats| {
            sum.saturating_add(stats.total_allocated.bytes())
        })
}

fn resolved_scan_path(path: &Path) -> PathBuf {
    if let Some(root) = drive_root_from_designator(path) {
        return root;
    }

    std::fs::canonicalize(path)
        .map(strip_verbatim_prefix)
        .unwrap_or_else(|_| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|current_dir| current_dir.join(path))
                    .unwrap_or_else(|_| path.to_path_buf())
            }
        })
}

fn scan_input_path(path: &Path) -> PathBuf {
    drive_root_from_designator(path).unwrap_or_else(|| path.to_path_buf())
}

fn display_scan_path(path: &Path) -> PathBuf {
    scan_input_path(path)
}

fn drive_root_from_designator(path: &Path) -> Option<PathBuf> {
    let value = path.to_string_lossy();
    let mut chars = value.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next()? != ':' || chars.next().is_some() {
        return None;
    }

    Some(PathBuf::from(format!("{}:\\", letter.to_ascii_uppercase())))
}

fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(stripped) = value.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path
}

fn query_filter(command: &ScanCommand) -> Result<QueryFilter> {
    let name = matcher_from_pattern(command.name.as_deref(), command.regex)?;
    let path = matcher_from_pattern(command.path_filter.as_deref(), command.regex)?;

    Ok(QueryFilter {
        name,
        extension: command.extension.clone(),
        path,
        min_size: command.min_size,
        max_size: command.max_size,
        min_allocated: command.min_allocated,
        max_allocated: command.max_allocated,
        modified_after: command.modified_after,
        modified_before: command.modified_before,
        include_directories: !command.files_only,
    })
}

fn matcher_from_pattern(pattern: Option<&str>, regex: bool) -> Result<Option<NameMatcher>> {
    Ok(match (pattern, regex) {
        (Some(pattern), true) => Some(NameMatcher::regex(pattern)?),
        (Some(needle), false) => Some(NameMatcher::contains(needle)),
        (None, _) => None,
    })
}

fn ranked_entry_ids(
    graph: &FileGraph,
    filter: &CompiledFilter,
    command: &ScanCommand,
    display_root: Option<&Path>,
) -> Vec<EntryId> {
    let root_ids = visible_root_ids(graph, display_root);
    let use_deep_ranking = command.deep || command_has_filters(command);
    let ids: Vec<_> = if use_deep_ranking {
        root_ids
            .iter()
            .flat_map(|root| descendants_of(graph, *root))
            .filter(|id| entry_has_parent(graph, *id))
            .filter(|id| filter.matches(graph, *id))
            .collect()
    } else {
        root_ids
            .iter()
            .flat_map(|root| graph.children_of(*root))
            .filter(|id| filter.matches(graph, *id))
            .collect()
    };
    top_entries_by_total_size(graph, ids, command.limit)
}

fn entry_has_parent(graph: &FileGraph, id: EntryId) -> bool {
    graph.entry(id).is_some_and(|entry| entry.parent.is_some())
}

fn visible_root_ids(graph: &FileGraph, display_root: Option<&Path>) -> Vec<EntryId> {
    if let Some(display_root) = display_root
        && let Some(root) = find_graph_path(graph, display_root)
    {
        return vec![root];
    }
    graph
        .ids()
        .filter(|id| !entry_has_parent(graph, *id))
        .collect()
}

fn descendants_of(graph: &FileGraph, root: EntryId) -> Vec<EntryId> {
    let mut ids = Vec::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        ids.push(id);
        pending.extend(graph.children_of(id));
    }
    ids
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

fn command_has_filters(command: &ScanCommand) -> bool {
    command.name.is_some()
        || command.path_filter.is_some()
        || command.regex
        || command.extension.is_some()
        || command.min_size.is_some()
        || command.max_size.is_some()
        || command.min_allocated.is_some()
        || command.max_allocated.is_some()
        || command.modified_after.is_some()
        || command.modified_before.is_some()
        || command.files_only
}

fn write_top_entries(
    writer: &mut impl Write,
    graph: &FileGraph,
    ids: &[EntryId],
    limit: usize,
    raw: bool,
) -> Result<()> {
    writeln!(writer, "Largest entries")?;
    writeln!(
        writer,
        "{:<12} {:<12} {:<5} Path",
        "Size", "Allocated", "Kind"
    )?;

    for id in ids.iter().take(limit) {
        let Some(stats) = graph.stats(*id) else {
            continue;
        };
        let Some(entry) = graph.entry(*id) else {
            continue;
        };
        let kind = if entry.flags.contains(EntryFlags::DIRECTORY) {
            "dir"
        } else {
            "file"
        };
        let path = graph
            .reconstruct_path(*id)
            .map(|path| path.display().to_string())
            .unwrap_or_default();

        writeln!(
            writer,
            "{:<12} {:<12} {:<5} {}",
            format_size(stats.total_size.bytes(), raw),
            format_size(stats.total_allocated.bytes(), raw),
            kind,
            path
        )?;
    }

    Ok(())
}

fn write_duplicate_candidates(
    writer: &mut impl Write,
    graph: &FileGraph,
    limit: usize,
    raw: bool,
) -> Result<()> {
    let candidates = find_duplicate_candidates(graph);
    writeln!(writer, "Duplicate candidates by size/name/date:")?;
    writeln!(
        writer,
        "{:<12} {:<7} {:<12} Name",
        "Size", "Entries", "Modified"
    )?;

    for candidate in candidates.iter().take(limit) {
        writeln!(
            writer,
            "{:<12} {:<7} {:<12} {}",
            format_size(candidate.size, raw),
            candidate.entries.len(),
            candidate.modified_unix,
            candidate.name
        )?;
        for id in &candidate.entries {
            let path = graph
                .reconstruct_path(*id)
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            writeln!(writer, "\t{path}")?;
        }
    }

    Ok(())
}

fn write_file_type_stats(writer: &mut impl Write, stats: &[FileTypeStat], raw: bool) -> Result<()> {
    writeln!(writer, "File types by size:")?;
    writeln!(
        writer,
        "{:<12} {:<12} {:<8} Extension",
        "Size", "Allocated", "Files"
    )?;

    for stat in stats {
        writeln!(
            writer,
            "{:<12} {:<12} {:<8} {}",
            format_size(stat.size, raw),
            format_size(stat.allocated, raw),
            format_count(stat.files),
            stat.extension
        )?;
    }

    Ok(())
}

fn write_scan_summary(
    writer: &mut impl Write,
    path: &Path,
    scanner_label: &str,
    graph: &FileGraph,
    summary: &ScanSummary,
    volume_usage: Option<VolumeUsage>,
    elapsed: Duration,
) -> Result<()> {
    writeln!(writer, "DiskLoom scan")?;
    writeln!(writer, "{:<12} {}", "Path", path.display())?;
    writeln!(writer, "{:<12} {}", "Scanner", scanner_label)?;
    writeln!(
        writer,
        "{:<12} {}",
        "Entries",
        format_count(summary.entries)
    )?;
    writeln!(
        writer,
        "{:<12} {} files, {} folders, {} inaccessible",
        "Breakdown",
        format_count(summary.files),
        format_count(summary.directories),
        format_count(summary.inaccessible)
    )?;
    writeln!(
        writer,
        "{:<12} {} logical, {} allocated",
        "Accounted",
        format_size(graph_accounted_size(graph), false),
        format_size(graph_accounted_allocated(graph), false)
    )?;
    if let Some(volume_usage) = volume_usage {
        let unaccounted = volume_usage
            .used
            .saturating_sub(volume_usage.accounted_allocated);
        writeln!(
            writer,
            "{:<12} {} used of {} total",
            "Volume",
            format_size(volume_usage.used, false),
            format_size(volume_usage.total, false)
        )?;
        if unaccounted > 0 {
            writeln!(
                writer,
                "{:<12} {} used by NTFS metadata, shadow copies, alternate streams, or other space not attributed to visible files",
                "Unaccounted",
                format_size(unaccounted, false)
            )?;
        }
    }
    writeln!(writer, "{:<12} {}", "Elapsed", format_duration(elapsed))?;
    Ok(())
}

fn graph_accounted_size(graph: &FileGraph) -> u64 {
    graph
        .ids()
        .filter(|id| !entry_has_parent(graph, *id))
        .filter_map(|id| graph.stats(id))
        .fold(0_u64, |sum, stats| {
            sum.saturating_add(stats.total_size.bytes())
        })
}

fn format_size(bytes: u64, raw: bool) -> String {
    if raw {
        return bytes.to_string();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn format_count(value: u64) -> String {
    let value = value.to_string();
    let mut out = String::with_capacity(value.len() + value.len() / 3);
    for (idx, ch) in value.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_duration(duration: Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1000 {
        return format!("{ms} ms");
    }
    if ms < 60_000 {
        return format!("{:.2} s", duration.as_secs_f64());
    }
    let minutes = ms / 60_000;
    let seconds = (ms % 60_000) / 1000;
    format!("{minutes}m {seconds}s")
}

fn run_volumes() -> Result<()> {
    let volumes = discover_volumes().context("failed to discover Windows volumes")?;
    let mut stdout = io::stdout().lock();

    writeln!(
        stdout,
        "{:<8} {:<10} {:<12} {:<12}",
        "Drive", "Format", "Used", "Total"
    )?;
    for volume in volumes {
        let kind = match volume.kind {
            VolumeKind::Ntfs => "NTFS".to_owned(),
            VolumeKind::Other(name) => name,
            VolumeKind::Unknown => "unknown".to_owned(),
        };
        let total = volume.total_bytes.unwrap_or_default();
        let free = volume.free_bytes.unwrap_or_default();
        let used = total.saturating_sub(free);
        writeln!(
            stdout,
            "{:<8} {:<10} {:<12} {:<12}",
            volume.root,
            kind,
            format_size(used, false),
            volume
                .total_bytes
                .map(|bytes| format_size(bytes, false))
                .unwrap_or_else(|| "-".to_owned())
        )?;
    }

    Ok(())
}

fn run_ntfs_probe(volume: &str) -> Result<()> {
    let info = NtfsScanner::probe_volume(volume)?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{info}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        path::{MAIN_SEPARATOR, Path, PathBuf},
    };

    use diskloom_core::{FileGraphBuilder, FileKind};

    use super::{
        ScanCommand, ScannerMode, display_root_for_direct_scan, display_scan_path, drive_volume,
        format_count, normalize_args, query_filter, ranked_entry_ids, resolved_scan_path,
        scan_input_path, scan_needs_elevation, volume_arg_is_drive_root,
        write_duplicate_candidates,
    };

    #[test]
    fn drive_volume_should_accept_drive_root() {
        assert_eq!(drive_volume(Path::new("c:\\")).as_deref(), Some("C:"));
    }

    #[test]
    fn drive_volume_should_reject_folder_path() {
        assert_eq!(drive_volume(Path::new("c:\\Users")), None);
    }

    #[test]
    fn resolved_scan_path_should_treat_bare_drive_as_root() {
        assert_eq!(resolved_scan_path(Path::new("a:")), PathBuf::from("A:\\"));
    }

    #[test]
    fn scan_input_path_should_treat_bare_drive_as_root() {
        assert_eq!(scan_input_path(Path::new("A:")), PathBuf::from("A:\\"));
    }

    #[test]
    fn display_scan_path_should_treat_bare_drive_as_root() {
        assert_eq!(display_scan_path(Path::new("a:")), PathBuf::from("A:\\"));
    }

    #[test]
    fn direct_scan_display_root_should_not_scope_bare_drive() {
        let resolved_path = resolved_scan_path(Path::new("a:"));

        assert!(display_root_for_direct_scan(&resolved_path).is_none());
    }

    #[test]
    fn scan_input_path_should_keep_relative_folder_paths() {
        assert_eq!(
            scan_input_path(Path::new("target")),
            PathBuf::from("target")
        );
    }

    #[test]
    fn scan_needs_elevation_should_match_drive_backed_scan_targets() {
        assert!(scan_needs_elevation(Path::new("c:\\"), ScannerMode::Auto));
        assert!(scan_needs_elevation(Path::new("c:\\"), ScannerMode::Ntfs));
        assert!(!scan_needs_elevation(
            Path::new("c:\\"),
            ScannerMode::Fallback
        ));
        assert!(scan_needs_elevation(
            Path::new("c:\\Users"),
            ScannerMode::Auto
        ));
    }

    #[test]
    fn volume_arg_is_drive_root_should_accept_drive_arguments() {
        assert!(volume_arg_is_drive_root("c:"));
        assert!(volume_arg_is_drive_root("c:\\"));
        assert!(!volume_arg_is_drive_root("c:\\Users"));
    }

    #[test]
    fn query_filter_should_match_full_path_argument() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let src = builder
            .add_entry(Some(root), "src", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(src), "main.rs", FileKind::File, 5, 8, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "readme.md", FileKind::File, 3, 4, 0)
            .unwrap();
        let graph = builder.finish();
        let command = scan_command_with_path_filter("src");

        let filter = query_filter(&command).unwrap().compile().unwrap();
        let matches: Vec<_> = filter.matching_ids(&graph).collect();

        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn default_ranking_should_show_direct_children_without_scan_root() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, ".", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let target = builder
            .add_entry(Some(root), "target", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let src = builder
            .add_entry(Some(root), "src", FileKind::Directory, 120, 120, 0)
            .unwrap();
        let nested = builder
            .add_entry(Some(target), "deps", FileKind::Directory, 900, 900, 0)
            .unwrap();
        let graph = builder.finish();
        let command = scan_command();
        let filter = query_filter(&command).unwrap().compile().unwrap();

        let ids = ranked_entry_ids(&graph, &filter, &command, None);

        assert_eq!(ids, vec![target, src]);
        assert!(!ids.contains(&root));
        assert!(!ids.contains(&nested));
    }

    #[test]
    fn deep_ranking_should_include_descendants_without_scan_root() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, ".", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let target = builder
            .add_entry(Some(root), "target", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let nested = builder
            .add_entry(Some(target), "deps", FileKind::Directory, 900, 900, 0)
            .unwrap();
        let graph = builder.finish();
        let mut command = scan_command();
        command.deep = true;
        let filter = query_filter(&command).unwrap().compile().unwrap();

        let ids = ranked_entry_ids(&graph, &filter, &command, None);

        assert!(ids.contains(&target));
        assert!(ids.contains(&nested));
        assert!(!ids.contains(&root));
    }

    #[test]
    fn direct_scan_ranking_should_scope_to_display_root_children() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "C:\\", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let users = builder
            .add_entry(Some(root), "Users", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let windows = builder
            .add_entry(
                Some(root),
                "Windows",
                FileKind::Directory,
                10_000,
                10_000,
                0,
            )
            .unwrap();
        let profile = builder
            .add_entry(Some(users), "heman", FileKind::Directory, 2_000, 2_000, 0)
            .unwrap();
        let graph = builder.finish();
        let command = scan_command();
        let filter = query_filter(&command).unwrap().compile().unwrap();

        let ids = ranked_entry_ids(&graph, &filter, &command, Some(Path::new("C:\\Users")));

        assert_eq!(ids, vec![profile]);
        assert!(!ids.contains(&windows));
        assert!(!ids.contains(&users));
    }

    #[test]
    fn duplicate_output_should_include_candidate_paths() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let left = builder
            .add_entry(Some(root), "left", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let right = builder
            .add_entry(Some(root), "right", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(left), "copy.bin", FileKind::File, 10, 10, 100)
            .unwrap();
        builder
            .add_entry(Some(right), "COPY.bin", FileKind::File, 10, 10, 100)
            .unwrap();
        let graph = builder.finish();
        let mut output = Vec::new();

        write_duplicate_candidates(&mut output, &graph, 10, false).unwrap();
        let output = String::from_utf8(output).unwrap();
        let left_path = format!("root{MAIN_SEPARATOR}left{MAIN_SEPARATOR}copy.bin");
        let right_path = format!("root{MAIN_SEPARATOR}right{MAIN_SEPARATOR}COPY.bin");

        assert!(output.contains(&left_path));
        assert!(output.contains(&right_path));
    }

    #[test]
    fn normalize_args_should_default_to_scan_current_directory() {
        let args = normalize_args([OsString::from("dlm")]);

        assert_eq!(
            args,
            [
                OsString::from("dlm"),
                OsString::from("scan"),
                OsString::from(".")
            ]
        );
    }

    #[test]
    fn normalize_args_should_keep_default_path_with_hidden_elevation_output() {
        let args = normalize_args([
            OsString::from("dlm"),
            OsString::from("--elevated-output"),
            OsString::from("scan.txt"),
        ]);

        assert_eq!(
            args,
            [
                OsString::from("dlm"),
                OsString::from("scan"),
                OsString::from("."),
                OsString::from("--elevated-output"),
                OsString::from("scan.txt"),
            ]
        );
    }

    #[test]
    fn normalize_args_should_treat_bare_path_as_scan_path() {
        let args = normalize_args([OsString::from("dlm"), OsString::from("C:\\Users\\heman")]);

        assert_eq!(
            args,
            [
                OsString::from("dlm"),
                OsString::from("scan"),
                OsString::from("C:\\Users\\heman")
            ]
        );
    }

    #[test]
    fn format_count_should_group_thousands() {
        assert_eq!(format_count(2_107_718), "2,107,718");
    }

    fn scan_command() -> ScanCommand {
        ScanCommand {
            path: PathBuf::from("."),
            scanner: ScannerMode::Fallback,
            csv: None,
            name: None,
            path_filter: None,
            regex: false,
            extension: None,
            min_size: None,
            max_size: None,
            min_allocated: None,
            max_allocated: None,
            modified_after: None,
            modified_before: None,
            files_only: false,
            follow_symlinks: false,
            precise_allocated: false,
            duplicates: false,
            file_types: false,
            deep: false,
            limit: 15,
            summary_only: false,
            raw: false,
            elevated_output: None,
        }
    }

    fn scan_command_with_path_filter(path_filter: &str) -> ScanCommand {
        let mut command = scan_command();
        command.path_filter = Some(path_filter.to_owned());
        command
    }
}
