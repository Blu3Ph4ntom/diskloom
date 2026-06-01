use std::{
    fs::File,
    io::{self, Write},
    path::PathBuf,
    time::Instant,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use diskloom_core::{EntryFlags, EntryId, FileGraph};
use diskloom_dupes::find_duplicate_candidates;
use diskloom_export::{CsvExportOptions, export_csv};
use diskloom_ntfs::NtfsScanner;
use diskloom_query::{NameMatcher, QueryFilter, SortKey, SortOrder, sort_entries};
use diskloom_scan::{FallbackScanner, ScanOptions};
use diskloom_windows::{VolumeKind, discover_volumes};

#[derive(Debug, Parser)]
#[command(name = "diskloom")]
#[command(about = "DiskLoom disk analyzer")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Scan(ScanCommand),
    NtfsProbe { volume: String },
    Volumes,
}

#[derive(Debug, Parser)]
struct ScanCommand {
    path: PathBuf,

    #[arg(long)]
    csv: Option<PathBuf>,

    #[arg(long)]
    name: Option<String>,

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

    #[arg(long, default_value_t = 25)]
    limit: usize,
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
    let options = ScanOptions {
        root: command.path.clone(),
        follow_symlinks: command.follow_symlinks,
    };
    let (graph, summary) = FallbackScanner::scan(options)
        .with_context(|| format!("failed to scan {}", command.path.display()))?;
    let elapsed = started.elapsed();

    let filter = query_filter(&command)?.compile()?;
    let mut ids: Vec<_> = filter.matching_ids(&graph).collect();
    sort_entries(&graph, &mut ids, SortKey::Size, SortOrder::Descending);

    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "Scanned {} entries ({} files, {} directories, {} inaccessible) in {:.2?}",
        summary.entries, summary.files, summary.directories, summary.inaccessible, elapsed
    )?;
    writeln!(stdout, "Scanner: fallback traversal")?;
    writeln!(stdout)?;
    write_top_entries(&mut stdout, &graph, &ids, command.limit)?;

    if command.duplicates {
        writeln!(stdout)?;
        write_duplicate_candidates(&mut stdout, &graph, command.limit)?;
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

fn query_filter(command: &ScanCommand) -> Result<QueryFilter> {
    let name = match (&command.name, command.regex) {
        (Some(pattern), true) => Some(NameMatcher::regex(pattern)?),
        (Some(needle), false) => Some(NameMatcher::contains(needle.as_str())),
        (None, _) => None,
    };

    Ok(QueryFilter {
        name,
        extension: command.extension.clone(),
        path: None,
        min_size: command.min_size,
        max_size: command.max_size,
        min_allocated: command.min_allocated,
        max_allocated: command.max_allocated,
        modified_after: command.modified_after,
        modified_before: command.modified_before,
        include_directories: !command.files_only,
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
