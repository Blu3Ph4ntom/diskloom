use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use diskloom_core::{EntryFlags, FileGraph};
use diskloom_ntfs::NtfsScanner;
use diskloom_scan::{FallbackScanner, ScanOptions, ScanSummary};

#[derive(Debug, Parser)]
#[command(name = "diskloom-bench")]
#[command(about = "DiskLoom benchmark harness")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Scan {
        path: PathBuf,
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        #[arg(long, default_value_t = 10)]
        sample_ms: u64,
        #[arg(long, value_enum, default_value = "fallback")]
        scanner: ScannerMode,
    },
    Dataset {
        root: PathBuf,
        #[arg(long, default_value_t = 100)]
        dirs: usize,
        #[arg(long, default_value_t = 100)]
        files_per_dir: usize,
        #[arg(long, default_value_t = 0)]
        bytes_per_file: usize,
    },
    Summarize {
        csv: PathBuf,
    },
    ComparePublic {
        csv: PathBuf,
        #[arg(long, value_enum)]
        claim: PublicClaimId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ScannerMode {
    Auto,
    Fallback,
    Ntfs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PublicClaimId {
    #[value(name = "wiztree-hdd-25gb")]
    WizTreeHdd25Gb,
    #[value(name = "wiztree-ssd-460gb")]
    WizTreeSsd460Gb,
}

#[derive(Debug, Clone, Copy)]
struct ScanMeasurement {
    iteration: usize,
    scanner: &'static str,
    fallback: bool,
    elapsed_ms: u128,
    entries: u64,
    files: u64,
    directories: u64,
    inaccessible: u64,
    peak_working_set_bytes: u64,
    peak_private_bytes: u64,
    final_working_set_bytes: u64,
    final_private_bytes: u64,
    peak_private_bytes_per_million_entries: u64,
    memory_samples: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct MemorySample {
    working_set_bytes: u64,
    private_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct MemoryPeak {
    peak_working_set_bytes: u64,
    peak_private_bytes: u64,
    final_working_set_bytes: u64,
    final_private_bytes: u64,
    samples: u64,
}

#[derive(Debug, Clone, Copy)]
struct ScanRun {
    elapsed_ms: u128,
    scanner: &'static str,
    fallback: bool,
    summary: ScanSummary,
}

#[derive(Debug, Clone, Copy)]
struct MeasuredScan {
    run: ScanRun,
    memory: MemoryPeak,
}

#[derive(Debug, Clone)]
struct ParsedMeasurement {
    scanner: String,
    fallback: bool,
    elapsed_ms: u128,
    entries: u64,
    peak_working_set_bytes: u64,
    peak_private_bytes: u64,
    peak_private_bytes_per_million_entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MeasurementSummary {
    runs: usize,
    scanners: String,
    fallback_runs: usize,
    entries_min: u64,
    entries_max: u64,
    elapsed_ms_min: u128,
    elapsed_ms_median: u128,
    elapsed_ms_max: u128,
    peak_working_set_bytes_max: u64,
    peak_private_bytes_max: u64,
    peak_private_bytes_per_million_entries_max: u64,
}

#[derive(Debug, Clone, Copy)]
struct PublicClaim {
    id: &'static str,
    source_url: &'static str,
    context: &'static str,
    elapsed_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicComparison {
    claim_id: &'static str,
    claim_source_url: &'static str,
    claim_context: &'static str,
    claim_elapsed_ms: u128,
    diskloom_runs: usize,
    diskloom_scanners: String,
    diskloom_fallback_runs: usize,
    diskloom_elapsed_ms_min: u128,
    diskloom_elapsed_ms_median: u128,
    diskloom_elapsed_ms_max: u128,
    diskloom_peak_private_bytes_max: u64,
    diskloom_vs_claim_ratio: String,
    validity: &'static str,
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Scan {
            path,
            iterations,
            sample_ms,
            scanner,
        } => run_scan(path, iterations, sample_ms, scanner),
        Command::Dataset {
            root,
            dirs,
            files_per_dir,
            bytes_per_file,
        } => create_dataset(root, dirs, files_per_dir, bytes_per_file),
        Command::Summarize { csv } => summarize_measurements(csv),
        Command::ComparePublic { csv, claim } => compare_public_claim(csv, claim),
    }
}

fn run_scan(path: PathBuf, iterations: usize, sample_ms: u64, scanner: ScannerMode) -> Result<()> {
    let mut measurements = Vec::with_capacity(iterations);
    let sample_interval = Duration::from_millis(sample_ms.max(1));

    for iteration in 1..=iterations {
        let scan = run_measured_scan(path.clone(), sample_interval, scanner)
            .with_context(|| format!("scan failed for {}", path.display()))?;
        measurements.push(measurement_from_run(iteration, scan));
    }

    write_measurements(&mut io::stdout().lock(), &measurements)?;
    Ok(())
}

fn run_measured_scan(
    path: PathBuf,
    sample_interval: Duration,
    scanner: ScannerMode,
) -> Result<MeasuredScan> {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = scan_once(path, scanner).map_err(|error| error.to_string());
        let _ = sender.send(result);
    });

    let mut memory = MemoryPeak::default();
    let run = loop {
        memory.observe(current_process_memory()?);
        match receiver.recv_timeout(sample_interval) {
            Ok(result) => break result.map_err(|error| anyhow!(error))?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("scan worker stopped"));
            }
        }
    };
    handle.join().map_err(|_| anyhow!("scan worker panicked"))?;
    memory.observe(current_process_memory()?);

    Ok(MeasuredScan { run, memory })
}

fn scan_once(path: PathBuf, scanner: ScannerMode) -> Result<ScanRun> {
    let started = Instant::now();
    let outcome = scan_path(path, scanner)?;

    Ok(ScanRun {
        elapsed_ms: started.elapsed().as_millis(),
        scanner: outcome.scanner,
        fallback: outcome.fallback,
        summary: outcome.summary,
    })
}

#[derive(Debug, Clone, Copy)]
struct ScanOutcome {
    scanner: &'static str,
    fallback: bool,
    summary: ScanSummary,
}

fn scan_path(path: PathBuf, scanner: ScannerMode) -> Result<ScanOutcome> {
    match scanner {
        ScannerMode::Fallback => scan_fallback(path, false),
        ScannerMode::Ntfs => scan_ntfs(&path),
        ScannerMode::Auto => {
            if drive_volume(&path).is_some() {
                match scan_ntfs(&path) {
                    Ok(outcome) => Ok(outcome),
                    Err(_) => scan_fallback(path, true),
                }
            } else {
                scan_fallback(path, false)
            }
        }
    }
}

fn scan_fallback(path: PathBuf, fallback: bool) -> Result<ScanOutcome> {
    let (_, summary) = FallbackScanner::scan(ScanOptions {
        root: path,
        follow_symlinks: false,
    })?;

    Ok(ScanOutcome {
        scanner: "fallback",
        fallback,
        summary,
    })
}

fn scan_ntfs(path: &Path) -> Result<ScanOutcome> {
    let volume = drive_volume(path).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let graph = NtfsScanner::scan_volume(&volume)?;
    Ok(ScanOutcome {
        scanner: "ntfs",
        fallback: false,
        summary: summary_from_graph(&graph),
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

fn measurement_from_run(iteration: usize, scan: MeasuredScan) -> ScanMeasurement {
    let summary = scan.run.summary;
    ScanMeasurement {
        iteration,
        scanner: scan.run.scanner,
        fallback: scan.run.fallback,
        elapsed_ms: scan.run.elapsed_ms,
        entries: summary.entries,
        files: summary.files,
        directories: summary.directories,
        inaccessible: summary.inaccessible,
        peak_working_set_bytes: scan.memory.peak_working_set_bytes,
        peak_private_bytes: scan.memory.peak_private_bytes,
        final_working_set_bytes: scan.memory.final_working_set_bytes,
        final_private_bytes: scan.memory.final_private_bytes,
        peak_private_bytes_per_million_entries: per_million(
            scan.memory.peak_private_bytes,
            summary.entries,
        ),
        memory_samples: scan.memory.samples,
    }
}

impl MemoryPeak {
    fn observe(&mut self, sample: MemorySample) {
        self.samples += 1;
        self.final_working_set_bytes = sample.working_set_bytes;
        self.final_private_bytes = sample.private_bytes;
        self.peak_working_set_bytes = self.peak_working_set_bytes.max(sample.working_set_bytes);
        self.peak_private_bytes = self.peak_private_bytes.max(sample.private_bytes);
    }
}

fn per_million(value: u64, count: u64) -> u64 {
    if count == 0 {
        return 0;
    }
    ((u128::from(value) * 1_000_000) / u128::from(count)) as u64
}

#[cfg(windows)]
fn current_process_memory() -> Result<MemorySample> {
    use std::mem::size_of;

    use windows::Win32::System::{
        ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::GetCurrentProcess,
    };

    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..PROCESS_MEMORY_COUNTERS_EX::default()
    };

    // SAFETY: GetCurrentProcess returns the current process pseudo-handle. The counters buffer
    // is initialized with its real size and is valid for the duration of the call.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    }
    .context("failed to read process memory counters")?;

    Ok(MemorySample {
        working_set_bytes: counters.WorkingSetSize as u64,
        private_bytes: counters.PrivateUsage as u64,
    })
}

#[cfg(not(windows))]
fn current_process_memory() -> Result<MemorySample> {
    Ok(MemorySample::default())
}

fn create_dataset(
    root: PathBuf,
    dirs: usize,
    files_per_dir: usize,
    bytes_per_file: usize,
) -> Result<()> {
    fs::create_dir_all(&root).with_context(|| format!("failed to create {}", root.display()))?;
    let payload = vec![0_u8; bytes_per_file];

    for dir_idx in 0..dirs {
        let dir = root.join(format!("dir-{dir_idx:06}"));
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        for file_idx in 0..files_per_dir {
            let path = dir.join(format!("file-{file_idx:06}.bin"));
            let mut file = File::create(&path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            file.write_all(&payload)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
    }

    println!(
        "created {} files in {} directories under {}",
        dirs.saturating_mul(files_per_dir),
        dirs,
        root.display()
    );
    Ok(())
}

fn summarize_measurements(path: PathBuf) -> Result<()> {
    let input =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let rows = parse_measurements(&input)?;
    let summary = summarize_rows(&rows)?;
    write_summary(&mut io::stdout().lock(), &summary)?;
    Ok(())
}

fn compare_public_claim(path: PathBuf, claim_id: PublicClaimId) -> Result<()> {
    let input =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let rows = parse_measurements(&input)?;
    let summary = summarize_rows(&rows)?;
    let comparison = compare_summary_to_claim(&summary, public_claim(claim_id));
    write_public_comparison(&mut io::stdout().lock(), &comparison)?;
    Ok(())
}

fn write_measurements(writer: &mut impl Write, measurements: &[ScanMeasurement]) -> Result<()> {
    writeln!(
        writer,
        "iteration,scanner,fallback,elapsed_ms,entries,files,directories,inaccessible,peak_working_set_bytes,peak_private_bytes,final_working_set_bytes,final_private_bytes,peak_private_bytes_per_million_entries,memory_samples"
    )?;
    for measurement in measurements {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            measurement.iteration,
            measurement.scanner,
            u8::from(measurement.fallback),
            measurement.elapsed_ms,
            measurement.entries,
            measurement.files,
            measurement.directories,
            measurement.inaccessible,
            measurement.peak_working_set_bytes,
            measurement.peak_private_bytes,
            measurement.final_working_set_bytes,
            measurement.final_private_bytes,
            measurement.peak_private_bytes_per_million_entries,
            measurement.memory_samples
        )?;
    }
    Ok(())
}

fn parse_measurements(input: &str) -> Result<Vec<ParsedMeasurement>> {
    let mut lines = input.lines().filter(|line| !line.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| anyhow!("measurement CSV is empty"))?;
    let headers: Vec<_> = header.split(',').collect();

    let scanner_idx = field_index(&headers, "scanner")?;
    let fallback_idx = field_index(&headers, "fallback")?;
    let elapsed_idx = field_index(&headers, "elapsed_ms")?;
    let entries_idx = field_index(&headers, "entries")?;
    let peak_ws_idx = field_index(&headers, "peak_working_set_bytes")?;
    let peak_private_idx = field_index(&headers, "peak_private_bytes")?;
    let per_million_idx = field_index(&headers, "peak_private_bytes_per_million_entries")?;

    lines
        .enumerate()
        .map(|(idx, line)| {
            let fields: Vec<_> = line.split(',').collect();
            Ok(ParsedMeasurement {
                scanner: field(&fields, scanner_idx, idx)?.to_owned(),
                fallback: parse_bool_field(field(&fields, fallback_idx, idx)?)?,
                elapsed_ms: parse_field(field(&fields, elapsed_idx, idx)?, "elapsed_ms")?,
                entries: parse_field(field(&fields, entries_idx, idx)?, "entries")?,
                peak_working_set_bytes: parse_field(
                    field(&fields, peak_ws_idx, idx)?,
                    "peak_working_set_bytes",
                )?,
                peak_private_bytes: parse_field(
                    field(&fields, peak_private_idx, idx)?,
                    "peak_private_bytes",
                )?,
                peak_private_bytes_per_million_entries: parse_field(
                    field(&fields, per_million_idx, idx)?,
                    "peak_private_bytes_per_million_entries",
                )?,
            })
        })
        .collect()
}

fn field_index(headers: &[&str], name: &str) -> Result<usize> {
    headers
        .iter()
        .position(|candidate| *candidate == name)
        .ok_or_else(|| anyhow!("missing CSV field `{name}`"))
}

fn field<'a>(fields: &'a [&str], idx: usize, row_idx: usize) -> Result<&'a str> {
    fields
        .get(idx)
        .copied()
        .ok_or_else(|| anyhow!("row {} is missing field {}", row_idx + 1, idx))
}

fn parse_field<T>(value: &str, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| anyhow!("invalid `{name}` value `{value}`: {error}"))
}

fn parse_bool_field(value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(anyhow!("invalid `fallback` value `{value}`")),
    }
}

