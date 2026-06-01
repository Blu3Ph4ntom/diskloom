use std::{
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use diskloom_core::{EntryFlags, EntryId, FileGraph};
use diskloom_dupes::find_duplicate_candidates;
use diskloom_export::{CsvExportOptions, export_csv};
use diskloom_ntfs::NtfsScanner;
use diskloom_query::{
    FileTypeStat, NameMatcher, QueryFilter, SortKey, SortOrder, file_type_stats, sort_entries,
};
use diskloom_scan::{FallbackScanner, ScanOptions, ScanSummary};
use diskloom_windows::{VolumeKind, discover_volumes};

#[derive(Debug, Parser)]
#[command(name = "diskloom")]
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
    duplicates: bool,

    #[arg(long)]
    file_types: bool,

    #[arg(long, default_value_t = 25)]
    limit: usize,
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
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Scan(command) => run_scan(command),
        Command::NtfsProbe { volume } => run_ntfs_probe(&volume),
        Command::Volumes => run_volumes(),
    }
}

fn run_scan(command: ScanCommand) -> Result<()> {
    let started = Instant::now();
    let outcome = scan_path(&command)
        .with_context(|| format!("failed to scan {}", command.path.display()))?;
    let elapsed = started.elapsed();
    let graph = outcome.graph;
    let summary = outcome.summary;

    let filter = query_filter(&command)?.compile()?;
    let mut ids: Vec<_> = filter.matching_ids(&graph).collect();
    sort_entries(&graph, &mut ids, SortKey::Size, SortOrder::Descending);

    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "Scanned {} entries ({} files, {} directories, {} inaccessible) in {:.2?}",
        summary.entries, summary.files, summary.directories, summary.inaccessible, elapsed
    )?;
    writeln!(stdout, "Scanner: {}", outcome.scanner_label)?;
    if let Some(reason) = outcome.fallback_reason {
        writeln!(stdout, "Fallback reason: {reason}")?;
    }
    writeln!(stdout)?;
    write_top_entries(&mut stdout, &graph, &ids, command.limit)?;

    if command.duplicates {
        writeln!(stdout)?;
        write_duplicate_candidates(&mut stdout, &graph, command.limit)?;
    }

    if command.file_types {
        writeln!(stdout)?;
        write_file_type_stats(&mut stdout, &file_type_stats(&graph, command.limit))?;
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
        writeln!(stdout)?;
        writeln!(stdout, "CSV exported to {}", csv_path.display())?;
    }

    Ok(())
}

fn scan_path(command: &ScanCommand) -> Result<ScanOutcome> {
    match command.scanner {
        ScannerMode::Fallback => scan_fallback(command, None),
        ScannerMode::Ntfs => scan_ntfs(command).map_err(Into::into),
        ScannerMode::Auto => {
            if drive_volume(&command.path).is_some() {
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

fn scan_fallback(command: &ScanCommand, fallback_reason: Option<String>) -> Result<ScanOutcome> {
    let (graph, summary) = FallbackScanner::scan(ScanOptions {
        root: command.path.clone(),
        follow_symlinks: command.follow_symlinks,
    })?;
    Ok(ScanOutcome {
        graph,
        summary,
        scanner_label: "fallback traversal",
        fallback_reason,
    })
}

fn scan_ntfs(command: &ScanCommand) -> Result<ScanOutcome, diskloom_ntfs::NtfsScanError> {
    let volume =
        drive_volume(&command.path).unwrap_or_else(|| command.path.to_string_lossy().into_owned());
    let graph = NtfsScanner::scan_volume(&volume)?;
    let summary = summary_from_graph(&graph);
    Ok(ScanOutcome {
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

fn write_top_entries(
    writer: &mut impl Write,
    graph: &FileGraph,
    ids: &[EntryId],
    limit: usize,
) -> Result<()> {
    writeln!(writer, "Top entries by total size:")?;
    writeln!(writer, "size\tallocated\tkind\tpath")?;

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
            "{}\t{}\t{}\t{}",
            stats.total_size.bytes(),
            stats.total_allocated.bytes(),
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
) -> Result<()> {
    let candidates = find_duplicate_candidates(graph);
    writeln!(writer, "Duplicate candidates by size/name/date:")?;

    for candidate in candidates.iter().take(limit) {
        writeln!(
            writer,
            "{} bytes\t{} entries\t{}",
            candidate.size,
            candidate.entries.len(),
            candidate.name
        )?;
    }

    Ok(())
}

fn write_file_type_stats(writer: &mut impl Write, stats: &[FileTypeStat]) -> Result<()> {
    writeln!(writer, "File types by size:")?;
    writeln!(writer, "size\tallocated\tfiles\textension")?;

    for stat in stats {
        writeln!(
            writer,
            "{}\t{}\t{}\t{}",
            stat.size, stat.allocated, stat.files, stat.extension
        )?;
    }

    Ok(())
}

fn run_volumes() -> Result<()> {
    let volumes = discover_volumes().context("failed to discover Windows volumes")?;
    let mut stdout = io::stdout().lock();

    for volume in volumes {
        let kind = match volume.kind {
            VolumeKind::Ntfs => "NTFS".to_owned(),
            VolumeKind::Other(name) => name,
            VolumeKind::Unknown => "unknown".to_owned(),
        };
        writeln!(stdout, "{}\t{}", volume.root, kind)?;
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
    use std::path::{Path, PathBuf};

    use diskloom_core::{FileGraphBuilder, FileKind};

    use super::{ScanCommand, ScannerMode, drive_volume, query_filter};

    #[test]
    fn drive_volume_should_accept_drive_root() {
        assert_eq!(drive_volume(Path::new("c:\\")).as_deref(), Some("C:"));
    }

    #[test]
    fn drive_volume_should_reject_folder_path() {
        assert_eq!(drive_volume(Path::new("c:\\Users")), None);
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

    fn scan_command_with_path_filter(path_filter: &str) -> ScanCommand {
        ScanCommand {
            path: PathBuf::from("."),
            scanner: ScannerMode::Fallback,
            csv: None,
            name: None,
            path_filter: Some(path_filter.to_owned()),
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
            duplicates: false,
            file_types: false,
            limit: 25,
        }
    }
}