fn summarize_rows(rows: &[ParsedMeasurement]) -> Result<MeasurementSummary> {
    if rows.is_empty() {
        return Err(anyhow!("measurement CSV has no data rows"));
    }

    let mut scanners = BTreeSet::new();
    let mut elapsed: Vec<_> = rows.iter().map(|row| row.elapsed_ms).collect();
    let entries: Vec<_> = rows.iter().map(|row| row.entries).collect();
    for row in rows {
        scanners.insert(row.scanner.as_str());
    }

    Ok(MeasurementSummary {
        runs: rows.len(),
        scanners: scanners.into_iter().collect::<Vec<_>>().join("+"),
        fallback_runs: rows.iter().filter(|row| row.fallback).count(),
        entries_min: *entries.iter().min().unwrap_or(&0),
        entries_max: *entries.iter().max().unwrap_or(&0),
        elapsed_ms_min: *elapsed.iter().min().unwrap_or(&0),
        elapsed_ms_median: median_u128(&mut elapsed),
        elapsed_ms_max: *elapsed.iter().max().unwrap_or(&0),
        peak_working_set_bytes_max: rows
            .iter()
            .map(|row| row.peak_working_set_bytes)
            .max()
            .unwrap_or(0),
        peak_private_bytes_max: rows
            .iter()
            .map(|row| row.peak_private_bytes)
            .max()
            .unwrap_or(0),
        peak_private_bytes_per_million_entries_max: rows
            .iter()
            .map(|row| row.peak_private_bytes_per_million_entries)
            .max()
            .unwrap_or(0),
    })
}

fn median_u128(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2
    } else {
        values[mid]
    }
}

fn write_summary(writer: &mut impl Write, summary: &MeasurementSummary) -> Result<()> {
    writeln!(
        writer,
        "runs,scanners,fallback_runs,entries_min,entries_max,elapsed_ms_min,elapsed_ms_median,elapsed_ms_max,peak_working_set_bytes_max,peak_private_bytes_max,peak_private_bytes_per_million_entries_max"
    )?;
    writeln!(
        writer,
        "{},{},{},{},{},{},{},{},{},{},{}",
        summary.runs,
        summary.scanners,
        summary.fallback_runs,
        summary.entries_min,
        summary.entries_max,
        summary.elapsed_ms_min,
        summary.elapsed_ms_median,
        summary.elapsed_ms_max,
        summary.peak_working_set_bytes_max,
        summary.peak_private_bytes_max,
        summary.peak_private_bytes_per_million_entries_max
    )?;
    Ok(())
}

fn public_claim(id: PublicClaimId) -> PublicClaim {
    match id {
        PublicClaimId::WizTreeHdd25Gb => PublicClaim {
            id: "wiztree-hdd-25gb",
            source_url: "https://diskanalyzer.com/wiztree-vs-windirstat",
            context: "25GB_NTFS_HDD_Acer_laptop_Windows_XP_vendor_test",
            elapsed_ms: 4_340,
        },
        PublicClaimId::WizTreeSsd460Gb => PublicClaim {
            id: "wiztree-ssd-460gb",
            source_url: "https://diskanalyzer.com/wiztree-vs-windirstat",
            context: "460GB_NTFS_SSD_ASUS_laptop_Windows_10_vendor_test",
            elapsed_ms: 5_230,
        },
    }
}

fn compare_summary_to_claim(summary: &MeasurementSummary, claim: PublicClaim) -> PublicComparison {
    PublicComparison {
        claim_id: claim.id,
        claim_source_url: claim.source_url,
        claim_context: claim.context,
        claim_elapsed_ms: claim.elapsed_ms,
        diskloom_runs: summary.runs,
        diskloom_scanners: summary.scanners.clone(),
        diskloom_fallback_runs: summary.fallback_runs,
        diskloom_elapsed_ms_min: summary.elapsed_ms_min,
        diskloom_elapsed_ms_median: summary.elapsed_ms_median,
        diskloom_elapsed_ms_max: summary.elapsed_ms_max,
        diskloom_peak_private_bytes_max: summary.peak_private_bytes_max,
        diskloom_vs_claim_ratio: ratio_decimal(summary.elapsed_ms_median, claim.elapsed_ms),
        validity: "reference_only_vendor_claim_not_same_machine",
    }
}

fn ratio_decimal(numerator: u128, denominator: u128) -> String {
    if denominator == 0 {
        return "n/a".to_owned();
    }
    let scaled = numerator.saturating_mul(1_000) / denominator;
    format!("{}.{:03}", scaled / 1_000, scaled % 1_000)
}

fn write_public_comparison(writer: &mut impl Write, comparison: &PublicComparison) -> Result<()> {
    writeln!(
        writer,
        "claim_id,claim_source_url,claim_context,claim_elapsed_ms,diskloom_runs,diskloom_scanners,diskloom_fallback_runs,diskloom_elapsed_ms_min,diskloom_elapsed_ms_median,diskloom_elapsed_ms_max,diskloom_peak_private_bytes_max,diskloom_vs_claim_ratio,validity"
    )?;
    writeln!(
        writer,
        "{},{},{},{},{},{},{},{},{},{},{},{},{}",
        comparison.claim_id,
        comparison.claim_source_url,
        comparison.claim_context,
        comparison.claim_elapsed_ms,
        comparison.diskloom_runs,
        comparison.diskloom_scanners,
        comparison.diskloom_fallback_runs,
        comparison.diskloom_elapsed_ms_min,
        comparison.diskloom_elapsed_ms_median,
        comparison.diskloom_elapsed_ms_max,
        comparison.diskloom_peak_private_bytes_max,
        comparison.diskloom_vs_claim_ratio,
        comparison.validity
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        MeasurementSummary, PublicClaimId, ScanMeasurement, compare_summary_to_claim,
        parse_measurements, per_million, public_claim, ratio_decimal, summarize_rows,
        write_measurements, write_public_comparison, write_summary,
    };

    #[test]
    fn write_measurements_should_emit_csv_rows() {
        let measurements = [ScanMeasurement {
            iteration: 1,
            scanner: "fallback",
            fallback: false,
            elapsed_ms: 10,
            entries: 3,
            files: 2,
            directories: 1,
            inaccessible: 0,
            peak_working_set_bytes: 100,
            peak_private_bytes: 90,
            final_working_set_bytes: 80,
            final_private_bytes: 70,
            peak_private_bytes_per_million_entries: 30_000_000,
            memory_samples: 4,
        }];
        let mut output = Vec::new();

        write_measurements(&mut output, &measurements).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("1,fallback,0,10,3,2,1,0,100,90,80,70,30000000,4"));
    }

    #[test]
    fn per_million_should_scale_without_floating_point() {
        assert_eq!(per_million(90, 3), 30_000_000);
        assert_eq!(per_million(90, 0), 0);
    }

    #[test]
    fn summarize_rows_should_compute_range_and_median() {
        let input = "\
iteration,scanner,fallback,elapsed_ms,entries,files,directories,inaccessible,peak_working_set_bytes,peak_private_bytes,final_working_set_bytes,final_private_bytes,peak_private_bytes_per_million_entries,memory_samples
1,fallback,0,10,3,2,1,0,100,90,80,70,30000000,4
2,fallback,0,20,3,2,1,0,110,95,80,70,31666666,4
3,fallback,0,30,3,2,1,0,105,92,80,70,30666666,4
";

        let rows = parse_measurements(input).unwrap();
        let summary = summarize_rows(&rows).unwrap();

        assert_eq!(summary.elapsed_ms_median, 20);
        assert_eq!(summary.peak_private_bytes_max, 95);
    }

    #[test]
    fn write_summary_should_emit_single_csv_row() {
        let summary = MeasurementSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 10,
            elapsed_ms_median: 20,
            elapsed_ms_max: 30,
            peak_working_set_bytes_max: 110,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let mut output = Vec::new();

        write_summary(&mut output, &summary).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("3,fallback,0,3,3,10,20,30,110,95,31666666"));
    }

    #[test]
    fn compare_summary_to_claim_should_mark_reference_only() {
        let summary = MeasurementSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 500,
            elapsed_ms_median: 1_046,
            elapsed_ms_max: 1_100,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };

        let comparison =
            compare_summary_to_claim(&summary, public_claim(PublicClaimId::WizTreeSsd460Gb));

        assert_eq!(comparison.diskloom_vs_claim_ratio, "0.200");
        assert_eq!(
            comparison.validity,
            "reference_only_vendor_claim_not_same_machine"
        );
    }

    #[test]
    fn write_public_comparison_should_emit_csv_row() {
        let summary = MeasurementSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 500,
            elapsed_ms_median: 1_046,
            elapsed_ms_max: 1_100,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let comparison =
            compare_summary_to_claim(&summary, public_claim(PublicClaimId::WizTreeSsd460Gb));
        let mut output = Vec::new();

        write_public_comparison(&mut output, &comparison).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("wiztree-ssd-460gb"));
        assert!(output.contains("reference_only_vendor_claim_not_same_machine"));
    }

    #[test]
    fn ratio_decimal_should_format_fixed_precision() {
        assert_eq!(ratio_decimal(1_046, 5_230), "0.200");
        assert_eq!(ratio_decimal(5_230, 5_230), "1.000");
        assert_eq!(ratio_decimal(5_230, 0), "n/a");
    }
}
