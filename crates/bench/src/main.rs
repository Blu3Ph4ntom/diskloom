use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use diskloom_core::{EntryFlags, FileGraph};
use diskloom_export::{CsvExportOptions, export_csv};
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
        #[arg(long, default_value_t = 1024)]
        progress_every: u64,
        #[arg(long, value_enum, default_value = "fallback")]
        scanner: ScannerMode,
    },
    Export {
        path: PathBuf,
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        #[arg(long, default_value_t = 10)]
        sample_ms: u64,
        #[arg(long, value_enum, default_value = "fallback")]
        scanner: ScannerMode,
        #[arg(long, default_value_t = true)]
        include_directories: bool,
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    Suite {
        path: PathBuf,
        output_dir: PathBuf,
        #[arg(long, default_value = "unspecified")]
        dataset_label: String,
        #[arg(long, default_value = "unknown")]
        cache_state: String,
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        #[arg(long, default_value_t = 10)]
        sample_ms: u64,
        #[arg(long, default_value_t = 1024)]
        progress_every: u64,
        #[arg(long, value_enum, default_value = "fallback")]
        scanner: ScannerMode,
        #[arg(long, default_value_t = true)]
        include_directories: bool,
        #[arg(long, value_enum)]
        claim: Vec<PublicClaimId>,
        #[arg(long)]
        competitor_csv: Option<PathBuf>,
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
        claim: Vec<PublicClaimId>,
    },
    CompareCompetitor {
        csv: PathBuf,
        competitor_csv: PathBuf,
        #[arg(long, default_value = "unspecified")]
        dataset_label: String,
        #[arg(long, default_value = "unknown")]
        cache_state: String,
    },
    CompetitorTemplate {
        #[arg(long)]
        examples: bool,
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
    #[value(name = "wiztree-ssd-500gb-typical")]
    WizTreeSsd500GbTypical,
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
    first_result_ms: u128,
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

#[derive(Debug, Clone, Copy)]
struct ExportMeasurement {
    iteration: usize,
    scanner: &'static str,
    fallback: bool,
    scan_elapsed_ms: u128,
    export_elapsed_ms: u128,
    total_elapsed_ms: u128,
    export_bytes: u64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportSummary {
    runs: usize,
    scanners: String,
    fallback_runs: usize,
    entries_min: u64,
    entries_max: u64,
    export_bytes_min: u64,
    export_bytes_max: u64,
    scan_elapsed_ms_median: u128,
    export_elapsed_ms_min: u128,
    export_elapsed_ms_median: u128,
    export_elapsed_ms_max: u128,
    total_elapsed_ms_median: u128,
    peak_working_set_bytes_max: u64,
    peak_private_bytes_max: u64,
    peak_private_bytes_per_million_entries_max: u64,
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
    first_result_ms: u128,
    scanner: &'static str,
    fallback: bool,
    summary: ScanSummary,
}

#[derive(Debug, Clone, Copy)]
struct MeasuredScan {
    run: ScanRun,
    memory: MemoryPeak,
}

#[derive(Debug, Clone, Copy)]
struct ExportRun {
    scan_elapsed_ms: u128,
    export_elapsed_ms: u128,
    total_elapsed_ms: u128,
    export_bytes: u64,
    scanner: &'static str,
    fallback: bool,
    summary: ScanSummary,
}

#[derive(Debug, Clone, Copy)]
struct MeasuredExport {
    run: ExportRun,
    memory: MemoryPeak,
}

#[derive(Debug, Clone)]
struct ParsedMeasurement {
    scanner: String,
    fallback: bool,
    elapsed_ms: u128,
    first_result_ms: u128,
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
    first_result_ms_min: u128,
    first_result_ms_median: u128,
    first_result_ms_max: u128,
    peak_working_set_bytes_max: u64,
    peak_private_bytes_max: u64,
    peak_private_bytes_per_million_entries_max: u64,
}

#[derive(Debug, Clone, Copy)]
struct PublicClaim {
    id: &'static str,
    source_url: &'static str,
    context: &'static str,
    scan_scope: ClaimScanScope,
    elapsed_ms_min: u128,
    elapsed_ms_max: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimScanScope {
    NtfsMft,
}

impl ClaimScanScope {
    fn label(self) -> &'static str {
        match self {
            Self::NtfsMft => "ntfs_mft",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicComparison {
    claim_id: &'static str,
    claim_source_url: &'static str,
    claim_context: &'static str,
    claim_scan_scope: &'static str,
    claim_elapsed_ms_min: u128,
    claim_elapsed_ms_max: u128,
    comparison_applicability: &'static str,
    diskloom_runs: usize,
    diskloom_scanners: String,
    diskloom_fallback_runs: usize,
    diskloom_elapsed_ms_min: u128,
    diskloom_elapsed_ms_median: u128,
    diskloom_elapsed_ms_max: u128,
    diskloom_peak_private_bytes_max: u64,
    diskloom_vs_claim_min_ratio: String,
    diskloom_vs_claim_max_ratio: String,
    diskloom_median_position: &'static str,
    validity: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompetitorMeasurement {
    tool: String,
    version: String,
    dataset_label: String,
    cache_state: String,
    scanner_scope: String,
    elapsed_ms: u128,
    peak_private_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CompetitorKey {
    tool: String,
    version: String,
    dataset_label: String,
    cache_state: String,
    scanner_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompetitorSummary {
    key: CompetitorKey,
    runs: usize,
    elapsed_ms_min: u128,
    elapsed_ms_median: u128,
    elapsed_ms_max: u128,
    peak_private_bytes_max: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SameMachineComparison {
    tool: String,
    version: String,
    dataset_label: String,
    cache_state: String,
    scanner_scope: String,
    context_match: &'static str,
    scanner_scope_match: &'static str,
    competitor_runs: usize,
    diskloom_runs: usize,
    competitor_elapsed_ms_min: u128,
    competitor_elapsed_ms_median: u128,
    competitor_elapsed_ms_max: u128,
    diskloom_elapsed_ms_min: u128,
    diskloom_elapsed_ms_median: u128,
    diskloom_elapsed_ms_max: u128,
    diskloom_vs_competitor_median_ratio: String,
    competitor_peak_private_bytes_max: Option<u64>,
    diskloom_peak_private_bytes_max: u64,
    diskloom_private_bytes_delta: Option<i128>,
    validity: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchmarkEnvironment {
    volume_root: String,
    file_system: String,
    drive_type: String,
    shell_elevated: String,
    windows_version: String,
    logical_cpus: String,
    physical_memory_bytes: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuiteRunContext {
    command_line: String,
    git_revision: String,
    git_dirty: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuditStatus {
    Pass,
    Warning,
    Fail,
}

impl AuditStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warning => "warning",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuiteAuditRow {
    check: &'static str,
    status: AuditStatus,
    message: String,
}

impl SuiteAuditRow {
    fn new(check: &'static str, status: AuditStatus, message: impl Into<String>) -> Self {
        Self {
            check,
            status,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
struct SuiteReport<'a> {
    path: &'a Path,
    output_dir: &'a Path,
    dataset_label: &'a str,
    cache_state: &'a str,
    scanner: ScannerMode,
    iterations: usize,
    sample_ms: u64,
    progress_every: u64,
    include_directories: bool,
    run_context: &'a SuiteRunContext,
    environment: &'a BenchmarkEnvironment,
    scan_summary: &'a MeasurementSummary,
    export_summary: &'a ExportSummary,
    comparisons: &'a [PublicComparison],
    same_machine_comparisons: &'a [SameMachineComparison],
    audit_rows: &'a [SuiteAuditRow],
}

#[derive(Debug)]
struct SuiteManifest<'a> {
    options: &'a SuiteOptions,
    run_context: &'a SuiteRunContext,
    environment: &'a BenchmarkEnvironment,
    scan_summary: &'a MeasurementSummary,
    export_summary: &'a ExportSummary,
    comparisons: &'a [PublicComparison],
    same_machine_comparisons: &'a [SameMachineComparison],
    audit_rows: &'a [SuiteAuditRow],
}

#[derive(Debug)]
struct SuiteOptions {
    path: PathBuf,
    output_dir: PathBuf,
    dataset_label: String,
    cache_state: String,
    iterations: usize,
    sample_ms: u64,
    progress_every: u64,
    scanner: ScannerMode,
    include_directories: bool,
    claims: Vec<PublicClaimId>,
    competitor_csv: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Scan {
            path,
            iterations,
            sample_ms,
            progress_every,
            scanner,
        } => run_scan(path, iterations, sample_ms, progress_every, scanner),
        Command::Export {
            path,
            iterations,
            sample_ms,
            scanner,
            include_directories,
            output_dir,
        } => run_export(
            path,
            iterations,
            sample_ms,
            scanner,
            include_directories,
            output_dir,
        ),
        Command::Suite {
            path,
            output_dir,
            dataset_label,
            cache_state,
            iterations,
            sample_ms,
            progress_every,
            scanner,
            include_directories,
            claim,
            competitor_csv,
        } => run_suite(SuiteOptions {
            path,
            output_dir,
            dataset_label: single_line_value(&dataset_label, "unspecified"),
            cache_state: single_line_value(&cache_state, "unknown"),
            iterations,
            sample_ms,
            progress_every,
            scanner,
            include_directories,
            claims: claim,
            competitor_csv,
        }),
        Command::Dataset {
            root,
            dirs,
            files_per_dir,
            bytes_per_file,
        } => create_dataset(root, dirs, files_per_dir, bytes_per_file),
        Command::Summarize { csv } => summarize_measurements(csv),
        Command::ComparePublic { csv, claim } => compare_public_claims(csv, &claim),
        Command::CompareCompetitor {
            csv,
            competitor_csv,
            dataset_label,
            cache_state,
        } => compare_competitor_measurements(
            csv,
            competitor_csv,
            single_line_value(&dataset_label, "unspecified"),
            single_line_value(&cache_state, "unknown"),
        ),
        Command::CompetitorTemplate { examples } => {
            write_competitor_template(&mut io::stdout().lock(), examples)
        }
    }
}

fn run_scan(
    path: PathBuf,
    iterations: usize,
    sample_ms: u64,
    progress_every: u64,
    scanner: ScannerMode,
) -> Result<()> {
    let mut measurements = Vec::with_capacity(iterations);
    let sample_interval = Duration::from_millis(sample_ms.max(1));

    for iteration in 1..=iterations {
        let scan = run_measured_scan(path.clone(), sample_interval, progress_every, scanner)
            .with_context(|| format!("scan failed for {}", path.display()))?;
        measurements.push(measurement_from_run(iteration, scan));
    }

    write_measurements(&mut io::stdout().lock(), &measurements)?;
    Ok(())
}

fn run_export(
    path: PathBuf,
    iterations: usize,
    sample_ms: u64,
    scanner: ScannerMode,
    include_directories: bool,
    output_dir: Option<PathBuf>,
) -> Result<()> {
    if let Some(output_dir) = &output_dir {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
    }

    let mut measurements = Vec::with_capacity(iterations);
    let sample_interval = Duration::from_millis(sample_ms.max(1));

    for iteration in 1..=iterations {
        let export = run_measured_export(
            path.clone(),
            sample_interval,
            scanner,
            include_directories,
            output_dir.clone(),
            iteration,
        )
        .with_context(|| format!("export benchmark failed for {}", path.display()))?;
        measurements.push(export_measurement_from_run(iteration, export));
    }

    write_export_measurements(&mut io::stdout().lock(), &measurements)?;
    Ok(())
}

fn run_suite(options: SuiteOptions) -> Result<()> {
    fs::create_dir_all(&options.output_dir)
        .with_context(|| format!("failed to create {}", options.output_dir.display()))?;

    let sample_interval = Duration::from_millis(options.sample_ms.max(1));
    let scan_measurements = collect_scan_measurements(
        &options.path,
        options.iterations,
        sample_interval,
        options.progress_every,
        options.scanner,
    )?;
    let scan_summary = summarize_rows(&scan_measurements_to_rows(&scan_measurements))?;
    let export_measurements = collect_export_measurements(
        &options.path,
        options.iterations,
        sample_interval,
        options.scanner,
        options.include_directories,
    )?;
    let export_summary = summarize_export_measurements(&export_measurements)?;
    let claims = selected_claims(&options.claims);
    let comparisons: Vec<_> = claims
        .into_iter()
        .map(|claim_id| compare_summary_to_claim(&scan_summary, public_claim(claim_id)))
        .collect();
    let selected_claim_ids: Vec<_> = comparisons
        .iter()
        .map(|comparison| comparison.claim_id)
        .collect();
    let same_machine_comparisons = suite_same_machine_comparisons(&options, &scan_summary)?;
    let run_context = detect_suite_run_context();
    let environment = detect_benchmark_environment(&options.path);
    let audit_rows = suite_audit_rows(
        &options,
        &run_context,
        &comparisons,
        &same_machine_comparisons,
    );

    let scan_csv = options.output_dir.join("scan.csv");
    let mut scan_file = File::create(&scan_csv)
        .with_context(|| format!("failed to create {}", scan_csv.display()))?;
    write_measurements(&mut scan_file, &scan_measurements)?;

    let scan_summary_csv = options.output_dir.join("scan-summary.csv");
    let mut scan_summary_file = File::create(&scan_summary_csv)
        .with_context(|| format!("failed to create {}", scan_summary_csv.display()))?;
    write_summary(&mut scan_summary_file, &scan_summary)?;

    let export_csv = options.output_dir.join("export.csv");
    let mut export_file = File::create(&export_csv)
        .with_context(|| format!("failed to create {}", export_csv.display()))?;
    write_export_measurements(&mut export_file, &export_measurements)?;

    let comparison_csv = options.output_dir.join("public-comparison.csv");
    let mut comparison_file = File::create(&comparison_csv)
        .with_context(|| format!("failed to create {}", comparison_csv.display()))?;
    write_public_comparisons(&mut comparison_file, &comparisons)?;

    let same_machine_csv = options.output_dir.join("same-machine-comparison.csv");
    let mut same_machine_file = File::create(&same_machine_csv)
        .with_context(|| format!("failed to create {}", same_machine_csv.display()))?;
    write_same_machine_comparisons(&mut same_machine_file, &same_machine_comparisons)?;

    let audit_csv = options.output_dir.join("audit.csv");
    let mut audit_file = File::create(&audit_csv)
        .with_context(|| format!("failed to create {}", audit_csv.display()))?;
    write_suite_audit(&mut audit_file, &audit_rows)?;

    let metadata_txt = options.output_dir.join("metadata.txt");
    let mut metadata_file = File::create(&metadata_txt)
        .with_context(|| format!("failed to create {}", metadata_txt.display()))?;
    write_suite_metadata(
        &mut metadata_file,
        &options,
        &run_context,
        &environment,
        &selected_claim_ids,
    )?;

    let suite_report = SuiteReport {
        path: &options.path,
        output_dir: &options.output_dir,
        dataset_label: &options.dataset_label,
        cache_state: &options.cache_state,
        scanner: options.scanner,
        iterations: options.iterations,
        sample_ms: options.sample_ms,
        progress_every: options.progress_every,
        include_directories: options.include_directories,
        run_context: &run_context,
        environment: &environment,
        scan_summary: &scan_summary,
        export_summary: &export_summary,
        comparisons: &comparisons,
        same_machine_comparisons: &same_machine_comparisons,
        audit_rows: &audit_rows,
    };

    let suite_manifest = SuiteManifest {
        options: &options,
        run_context: &run_context,
        environment: &environment,
        scan_summary: &scan_summary,
        export_summary: &export_summary,
        comparisons: &comparisons,
        same_machine_comparisons: &same_machine_comparisons,
        audit_rows: &audit_rows,
    };

    let manifest_json = options.output_dir.join("manifest.json");
    let mut manifest_file = File::create(&manifest_json)
        .with_context(|| format!("failed to create {}", manifest_json.display()))?;
    write_suite_manifest(&mut manifest_file, &suite_manifest)?;

    let report_md = options.output_dir.join("report.md");
    let mut report_file = File::create(&report_md)
        .with_context(|| format!("failed to create {}", report_md.display()))?;
    write_suite_report(&mut report_file, &suite_report)?;

    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "benchmark suite written to {}",
        options.output_dir.display()
    )?;
    writeln!(stdout, "scan: {}", scan_csv.display())?;
    writeln!(stdout, "scan summary: {}", scan_summary_csv.display())?;
    writeln!(stdout, "export: {}", export_csv.display())?;
    writeln!(stdout, "public comparison: {}", comparison_csv.display())?;
    writeln!(
        stdout,
        "same-machine comparison: {}",
        same_machine_csv.display()
    )?;
    writeln!(stdout, "audit: {}", audit_csv.display())?;
    writeln!(stdout, "metadata: {}", metadata_txt.display())?;
    writeln!(stdout, "manifest: {}", manifest_json.display())?;
    writeln!(stdout, "report: {}", report_md.display())?;

    Ok(())
}

fn collect_scan_measurements(
    path: &Path,
    iterations: usize,
    sample_interval: Duration,
    progress_every: u64,
    scanner: ScannerMode,
) -> Result<Vec<ScanMeasurement>> {
    let mut measurements = Vec::with_capacity(iterations);
    for iteration in 1..=iterations {
        let scan = run_measured_scan(path.to_path_buf(), sample_interval, progress_every, scanner)
            .with_context(|| format!("scan failed for {}", path.display()))?;
        measurements.push(measurement_from_run(iteration, scan));
    }
    Ok(measurements)
}

fn collect_export_measurements(
    path: &Path,
    iterations: usize,
    sample_interval: Duration,
    scanner: ScannerMode,
    include_directories: bool,
) -> Result<Vec<ExportMeasurement>> {
    let mut measurements = Vec::with_capacity(iterations);
    for iteration in 1..=iterations {
        let export = run_measured_export(
            path.to_path_buf(),
            sample_interval,
            scanner,
            include_directories,
            None,
            iteration,
        )
        .with_context(|| format!("export benchmark failed for {}", path.display()))?;
        measurements.push(export_measurement_from_run(iteration, export));
    }
    Ok(measurements)
}

fn run_measured_scan(
    path: PathBuf,
    sample_interval: Duration,
    progress_every: u64,
    scanner: ScannerMode,
) -> Result<MeasuredScan> {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = scan_once(path, scanner, progress_every).map_err(|error| error.to_string());
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

fn run_measured_export(
    path: PathBuf,
    sample_interval: Duration,
    scanner: ScannerMode,
    include_directories: bool,
    output_dir: Option<PathBuf>,
    iteration: usize,
) -> Result<MeasuredExport> {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = export_once(path, scanner, include_directories, output_dir, iteration)
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });

    let mut memory = MemoryPeak::default();
    let run = loop {
        memory.observe(current_process_memory()?);
        match receiver.recv_timeout(sample_interval) {
            Ok(result) => break result.map_err(|error| anyhow!(error))?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("export worker stopped"));
            }
        }
    };
    handle
        .join()
        .map_err(|_| anyhow!("export worker panicked"))?;
    memory.observe(current_process_memory()?);

    Ok(MeasuredExport { run, memory })
}

fn scan_once(path: PathBuf, scanner: ScannerMode, progress_every: u64) -> Result<ScanRun> {
    let started = Instant::now();
    let mut first_result_ms = None;
    let outcome = scan_path(
        path,
        scanner,
        progress_every,
        &started,
        &mut first_result_ms,
    )?;
    let elapsed_ms = started.elapsed().as_millis();

    Ok(ScanRun {
        elapsed_ms,
        first_result_ms: first_result_ms.unwrap_or(elapsed_ms),
        scanner: outcome.scanner,
        fallback: outcome.fallback,
        summary: outcome.summary,
    })
}

fn export_once(
    path: PathBuf,
    scanner: ScannerMode,
    include_directories: bool,
    output_dir: Option<PathBuf>,
    iteration: usize,
) -> Result<ExportRun> {
    let total_started = Instant::now();
    let scan_started = Instant::now();
    let outcome = scan_graph_path(path, scanner)?;
    let scan_elapsed_ms = scan_started.elapsed().as_millis();

    let export_started = Instant::now();
    let options = CsvExportOptions {
        include_directories,
    };
    let export_bytes = if let Some(output_dir) = output_dir {
        let output_path = output_dir.join(format!("diskloom-export-{iteration:03}.csv"));
        let file = File::create(&output_path)
            .with_context(|| format!("failed to create {}", output_path.display()))?;
        export_graph(file, &outcome.graph, options)?
    } else {
        export_graph(io::sink(), &outcome.graph, options)?
    };
    let export_elapsed_ms = export_started.elapsed().as_millis();

    Ok(ExportRun {
        scan_elapsed_ms,
        export_elapsed_ms,
        total_elapsed_ms: total_started.elapsed().as_millis(),
        export_bytes,
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

#[derive(Debug)]
struct ScanGraphOutcome {
    scanner: &'static str,
    fallback: bool,
    graph: FileGraph,
    summary: ScanSummary,
}

fn scan_graph_path(path: PathBuf, scanner: ScannerMode) -> Result<ScanGraphOutcome> {
    match scanner {
        ScannerMode::Fallback => scan_graph_fallback(path, false),
        ScannerMode::Ntfs => scan_graph_ntfs(&path),
        ScannerMode::Auto => {
            if drive_volume(&path).is_some() {
                match scan_graph_ntfs(&path) {
                    Ok(outcome) => Ok(outcome),
                    Err(_) => scan_graph_fallback(path, true),
                }
            } else {
                scan_graph_fallback(path, false)
            }
        }
    }
}

fn scan_graph_fallback(path: PathBuf, fallback: bool) -> Result<ScanGraphOutcome> {
    let (graph, summary) = FallbackScanner::scan(ScanOptions {
        root: path,
        follow_symlinks: false,
    })?;

    Ok(ScanGraphOutcome {
        scanner: "fallback",
        fallback,
        graph,
        summary,
    })
}

fn scan_graph_ntfs(path: &Path) -> Result<ScanGraphOutcome> {
    let volume = drive_volume(path).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let graph = NtfsScanner::scan_volume(&volume)?;
    let summary = summary_from_graph(&graph);
    Ok(ScanGraphOutcome {
        scanner: "ntfs",
        fallback: false,
        graph,
        summary,
    })
}

fn scan_path(
    path: PathBuf,
    scanner: ScannerMode,
    progress_every: u64,
    started: &Instant,
    first_result_ms: &mut Option<u128>,
) -> Result<ScanOutcome> {
    match scanner {
        ScannerMode::Fallback => {
            scan_fallback_with_progress(path, false, progress_every, started, first_result_ms)
        }
        ScannerMode::Ntfs => scan_ntfs(path.as_path()),
        ScannerMode::Auto => {
            if drive_volume(&path).is_some() {
                match scan_ntfs(path.as_path()) {
                    Ok(outcome) => Ok(outcome),
                    Err(_) => scan_fallback_with_progress(
                        path,
                        true,
                        progress_every,
                        started,
                        first_result_ms,
                    ),
                }
            } else {
                scan_fallback_with_progress(path, false, progress_every, started, first_result_ms)
            }
        }
    }
}

fn scan_fallback_with_progress(
    path: PathBuf,
    fallback: bool,
    progress_every: u64,
    started: &Instant,
    first_result_ms: &mut Option<u128>,
) -> Result<ScanOutcome> {
    let (_, summary) = FallbackScanner::scan_with_progress(
        ScanOptions {
            root: path,
            follow_symlinks: false,
        },
        progress_every,
        |_| {
            if first_result_ms.is_none() {
                *first_result_ms = Some(started.elapsed().as_millis());
            }
        },
    )?;

    Ok(ScanOutcome {
        scanner: "fallback",
        fallback,
        summary,
    })
}

fn scan_ntfs(path: &Path) -> Result<ScanOutcome> {
    let outcome = scan_graph_ntfs(path)?;
    Ok(ScanOutcome {
        scanner: outcome.scanner,
        fallback: outcome.fallback,
        summary: outcome.summary,
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
        first_result_ms: scan.run.first_result_ms,
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

fn export_measurement_from_run(iteration: usize, export: MeasuredExport) -> ExportMeasurement {
    let summary = export.run.summary;
    ExportMeasurement {
        iteration,
        scanner: export.run.scanner,
        fallback: export.run.fallback,
        scan_elapsed_ms: export.run.scan_elapsed_ms,
        export_elapsed_ms: export.run.export_elapsed_ms,
        total_elapsed_ms: export.run.total_elapsed_ms,
        export_bytes: export.run.export_bytes,
        entries: summary.entries,
        files: summary.files,
        directories: summary.directories,
        inaccessible: summary.inaccessible,
        peak_working_set_bytes: export.memory.peak_working_set_bytes,
        peak_private_bytes: export.memory.peak_private_bytes,
        final_working_set_bytes: export.memory.final_working_set_bytes,
        final_private_bytes: export.memory.final_private_bytes,
        peak_private_bytes_per_million_entries: per_million(
            export.memory.peak_private_bytes,
            summary.entries,
        ),
        memory_samples: export.memory.samples,
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

fn export_graph<W: Write>(writer: W, graph: &FileGraph, options: CsvExportOptions) -> Result<u64> {
    let mut writer = CountingWriter::new(writer);
    export_csv(graph, &mut writer, options)?;
    Ok(writer.bytes())
}

#[derive(Debug)]
struct CountingWriter<W> {
    inner: W,
    bytes: u64,
}

impl<W> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, bytes: 0 }
    }

    fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
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

fn compare_public_claims(path: PathBuf, claims: &[PublicClaimId]) -> Result<()> {
    let input =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let rows = parse_measurements(&input)?;
    let summary = summarize_rows(&rows)?;
    let comparisons: Vec<_> = selected_claims(claims)
        .into_iter()
        .map(|claim_id| compare_summary_to_claim(&summary, public_claim(claim_id)))
        .collect();
    write_public_comparisons(&mut io::stdout().lock(), &comparisons)?;
    Ok(())
}

fn compare_competitor_measurements(
    diskloom_csv: PathBuf,
    competitor_csv: PathBuf,
    dataset_label: String,
    cache_state: String,
) -> Result<()> {
    let diskloom_input = fs::read_to_string(&diskloom_csv)
        .with_context(|| format!("failed to read {}", diskloom_csv.display()))?;
    let competitor_input = fs::read_to_string(&competitor_csv)
        .with_context(|| format!("failed to read {}", competitor_csv.display()))?;
    let diskloom_rows = parse_measurements(&diskloom_input)?;
    let diskloom_summary = summarize_rows(&diskloom_rows)?;
    let competitor_rows = parse_competitor_measurements(&competitor_input)?;
    let comparisons = compare_summary_to_competitors(
        &diskloom_summary,
        &competitor_rows,
        &dataset_label,
        &cache_state,
    )?;
    write_same_machine_comparisons(&mut io::stdout().lock(), &comparisons)?;
    Ok(())
}

fn write_competitor_template(writer: &mut impl Write, examples: bool) -> Result<()> {
    writeln!(
        writer,
        "tool,version,dataset_label,cache_state,scanner_scope,elapsed_ms,peak_private_bytes,notes"
    )?;
    if examples {
        writeln!(
            writer,
            "WizTree,example,example-dataset,warm,ntfs_mft,5230,,manual timing"
        )?;
        writeln!(
            writer,
            "TreeSize,example,example-dataset,warm,traversal,18000,,manual timing"
        )?;
    }
    Ok(())
}

fn suite_same_machine_comparisons(
    options: &SuiteOptions,
    diskloom_summary: &MeasurementSummary,
) -> Result<Vec<SameMachineComparison>> {
    let Some(competitor_csv) = &options.competitor_csv else {
        return Ok(Vec::new());
    };
    let input = fs::read_to_string(competitor_csv)
        .with_context(|| format!("failed to read {}", competitor_csv.display()))?;
    let rows = parse_competitor_measurements(&input)?;
    compare_summary_to_competitors(
        diskloom_summary,
        &rows,
        &options.dataset_label,
        &options.cache_state,
    )
}

fn selected_claims(claims: &[PublicClaimId]) -> Vec<PublicClaimId> {
    if claims.is_empty() {
        all_public_claims().to_vec()
    } else {
        claims.to_vec()
    }
}

fn all_public_claims() -> [PublicClaimId; 3] {
    [
        PublicClaimId::WizTreeSsd500GbTypical,
        PublicClaimId::WizTreeSsd460Gb,
        PublicClaimId::WizTreeHdd25Gb,
    ]
}

fn write_measurements(writer: &mut impl Write, measurements: &[ScanMeasurement]) -> Result<()> {
    writeln!(
        writer,
        "iteration,scanner,fallback,elapsed_ms,first_result_ms,entries,files,directories,inaccessible,peak_working_set_bytes,peak_private_bytes,final_working_set_bytes,final_private_bytes,peak_private_bytes_per_million_entries,memory_samples"
    )?;
    for measurement in measurements {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            measurement.iteration,
            measurement.scanner,
            u8::from(measurement.fallback),
            measurement.elapsed_ms,
            measurement.first_result_ms,
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

fn write_export_measurements(
    writer: &mut impl Write,
    measurements: &[ExportMeasurement],
) -> Result<()> {
    writeln!(
        writer,
        "iteration,scanner,fallback,scan_elapsed_ms,export_elapsed_ms,total_elapsed_ms,export_bytes,entries,files,directories,inaccessible,peak_working_set_bytes,peak_private_bytes,final_working_set_bytes,final_private_bytes,peak_private_bytes_per_million_entries,memory_samples"
    )?;
    for measurement in measurements {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            measurement.iteration,
            measurement.scanner,
            u8::from(measurement.fallback),
            measurement.scan_elapsed_ms,
            measurement.export_elapsed_ms,
            measurement.total_elapsed_ms,
            measurement.export_bytes,
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
    let first_result_idx = optional_field_index(&headers, "first_result_ms");
    let entries_idx = field_index(&headers, "entries")?;
    let peak_ws_idx = field_index(&headers, "peak_working_set_bytes")?;
    let peak_private_idx = field_index(&headers, "peak_private_bytes")?;
    let per_million_idx = field_index(&headers, "peak_private_bytes_per_million_entries")?;

    lines
        .enumerate()
        .map(|(idx, line)| {
            let fields: Vec<_> = line.split(',').collect();
            let elapsed_ms = parse_field(field(&fields, elapsed_idx, idx)?, "elapsed_ms")?;
            let first_result_ms = match first_result_idx {
                Some(first_result_idx) => {
                    parse_field(field(&fields, first_result_idx, idx)?, "first_result_ms")?
                }
                None => elapsed_ms,
            };
            Ok(ParsedMeasurement {
                scanner: field(&fields, scanner_idx, idx)?.to_owned(),
                fallback: parse_bool_field(field(&fields, fallback_idx, idx)?)?,
                elapsed_ms,
                first_result_ms,
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

fn parse_competitor_measurements(input: &str) -> Result<Vec<CompetitorMeasurement>> {
    let mut lines = input.lines().filter(|line| !line.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| anyhow!("competitor CSV is empty"))?;
    let headers: Vec<_> = header.split(',').collect();

    let tool_idx = field_index(&headers, "tool")?;
    let version_idx = field_index(&headers, "version")?;
    let dataset_label_idx = field_index(&headers, "dataset_label")?;
    let cache_state_idx = field_index(&headers, "cache_state")?;
    let scanner_scope_idx = field_index(&headers, "scanner_scope")?;
    let elapsed_ms_idx = field_index(&headers, "elapsed_ms")?;
    let peak_private_bytes_idx = optional_field_index(&headers, "peak_private_bytes");

    lines
        .enumerate()
        .map(|(idx, line)| {
            let fields: Vec<_> = line.split(',').collect();
            Ok(CompetitorMeasurement {
                tool: field(&fields, tool_idx, idx)?.trim().to_owned(),
                version: field(&fields, version_idx, idx)?.trim().to_owned(),
                dataset_label: field(&fields, dataset_label_idx, idx)?.trim().to_owned(),
                cache_state: field(&fields, cache_state_idx, idx)?.trim().to_owned(),
                scanner_scope: field(&fields, scanner_scope_idx, idx)?.trim().to_owned(),
                elapsed_ms: parse_field(field(&fields, elapsed_ms_idx, idx)?.trim(), "elapsed_ms")?,
                peak_private_bytes: match peak_private_bytes_idx {
                    Some(peak_private_bytes_idx) => parse_optional_u64_field(
                        field(&fields, peak_private_bytes_idx, idx)?.trim(),
                        "peak_private_bytes",
                    )?,
                    None => None,
                },
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

fn optional_field_index(headers: &[&str], name: &str) -> Option<usize> {
    headers.iter().position(|candidate| *candidate == name)
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

fn parse_optional_u64_field(value: &str, name: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_field(value, name).map(Some)
    }
}

fn summarize_rows(rows: &[ParsedMeasurement]) -> Result<MeasurementSummary> {
    if rows.is_empty() {
        return Err(anyhow!("measurement CSV has no data rows"));
    }

    let mut scanners = BTreeSet::new();
    let mut elapsed: Vec<_> = rows.iter().map(|row| row.elapsed_ms).collect();
    let mut first_result: Vec<_> = rows.iter().map(|row| row.first_result_ms).collect();
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
        first_result_ms_min: *first_result.iter().min().unwrap_or(&0),
        first_result_ms_median: median_u128(&mut first_result),
        first_result_ms_max: *first_result.iter().max().unwrap_or(&0),
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

fn scan_measurements_to_rows(measurements: &[ScanMeasurement]) -> Vec<ParsedMeasurement> {
    measurements
        .iter()
        .map(|measurement| ParsedMeasurement {
            scanner: measurement.scanner.to_owned(),
            fallback: measurement.fallback,
            elapsed_ms: measurement.elapsed_ms,
            first_result_ms: measurement.first_result_ms,
            entries: measurement.entries,
            peak_working_set_bytes: measurement.peak_working_set_bytes,
            peak_private_bytes: measurement.peak_private_bytes,
            peak_private_bytes_per_million_entries: measurement
                .peak_private_bytes_per_million_entries,
        })
        .collect()
}

fn summarize_export_measurements(measurements: &[ExportMeasurement]) -> Result<ExportSummary> {
    if measurements.is_empty() {
        return Err(anyhow!("export measurements have no data rows"));
    }

    let mut scanners = BTreeSet::new();
    let entries: Vec<_> = measurements
        .iter()
        .map(|measurement| measurement.entries)
        .collect();
    let export_bytes: Vec<_> = measurements
        .iter()
        .map(|measurement| measurement.export_bytes)
        .collect();
    let mut scan_elapsed: Vec<_> = measurements
        .iter()
        .map(|measurement| measurement.scan_elapsed_ms)
        .collect();
    let mut export_elapsed: Vec<_> = measurements
        .iter()
        .map(|measurement| measurement.export_elapsed_ms)
        .collect();
    let mut total_elapsed: Vec<_> = measurements
        .iter()
        .map(|measurement| measurement.total_elapsed_ms)
        .collect();
    for measurement in measurements {
        scanners.insert(measurement.scanner);
    }

    Ok(ExportSummary {
        runs: measurements.len(),
        scanners: scanners.into_iter().collect::<Vec<_>>().join("+"),
        fallback_runs: measurements
            .iter()
            .filter(|measurement| measurement.fallback)
            .count(),
        entries_min: *entries.iter().min().unwrap_or(&0),
        entries_max: *entries.iter().max().unwrap_or(&0),
        export_bytes_min: *export_bytes.iter().min().unwrap_or(&0),
        export_bytes_max: *export_bytes.iter().max().unwrap_or(&0),
        scan_elapsed_ms_median: median_u128(&mut scan_elapsed),
        export_elapsed_ms_min: *export_elapsed.iter().min().unwrap_or(&0),
        export_elapsed_ms_median: median_u128(&mut export_elapsed),
        export_elapsed_ms_max: *export_elapsed.iter().max().unwrap_or(&0),
        total_elapsed_ms_median: median_u128(&mut total_elapsed),
        peak_working_set_bytes_max: measurements
            .iter()
            .map(|measurement| measurement.peak_working_set_bytes)
            .max()
            .unwrap_or(0),
        peak_private_bytes_max: measurements
            .iter()
            .map(|measurement| measurement.peak_private_bytes)
            .max()
            .unwrap_or(0),
        peak_private_bytes_per_million_entries_max: measurements
            .iter()
            .map(|measurement| measurement.peak_private_bytes_per_million_entries)
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
        "runs,scanners,fallback_runs,entries_min,entries_max,elapsed_ms_min,elapsed_ms_median,elapsed_ms_max,first_result_ms_min,first_result_ms_median,first_result_ms_max,peak_working_set_bytes_max,peak_private_bytes_max,peak_private_bytes_per_million_entries_max"
    )?;
    writeln!(
        writer,
        "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        summary.runs,
        summary.scanners,
        summary.fallback_runs,
        summary.entries_min,
        summary.entries_max,
        summary.elapsed_ms_min,
        summary.elapsed_ms_median,
        summary.elapsed_ms_max,
        summary.first_result_ms_min,
        summary.first_result_ms_median,
        summary.first_result_ms_max,
        summary.peak_working_set_bytes_max,
        summary.peak_private_bytes_max,
        summary.peak_private_bytes_per_million_entries_max
    )?;
    Ok(())
}

fn public_claim(id: PublicClaimId) -> PublicClaim {
    match id {
        PublicClaimId::WizTreeSsd500GbTypical => PublicClaim {
            id: "wiztree-ssd-500gb-typical",
            source_url: "https://diskanalyzer.com/",
            context: "500GB_NTFS_SSD_typical_current_public_homepage_claim",
            scan_scope: ClaimScanScope::NtfsMft,
            elapsed_ms_min: 3_000,
            elapsed_ms_max: 8_000,
        },
        PublicClaimId::WizTreeHdd25Gb => PublicClaim {
            id: "wiztree-hdd-25gb",
            source_url: "https://diskanalyzer.com/wiztree-vs-windirstat",
            context: "25GB_NTFS_HDD_Acer_laptop_Windows_XP_vendor_test",
            scan_scope: ClaimScanScope::NtfsMft,
            elapsed_ms_min: 4_340,
            elapsed_ms_max: 4_340,
        },
        PublicClaimId::WizTreeSsd460Gb => PublicClaim {
            id: "wiztree-ssd-460gb",
            source_url: "https://diskanalyzer.com/wiztree-vs-windirstat",
            context: "460GB_NTFS_SSD_ASUS_laptop_Windows_10_vendor_test",
            scan_scope: ClaimScanScope::NtfsMft,
            elapsed_ms_min: 5_230,
            elapsed_ms_max: 5_230,
        },
    }
}

fn compare_summary_to_claim(summary: &MeasurementSummary, claim: PublicClaim) -> PublicComparison {
    PublicComparison {
        claim_id: claim.id,
        claim_source_url: claim.source_url,
        claim_context: claim.context,
        claim_scan_scope: claim.scan_scope.label(),
        claim_elapsed_ms_min: claim.elapsed_ms_min,
        claim_elapsed_ms_max: claim.elapsed_ms_max,
        comparison_applicability: comparison_applicability(summary, claim.scan_scope),
        diskloom_runs: summary.runs,
        diskloom_scanners: summary.scanners.clone(),
        diskloom_fallback_runs: summary.fallback_runs,
        diskloom_elapsed_ms_min: summary.elapsed_ms_min,
        diskloom_elapsed_ms_median: summary.elapsed_ms_median,
        diskloom_elapsed_ms_max: summary.elapsed_ms_max,
        diskloom_peak_private_bytes_max: summary.peak_private_bytes_max,
        diskloom_vs_claim_min_ratio: ratio_decimal(summary.elapsed_ms_median, claim.elapsed_ms_min),
        diskloom_vs_claim_max_ratio: ratio_decimal(summary.elapsed_ms_median, claim.elapsed_ms_max),
        diskloom_median_position: median_position(
            summary.elapsed_ms_median,
            claim.elapsed_ms_min,
            claim.elapsed_ms_max,
        ),
        validity: "reference_only_vendor_claim_not_same_machine",
    }
}

fn comparison_applicability(
    summary: &MeasurementSummary,
    claim_scan_scope: ClaimScanScope,
) -> &'static str {
    match claim_scan_scope {
        ClaimScanScope::NtfsMft => {
            if summary.fallback_runs == 0 && summary.scanners == "ntfs" {
                "aligned_ntfs_mft"
            } else if summary.scanners.split('+').any(|scanner| scanner == "ntfs") {
                "mixed_or_fallback_not_aligned"
            } else {
                "not_aligned_requires_ntfs_mft"
            }
        }
    }
}

fn median_position(median_ms: u128, claim_min_ms: u128, claim_max_ms: u128) -> &'static str {
    if claim_min_ms > claim_max_ms {
        "invalid_public_range"
    } else if median_ms < claim_min_ms {
        "below_public_range"
    } else if median_ms > claim_max_ms {
        "above_public_range"
    } else {
        "within_public_range"
    }
}

fn ratio_decimal(numerator: u128, denominator: u128) -> String {
    if denominator == 0 {
        return "n/a".to_owned();
    }
    let scaled = numerator.saturating_mul(1_000) / denominator;
    format!("{}.{:03}", scaled / 1_000, scaled % 1_000)
}

fn compare_summary_to_competitors(
    diskloom_summary: &MeasurementSummary,
    competitor_rows: &[CompetitorMeasurement],
    dataset_label: &str,
    cache_state: &str,
) -> Result<Vec<SameMachineComparison>> {
    let competitor_summaries = summarize_competitors(competitor_rows)?;
    Ok(competitor_summaries
        .iter()
        .map(|summary| {
            compare_summary_to_competitor(diskloom_summary, summary, dataset_label, cache_state)
        })
        .collect())
}

fn summarize_competitors(rows: &[CompetitorMeasurement]) -> Result<Vec<CompetitorSummary>> {
    if rows.is_empty() {
        return Err(anyhow!("competitor CSV has no data rows"));
    }

    let mut groups: BTreeMap<CompetitorKey, Vec<&CompetitorMeasurement>> = BTreeMap::new();
    for row in rows {
        groups
            .entry(CompetitorKey {
                tool: row.tool.clone(),
                version: row.version.clone(),
                dataset_label: row.dataset_label.clone(),
                cache_state: row.cache_state.clone(),
                scanner_scope: row.scanner_scope.clone(),
            })
            .or_default()
            .push(row);
    }

    groups
        .into_iter()
        .map(|(key, rows)| summarize_competitor_group(key, &rows))
        .collect()
}

fn summarize_competitor_group(
    key: CompetitorKey,
    rows: &[&CompetitorMeasurement],
) -> Result<CompetitorSummary> {
    let mut elapsed: Vec<_> = rows.iter().map(|row| row.elapsed_ms).collect();
    let elapsed_ms_min = *elapsed.iter().min().unwrap_or(&0);
    let elapsed_ms_max = *elapsed.iter().max().unwrap_or(&0);
    let elapsed_ms_median = median_u128(&mut elapsed);
    let peak_private_bytes_max = rows.iter().filter_map(|row| row.peak_private_bytes).max();

    Ok(CompetitorSummary {
        key,
        runs: rows.len(),
        elapsed_ms_min,
        elapsed_ms_median,
        elapsed_ms_max,
        peak_private_bytes_max,
    })
}

fn compare_summary_to_competitor(
    diskloom_summary: &MeasurementSummary,
    competitor_summary: &CompetitorSummary,
    dataset_label: &str,
    cache_state: &str,
) -> SameMachineComparison {
    let context_match = competitor_context_match(competitor_summary, dataset_label, cache_state);
    let scanner_scope_match =
        competitor_scanner_scope_match(diskloom_summary, &competitor_summary.key.scanner_scope);
    let validity = same_machine_validity(context_match, scanner_scope_match);
    let diskloom_private_bytes_delta = competitor_summary
        .peak_private_bytes_max
        .map(|value| i128::from(diskloom_summary.peak_private_bytes_max) - i128::from(value));

    SameMachineComparison {
        tool: competitor_summary.key.tool.clone(),
        version: competitor_summary.key.version.clone(),
        dataset_label: competitor_summary.key.dataset_label.clone(),
        cache_state: competitor_summary.key.cache_state.clone(),
        scanner_scope: competitor_summary.key.scanner_scope.clone(),
        context_match,
        scanner_scope_match,
        competitor_runs: competitor_summary.runs,
        diskloom_runs: diskloom_summary.runs,
        competitor_elapsed_ms_min: competitor_summary.elapsed_ms_min,
        competitor_elapsed_ms_median: competitor_summary.elapsed_ms_median,
        competitor_elapsed_ms_max: competitor_summary.elapsed_ms_max,
        diskloom_elapsed_ms_min: diskloom_summary.elapsed_ms_min,
        diskloom_elapsed_ms_median: diskloom_summary.elapsed_ms_median,
        diskloom_elapsed_ms_max: diskloom_summary.elapsed_ms_max,
        diskloom_vs_competitor_median_ratio: ratio_decimal(
            diskloom_summary.elapsed_ms_median,
            competitor_summary.elapsed_ms_median,
        ),
        competitor_peak_private_bytes_max: competitor_summary.peak_private_bytes_max,
        diskloom_peak_private_bytes_max: diskloom_summary.peak_private_bytes_max,
        diskloom_private_bytes_delta,
        validity,
    }
}

fn competitor_scanner_scope_match(
    diskloom_summary: &MeasurementSummary,
    competitor_scope: &str,
) -> &'static str {
    match competitor_scope.trim().to_ascii_lowercase().as_str() {
        "ntfs_mft" => {
            if diskloom_summary.fallback_runs == 0 && diskloom_summary.scanners == "ntfs" {
                "aligned_ntfs_mft"
            } else if diskloom_summary
                .scanners
                .split('+')
                .any(|scanner| scanner == "ntfs")
            {
                "mixed_or_fallback_not_aligned"
            } else {
                "not_aligned_requires_ntfs_mft"
            }
        }
        "traversal" | "fallback" => {
            if diskloom_summary.scanners == "fallback" {
                "aligned_traversal"
            } else if diskloom_summary
                .scanners
                .split('+')
                .any(|scanner| scanner == "fallback")
            {
                "mixed_or_ntfs_not_aligned"
            } else {
                "not_aligned_requires_traversal"
            }
        }
        _ => "unknown_competitor_scope",
    }
}

fn competitor_context_match(
    competitor_summary: &CompetitorSummary,
    dataset_label: &str,
    cache_state: &str,
) -> &'static str {
    if dataset_label == "unspecified" || cache_state == "unknown" {
        "missing_diskloom_context"
    } else if competitor_summary.key.dataset_label == dataset_label
        && competitor_summary.key.cache_state == cache_state
    {
        "matched"
    } else {
        "mismatch"
    }
}

fn same_machine_validity(context_match: &str, scanner_scope_match: &str) -> &'static str {
    match context_match {
        "missing_diskloom_context" => "missing_diskloom_context",
        "matched" => match scanner_scope_match {
            "aligned_ntfs_mft" | "aligned_traversal" => "same_machine_user_supplied",
            "unknown_competitor_scope" => "scanner_scope_unknown",
            _ => "scanner_scope_mismatch",
        },
        _ => "context_mismatch",
    }
}

fn optional_u64_csv(value: Option<u64>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn optional_i128_csv(value: Option<i128>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn write_same_machine_comparisons(
    writer: &mut impl Write,
    comparisons: &[SameMachineComparison],
) -> Result<()> {
    writeln!(
        writer,
        "tool,version,dataset_label,cache_state,scanner_scope,context_match,scanner_scope_match,competitor_runs,diskloom_runs,competitor_elapsed_ms_min,competitor_elapsed_ms_median,competitor_elapsed_ms_max,diskloom_elapsed_ms_min,diskloom_elapsed_ms_median,diskloom_elapsed_ms_max,diskloom_vs_competitor_median_ratio,competitor_peak_private_bytes_max,diskloom_peak_private_bytes_max,diskloom_private_bytes_delta,validity"
    )?;
    for comparison in comparisons {
        write_csv_cell(writer, &comparison.tool)?;
        write!(writer, ",")?;
        write_csv_cell(writer, &comparison.version)?;
        write!(writer, ",")?;
        write_csv_cell(writer, &comparison.dataset_label)?;
        write!(writer, ",")?;
        write_csv_cell(writer, &comparison.cache_state)?;
        write!(writer, ",")?;
        write_csv_cell(writer, &comparison.scanner_scope)?;
        write!(
            writer,
            ",{},{},{},{},{},{},{},{},{},{},{},{},{},{},",
            comparison.context_match,
            comparison.scanner_scope_match,
            comparison.competitor_runs,
            comparison.diskloom_runs,
            comparison.competitor_elapsed_ms_min,
            comparison.competitor_elapsed_ms_median,
            comparison.competitor_elapsed_ms_max,
            comparison.diskloom_elapsed_ms_min,
            comparison.diskloom_elapsed_ms_median,
            comparison.diskloom_elapsed_ms_max,
            comparison.diskloom_vs_competitor_median_ratio,
            optional_u64_csv(comparison.competitor_peak_private_bytes_max),
            comparison.diskloom_peak_private_bytes_max,
            optional_i128_csv(comparison.diskloom_private_bytes_delta),
        )?;
        write_csv_cell(writer, comparison.validity)?;
        writeln!(writer)?;
    }
    Ok(())
}

fn write_public_comparisons(
    writer: &mut impl Write,
    comparisons: &[PublicComparison],
) -> Result<()> {
    writeln!(
        writer,
        "claim_id,claim_source_url,claim_context,claim_scan_scope,claim_elapsed_ms_min,claim_elapsed_ms_max,comparison_applicability,diskloom_runs,diskloom_scanners,diskloom_fallback_runs,diskloom_elapsed_ms_min,diskloom_elapsed_ms_median,diskloom_elapsed_ms_max,diskloom_peak_private_bytes_max,diskloom_vs_claim_min_ratio,diskloom_vs_claim_max_ratio,diskloom_median_position,validity"
    )?;
    for comparison in comparisons {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            comparison.claim_id,
            comparison.claim_source_url,
            comparison.claim_context,
            comparison.claim_scan_scope,
            comparison.claim_elapsed_ms_min,
            comparison.claim_elapsed_ms_max,
            comparison.comparison_applicability,
            comparison.diskloom_runs,
            comparison.diskloom_scanners,
            comparison.diskloom_fallback_runs,
            comparison.diskloom_elapsed_ms_min,
            comparison.diskloom_elapsed_ms_median,
            comparison.diskloom_elapsed_ms_max,
            comparison.diskloom_peak_private_bytes_max,
            comparison.diskloom_vs_claim_min_ratio,
            comparison.diskloom_vs_claim_max_ratio,
            comparison.diskloom_median_position,
            comparison.validity
        )?;
    }
    Ok(())
}

fn suite_audit_rows(
    options: &SuiteOptions,
    run_context: &SuiteRunContext,
    comparisons: &[PublicComparison],
    same_machine_comparisons: &[SameMachineComparison],
) -> Vec<SuiteAuditRow> {
    let mut rows = Vec::new();

    rows.push(if options.dataset_label == "unspecified" {
        SuiteAuditRow::new(
            "dataset_label",
            AuditStatus::Fail,
            "Set --dataset-label before publishing benchmark results.",
        )
    } else {
        SuiteAuditRow::new(
            "dataset_label",
            AuditStatus::Pass,
            "Dataset label is recorded.",
        )
    });

    rows.push(if options.cache_state == "unknown" {
        SuiteAuditRow::new(
            "cache_state",
            AuditStatus::Fail,
            "Set --cache-state to cold, warm, or a documented custom state.",
        )
    } else {
        SuiteAuditRow::new("cache_state", AuditStatus::Pass, "Cache state is recorded.")
    });

    rows.push(if options.iterations >= 3 {
        SuiteAuditRow::new(
            "iterations",
            AuditStatus::Pass,
            "Run count is sufficient for a median.",
        )
    } else {
        SuiteAuditRow::new(
            "iterations",
            AuditStatus::Warning,
            "Use at least 3 iterations for publishable benchmark summaries.",
        )
    });

    rows.push(if run_context.git_dirty == "false" {
        SuiteAuditRow::new("git_dirty", AuditStatus::Pass, "Git worktree was clean.")
    } else {
        SuiteAuditRow::new(
            "git_dirty",
            AuditStatus::Warning,
            "Git worktree was not clean; record the exact diff if publishing.",
        )
    });

    rows.push(
        if same_machine_comparisons
            .iter()
            .any(|comparison| comparison.validity == "same_machine_user_supplied")
        {
            SuiteAuditRow::new(
                "same_machine_competitors",
                AuditStatus::Pass,
                "At least one matched same-machine competitor row is recorded.",
            )
        } else if options.competitor_csv.is_some() {
            SuiteAuditRow::new(
                "same_machine_competitors",
                AuditStatus::Warning,
                "Competitor CSV was supplied, but no rows matched the suite dataset/cache context.",
            )
        } else {
            SuiteAuditRow::new(
                "same_machine_competitors",
                AuditStatus::Warning,
                "No same-machine competitor CSV was supplied.",
            )
        },
    );

    rows.push(if comparisons.is_empty() {
        SuiteAuditRow::new(
            "public_claims",
            AuditStatus::Warning,
            "No public claim reference rows were selected.",
        )
    } else if comparisons
        .iter()
        .all(|comparison| comparison.validity == "reference_only_vendor_claim_not_same_machine")
    {
        SuiteAuditRow::new(
            "public_claims",
            AuditStatus::Warning,
            "Public WizTree rows are reference-only; same-machine competitor runs are required for speed claims.",
        )
    } else {
        SuiteAuditRow::new(
            "public_claims",
            AuditStatus::Pass,
            "Public claim validity includes stronger evidence than reference-only rows.",
        )
    });

    rows.push(
        if comparisons
            .iter()
            .all(|comparison| comparison.comparison_applicability == "aligned_ntfs_mft")
        {
            SuiteAuditRow::new(
                "comparison_scope",
                AuditStatus::Pass,
                "DiskLoom scanner scope matches all selected public claims.",
            )
        } else {
            SuiteAuditRow::new(
                "comparison_scope",
                AuditStatus::Warning,
                "At least one public claim is NTFS MFT scoped but this suite is fallback or mixed.",
            )
        },
    );

    rows
}

fn suite_audit_status(rows: &[SuiteAuditRow]) -> AuditStatus {
    if rows.iter().any(|row| row.status == AuditStatus::Fail) {
        AuditStatus::Fail
    } else if rows.iter().any(|row| row.status == AuditStatus::Warning) {
        AuditStatus::Warning
    } else {
        AuditStatus::Pass
    }
}

fn write_suite_audit(writer: &mut impl Write, rows: &[SuiteAuditRow]) -> Result<()> {
    writeln!(writer, "check,status,message")?;
    for row in rows {
        write_csv_cell(writer, row.check)?;
        write!(writer, ",")?;
        write_csv_cell(writer, row.status.label())?;
        write!(writer, ",")?;
        write_csv_cell(writer, &row.message)?;
        writeln!(writer)?;
    }
    Ok(())
}

fn write_csv_cell(writer: &mut impl Write, value: &str) -> Result<()> {
    if value
        .chars()
        .any(|ch| matches!(ch, ',' | '"' | '\r' | '\n'))
    {
        write!(writer, "\"{}\"", value.replace('"', "\"\""))?;
    } else {
        write!(writer, "{value}")?;
    }
    Ok(())
}

fn write_suite_report(writer: &mut impl Write, report: &SuiteReport<'_>) -> Result<()> {
    writeln!(writer, "# DiskLoom Benchmark Suite")?;
    writeln!(writer)?;
    writeln!(writer, "## Run")?;
    writeln!(writer)?;
    writeln!(writer, "- Path: `{}`", report.path.display())?;
    writeln!(
        writer,
        "- Output directory: `{}`",
        report.output_dir.display()
    )?;
    writeln!(writer, "- Dataset label: `{}`", report.dataset_label)?;
    writeln!(writer, "- Cache state: `{}`", report.cache_state)?;
    writeln!(writer, "- Scanner: `{}`", scanner_label(report.scanner))?;
    writeln!(writer, "- Iterations: {}", report.iterations)?;
    writeln!(writer, "- Sample interval: {} ms", report.sample_ms)?;
    writeln!(
        writer,
        "- Progress interval: {} entries",
        report.progress_every
    )?;
    writeln!(
        writer,
        "- Export includes directories: {}",
        report.include_directories
    )?;
    writeln!(writer, "- Command: `{}`", report.run_context.command_line)?;
    writeln!(
        writer,
        "- Git revision: `{}`",
        report.run_context.git_revision
    )?;
    writeln!(writer, "- Git dirty: `{}`", report.run_context.git_dirty)?;
    writeln!(writer)?;
    writeln!(writer, "## Benchmark Audit")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "- Overall status: `{}`",
        suite_audit_status(report.audit_rows).label()
    )?;
    writeln!(writer)?;
    writeln!(writer, "| check | status | message |")?;
    writeln!(writer, "| --- | --- | --- |")?;
    for row in report.audit_rows {
        writeln!(
            writer,
            "| {} | {} | {} |",
            row.check,
            row.status.label(),
            row.message
        )?;
    }
    writeln!(writer)?;
    writeln!(writer, "## Environment")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "- Volume root: `{}`",
        report.environment.volume_root
    )?;
    writeln!(
        writer,
        "- File system: `{}`",
        report.environment.file_system
    )?;
    writeln!(writer, "- Drive type: `{}`", report.environment.drive_type)?;
    writeln!(
        writer,
        "- Shell elevated: `{}`",
        report.environment.shell_elevated
    )?;
    writeln!(
        writer,
        "- Windows version: `{}`",
        report.environment.windows_version
    )?;
    writeln!(
        writer,
        "- Logical CPUs: `{}`",
        report.environment.logical_cpus
    )?;
    writeln!(
        writer,
        "- Physical memory bytes: `{}`",
        report.environment.physical_memory_bytes
    )?;
    writeln!(writer)?;
    writeln!(writer, "## DiskLoom Scan")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "| runs | scanners | fallback runs | entries | full scan median/range ms | first result median/range ms | peak private bytes | private bytes per million entries |"
    )?;
    writeln!(writer, "| --- | --- | --- | --- | --- | --- | --- | --- |")?;
    writeln!(
        writer,
        "| {} | {} | {} | {}-{} | {} / {}-{} | {} / {}-{} | {} | {} |",
        report.scan_summary.runs,
        report.scan_summary.scanners,
        report.scan_summary.fallback_runs,
        report.scan_summary.entries_min,
        report.scan_summary.entries_max,
        report.scan_summary.elapsed_ms_median,
        report.scan_summary.elapsed_ms_min,
        report.scan_summary.elapsed_ms_max,
        report.scan_summary.first_result_ms_median,
        report.scan_summary.first_result_ms_min,
        report.scan_summary.first_result_ms_max,
        report.scan_summary.peak_private_bytes_max,
        report
            .scan_summary
            .peak_private_bytes_per_million_entries_max
    )?;
    writeln!(writer)?;
    writeln!(writer, "## DiskLoom CSV Export")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "| runs | scanners | fallback runs | entries | export bytes | scan median ms | export median/range ms | total median ms | peak private bytes |"
    )?;
    writeln!(
        writer,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    )?;
    writeln!(
        writer,
        "| {} | {} | {} | {}-{} | {}-{} | {} | {} / {}-{} | {} | {} |",
        report.export_summary.runs,
        report.export_summary.scanners,
        report.export_summary.fallback_runs,
        report.export_summary.entries_min,
        report.export_summary.entries_max,
        report.export_summary.export_bytes_min,
        report.export_summary.export_bytes_max,
        report.export_summary.scan_elapsed_ms_median,
        report.export_summary.export_elapsed_ms_median,
        report.export_summary.export_elapsed_ms_min,
        report.export_summary.export_elapsed_ms_max,
        report.export_summary.total_elapsed_ms_median,
        report.export_summary.peak_private_bytes_max
    )?;
    writeln!(writer)?;
    writeln!(writer, "## Same-Machine Competitor Comparisons")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "| tool | version | context | scope | scope match | competitor median/range ms | DiskLoom median/range ms | ratio | competitor peak private bytes | DiskLoom peak private bytes | validity |"
    )?;
    writeln!(
        writer,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    )?;
    for comparison in report.same_machine_comparisons {
        writeln!(
            writer,
            "| {} | {} | {} | {} | {} | {} / {}-{} | {} / {}-{} | {} | {} | {} | {} |",
            comparison.tool,
            comparison.version,
            comparison.context_match,
            comparison.scanner_scope,
            comparison.scanner_scope_match,
            comparison.competitor_elapsed_ms_median,
            comparison.competitor_elapsed_ms_min,
            comparison.competitor_elapsed_ms_max,
            comparison.diskloom_elapsed_ms_median,
            comparison.diskloom_elapsed_ms_min,
            comparison.diskloom_elapsed_ms_max,
            comparison.diskloom_vs_competitor_median_ratio,
            optional_u64_csv(comparison.competitor_peak_private_bytes_max),
            comparison.diskloom_peak_private_bytes_max,
            comparison.validity
        )?;
    }
    if report.same_machine_comparisons.is_empty() {
        writeln!(
            writer,
            "No same-machine competitor rows were supplied for this suite."
        )?;
    }
    writeln!(writer)?;
    writeln!(writer, "## Public WizTree Reference Claims")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "| claim | source | scope | applicability | claim ms range | DiskLoom median ms | position | ratio vs min | ratio vs max | validity |"
    )?;
    writeln!(
        writer,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    )?;
    for comparison in report.comparisons {
        writeln!(
            writer,
            "| {} | {} | {} | {} | {}-{} | {} | {} | {} | {} | {} |",
            comparison.claim_id,
            comparison.claim_source_url,
            comparison.claim_scan_scope,
            comparison.comparison_applicability,
            comparison.claim_elapsed_ms_min,
            comparison.claim_elapsed_ms_max,
            comparison.diskloom_elapsed_ms_median,
            comparison.diskloom_median_position,
            comparison.diskloom_vs_claim_min_ratio,
            comparison.diskloom_vs_claim_max_ratio,
            comparison.validity
        )?;
    }
    writeln!(writer)?;
    writeln!(
        writer,
        "Public claim rows are source-labeled historical reference points only. Applicability marks whether the DiskLoom run used the same scanner class as the public claim. They are not same-machine competitor benchmarks and must not be used to claim DiskLoom is faster than WizTree."
    )?;
    Ok(())
}

fn write_suite_metadata(
    writer: &mut impl Write,
    options: &SuiteOptions,
    run_context: &SuiteRunContext,
    environment: &BenchmarkEnvironment,
    selected_claim_ids: &[&str],
) -> Result<()> {
    writeln!(
        writer,
        "diskloom_bench_version={}",
        env!("CARGO_PKG_VERSION")
    )?;
    writeln!(writer, "generated_unix_seconds={}", unix_now_seconds())?;
    writeln!(writer, "target_os={}", std::env::consts::OS)?;
    writeln!(writer, "target_arch={}", std::env::consts::ARCH)?;
    writeln!(writer, "command_line={}", run_context.command_line)?;
    writeln!(writer, "git_revision={}", run_context.git_revision)?;
    writeln!(writer, "git_dirty={}", run_context.git_dirty)?;
    writeln!(writer, "dataset_label={}", options.dataset_label)?;
    writeln!(writer, "cache_state={}", options.cache_state)?;
    writeln!(
        writer,
        "competitor_csv={}",
        options
            .competitor_csv
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string())
    )?;
    writeln!(writer, "detected_volume_root={}", environment.volume_root)?;
    writeln!(writer, "detected_filesystem={}", environment.file_system)?;
    writeln!(writer, "detected_drive_type={}", environment.drive_type)?;
    writeln!(
        writer,
        "detected_shell_elevated={}",
        environment.shell_elevated
    )?;
    writeln!(
        writer,
        "detected_windows_version={}",
        environment.windows_version
    )?;
    writeln!(writer, "detected_logical_cpus={}", environment.logical_cpus)?;
    writeln!(
        writer,
        "detected_physical_memory_bytes={}",
        environment.physical_memory_bytes
    )?;
    writeln!(writer, "path={}", options.path.display())?;
    writeln!(writer, "output_dir={}", options.output_dir.display())?;
    writeln!(writer, "scanner={}", scanner_label(options.scanner))?;
    writeln!(writer, "iterations={}", options.iterations)?;
    writeln!(writer, "sample_ms={}", options.sample_ms)?;
    writeln!(writer, "progress_every={}", options.progress_every)?;
    writeln!(
        writer,
        "include_directories={}",
        options.include_directories
    )?;
    writeln!(writer, "public_claims={}", selected_claim_ids.join(","))?;
    writeln!(
        writer,
        "public_claim_validity=reference_only_vendor_claim_not_same_machine"
    )?;
    writeln!(writer)?;
    writeln!(writer, "publication_checklist:")?;
    writeln!(writer, "- hardware=")?;
    writeln!(writer, "- windows_version=")?;
    writeln!(writer, "- filesystem=")?;
    writeln!(writer, "- drive_type=")?;
    writeln!(writer, "- shell_elevated=")?;
    writeln!(writer, "- cache_state=")?;
    writeln!(writer, "- competitor_versions=")?;
    writeln!(writer, "- same_machine_competitor_runs=")?;
    writeln!(
        writer,
        "- note=Do not publish faster-than-WizTree claims from public reference rows alone."
    )?;
    Ok(())
}

fn write_suite_manifest(writer: &mut impl Write, manifest: &SuiteManifest<'_>) -> Result<()> {
    let options = manifest.options;
    let run_context = manifest.run_context;
    let environment = manifest.environment;
    let scan_summary = manifest.scan_summary;
    let export_summary = manifest.export_summary;
    let comparisons = manifest.comparisons;
    let same_machine_comparisons = manifest.same_machine_comparisons;
    let audit_rows = manifest.audit_rows;

    writeln!(writer, "{{")?;
    writeln!(
        writer,
        "  \"schema\": {},",
        json_string("diskloom.benchmark-suite.v1")
    )?;
    writeln!(
        writer,
        "  \"diskloom_bench_version\": {},",
        json_string(env!("CARGO_PKG_VERSION"))
    )?;
    writeln!(
        writer,
        "  \"generated_unix_seconds\": {},",
        unix_now_seconds()
    )?;
    writeln!(
        writer,
        "  \"target_os\": {},",
        json_string(std::env::consts::OS)
    )?;
    writeln!(
        writer,
        "  \"target_arch\": {},",
        json_string(std::env::consts::ARCH)
    )?;
    writeln!(
        writer,
        "  \"audit_status\": {},",
        json_string(suite_audit_status(audit_rows).label())
    )?;
    writeln!(writer, "  \"run\": {{")?;
    writeln!(
        writer,
        "    \"path\": {},",
        json_string(&options.path.display().to_string())
    )?;
    writeln!(
        writer,
        "    \"output_dir\": {},",
        json_string(&options.output_dir.display().to_string())
    )?;
    writeln!(
        writer,
        "    \"dataset_label\": {},",
        json_string(&options.dataset_label)
    )?;
    writeln!(
        writer,
        "    \"cache_state\": {},",
        json_string(&options.cache_state)
    )?;
    writeln!(
        writer,
        "    \"competitor_csv\": {},",
        json_string(
            &options
                .competitor_csv
                .as_ref()
                .map_or_else(String::new, |path| path.display().to_string())
        )
    )?;
    writeln!(
        writer,
        "    \"scanner\": {},",
        json_string(scanner_label(options.scanner))
    )?;
    writeln!(writer, "    \"iterations\": {},", options.iterations)?;
    writeln!(writer, "    \"sample_ms\": {},", options.sample_ms)?;
    writeln!(
        writer,
        "    \"progress_every\": {},",
        options.progress_every
    )?;
    writeln!(
        writer,
        "    \"include_directories\": {}",
        options.include_directories
    )?;
    writeln!(writer, "  }},")?;
    writeln!(writer, "  \"git\": {{")?;
    writeln!(
        writer,
        "    \"revision\": {},",
        json_string(&run_context.git_revision)
    )?;
    writeln!(
        writer,
        "    \"dirty\": {}",
        json_string(&run_context.git_dirty)
    )?;
    writeln!(writer, "  }},")?;
    writeln!(
        writer,
        "  \"command_line\": {},",
        json_string(&run_context.command_line)
    )?;
    writeln!(writer, "  \"environment\": {{")?;
    writeln!(
        writer,
        "    \"volume_root\": {},",
        json_string(&environment.volume_root)
    )?;
    writeln!(
        writer,
        "    \"file_system\": {},",
        json_string(&environment.file_system)
    )?;
    writeln!(
        writer,
        "    \"drive_type\": {},",
        json_string(&environment.drive_type)
    )?;
    writeln!(
        writer,
        "    \"shell_elevated\": {},",
        json_string(&environment.shell_elevated)
    )?;
    writeln!(
        writer,
        "    \"windows_version\": {},",
        json_string(&environment.windows_version)
    )?;
    writeln!(
        writer,
        "    \"logical_cpus\": {},",
        json_string(&environment.logical_cpus)
    )?;
    writeln!(
        writer,
        "    \"physical_memory_bytes\": {}",
        json_string(&environment.physical_memory_bytes)
    )?;
    writeln!(writer, "  }},")?;
    writeln!(writer, "  \"artifacts\": [")?;
    writeln!(writer, "    {},", json_string("scan.csv"))?;
    writeln!(writer, "    {},", json_string("scan-summary.csv"))?;
    writeln!(writer, "    {},", json_string("export.csv"))?;
    writeln!(writer, "    {},", json_string("public-comparison.csv"))?;
    writeln!(
        writer,
        "    {},",
        json_string("same-machine-comparison.csv")
    )?;
    writeln!(writer, "    {},", json_string("audit.csv"))?;
    writeln!(writer, "    {},", json_string("metadata.txt"))?;
    writeln!(writer, "    {},", json_string("manifest.json"))?;
    writeln!(writer, "    {}", json_string("report.md"))?;
    writeln!(writer, "  ],")?;
    writeln!(writer, "  \"scan_summary\": {{")?;
    writeln!(writer, "    \"runs\": {},", scan_summary.runs)?;
    writeln!(
        writer,
        "    \"scanners\": {},",
        json_string(&scan_summary.scanners)
    )?;
    writeln!(
        writer,
        "    \"fallback_runs\": {},",
        scan_summary.fallback_runs
    )?;
    writeln!(writer, "    \"entries_min\": {},", scan_summary.entries_min)?;
    writeln!(writer, "    \"entries_max\": {},", scan_summary.entries_max)?;
    writeln!(
        writer,
        "    \"elapsed_ms_min\": {},",
        scan_summary.elapsed_ms_min
    )?;
    writeln!(
        writer,
        "    \"elapsed_ms_median\": {},",
        scan_summary.elapsed_ms_median
    )?;
    writeln!(
        writer,
        "    \"elapsed_ms_max\": {},",
        scan_summary.elapsed_ms_max
    )?;
    writeln!(
        writer,
        "    \"first_result_ms_median\": {},",
        scan_summary.first_result_ms_median
    )?;
    writeln!(
        writer,
        "    \"peak_private_bytes_max\": {},",
        scan_summary.peak_private_bytes_max
    )?;
    writeln!(
        writer,
        "    \"peak_private_bytes_per_million_entries_max\": {}",
        scan_summary.peak_private_bytes_per_million_entries_max
    )?;
    writeln!(writer, "  }},")?;
    writeln!(writer, "  \"export_summary\": {{")?;
    writeln!(writer, "    \"runs\": {},", export_summary.runs)?;
    writeln!(
        writer,
        "    \"scanners\": {},",
        json_string(&export_summary.scanners)
    )?;
    writeln!(
        writer,
        "    \"fallback_runs\": {},",
        export_summary.fallback_runs
    )?;
    writeln!(
        writer,
        "    \"export_elapsed_ms_median\": {},",
        export_summary.export_elapsed_ms_median
    )?;
    writeln!(
        writer,
        "    \"total_elapsed_ms_median\": {},",
        export_summary.total_elapsed_ms_median
    )?;
    writeln!(
        writer,
        "    \"export_bytes_min\": {},",
        export_summary.export_bytes_min
    )?;
    writeln!(
        writer,
        "    \"export_bytes_max\": {},",
        export_summary.export_bytes_max
    )?;
    writeln!(
        writer,
        "    \"peak_private_bytes_max\": {}",
        export_summary.peak_private_bytes_max
    )?;
    writeln!(writer, "  }},")?;
    writeln!(writer, "  \"audit\": [")?;
    for (idx, row) in audit_rows.iter().enumerate() {
        let suffix = if idx + 1 == audit_rows.len() { "" } else { "," };
        writeln!(writer, "    {{")?;
        writeln!(writer, "      \"check\": {},", json_string(row.check))?;
        writeln!(
            writer,
            "      \"status\": {},",
            json_string(row.status.label())
        )?;
        writeln!(writer, "      \"message\": {}", json_string(&row.message))?;
        writeln!(writer, "    }}{suffix}")?;
    }
    writeln!(writer, "  ],")?;
    writeln!(writer, "  \"same_machine_comparisons\": [")?;
    for (idx, comparison) in same_machine_comparisons.iter().enumerate() {
        let suffix = if idx + 1 == same_machine_comparisons.len() {
            ""
        } else {
            ","
        };
        writeln!(writer, "    {{")?;
        writeln!(writer, "      \"tool\": {},", json_string(&comparison.tool))?;
        writeln!(
            writer,
            "      \"version\": {},",
            json_string(&comparison.version)
        )?;
        writeln!(
            writer,
            "      \"dataset_label\": {},",
            json_string(&comparison.dataset_label)
        )?;
        writeln!(
            writer,
            "      \"cache_state\": {},",
            json_string(&comparison.cache_state)
        )?;
        writeln!(
            writer,
            "      \"scanner_scope\": {},",
            json_string(&comparison.scanner_scope)
        )?;
        writeln!(
            writer,
            "      \"context_match\": {},",
            json_string(comparison.context_match)
        )?;
        writeln!(
            writer,
            "      \"scanner_scope_match\": {},",
            json_string(comparison.scanner_scope_match)
        )?;
        writeln!(
            writer,
            "      \"competitor_runs\": {},",
            comparison.competitor_runs
        )?;
        writeln!(
            writer,
            "      \"diskloom_runs\": {},",
            comparison.diskloom_runs
        )?;
        writeln!(
            writer,
            "      \"competitor_elapsed_ms_median\": {},",
            comparison.competitor_elapsed_ms_median
        )?;
        writeln!(
            writer,
            "      \"diskloom_elapsed_ms_median\": {},",
            comparison.diskloom_elapsed_ms_median
        )?;
        writeln!(
            writer,
            "      \"diskloom_vs_competitor_median_ratio\": {},",
            json_string(&comparison.diskloom_vs_competitor_median_ratio)
        )?;
        writeln!(
            writer,
            "      \"validity\": {}",
            json_string(comparison.validity)
        )?;
        writeln!(writer, "    }}{suffix}")?;
    }
    writeln!(writer, "  ],")?;
    writeln!(writer, "  \"public_claims\": [")?;
    for (idx, comparison) in comparisons.iter().enumerate() {
        let suffix = if idx + 1 == comparisons.len() {
            ""
        } else {
            ","
        };
        writeln!(writer, "    {{")?;
        writeln!(
            writer,
            "      \"claim_id\": {},",
            json_string(comparison.claim_id)
        )?;
        writeln!(
            writer,
            "      \"claim_source_url\": {},",
            json_string(comparison.claim_source_url)
        )?;
        writeln!(
            writer,
            "      \"claim_scan_scope\": {},",
            json_string(comparison.claim_scan_scope)
        )?;
        writeln!(
            writer,
            "      \"comparison_applicability\": {},",
            json_string(comparison.comparison_applicability)
        )?;
        writeln!(
            writer,
            "      \"claim_elapsed_ms_min\": {},",
            comparison.claim_elapsed_ms_min
        )?;
        writeln!(
            writer,
            "      \"claim_elapsed_ms_max\": {},",
            comparison.claim_elapsed_ms_max
        )?;
        writeln!(
            writer,
            "      \"diskloom_elapsed_ms_median\": {},",
            comparison.diskloom_elapsed_ms_median
        )?;
        writeln!(
            writer,
            "      \"diskloom_median_position\": {},",
            json_string(comparison.diskloom_median_position)
        )?;
        writeln!(
            writer,
            "      \"validity\": {}",
            json_string(comparison.validity)
        )?;
        writeln!(writer, "    }}{suffix}")?;
    }
    writeln!(writer, "  ]")?;
    writeln!(writer, "}}")?;
    Ok(())
}

fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn detect_suite_run_context() -> SuiteRunContext {
    SuiteRunContext {
        command_line: current_command_line(),
        git_revision: git_revision(),
        git_dirty: git_dirty_state(),
    }
}

fn current_command_line() -> String {
    std::env::args()
        .map(|arg| shell_quote_arg(&arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn single_line_value(value: &str, fallback: &str) -> String {
    let sanitized = value.replace(['\r', '\n'], " ");
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(output, "\\u{:04x}", ch as u32);
            }
            ch => output.push(ch),
        }
    }
    output.push('"');
    output
}

fn shell_quote_arg(arg: &str) -> String {
    let sanitized = arg.replace(['\r', '\n'], " ");
    if sanitized.is_empty()
        || sanitized
            .chars()
            .any(|ch| ch.is_ascii_whitespace() || ch == '"')
    {
        format!("\"{}\"", sanitized.replace('"', "\\\""))
    } else {
        sanitized
    }
}

fn git_revision() -> String {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        Ok(output) => format!("unknown(exit={})", output.status),
        Err(error) => format!("unknown({error})"),
    }
}

fn git_dirty_state() -> String {
    let output = ProcessCommand::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output();
    match output {
        Ok(output) if output.status.success() => bool_label(!output.stdout.is_empty()).to_owned(),
        Ok(output) => format!("unknown(exit={})", output.status),
        Err(error) => format!("unknown({error})"),
    }
}

#[cfg(not(windows))]
fn bool_label(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn logical_cpus() -> String {
    thread::available_parallelism()
        .map(|cpus| cpus.get().to_string())
        .unwrap_or_else(|error| format!("unknown({error})"))
}

#[cfg(windows)]
fn detect_benchmark_environment(path: &Path) -> BenchmarkEnvironment {
    let volume_root = windows_volume_root(path).unwrap_or_else(|| "unknown".to_owned());
    BenchmarkEnvironment {
        file_system: windows_file_system_name(&volume_root),
        drive_type: windows_drive_type(&volume_root),
        shell_elevated: windows_shell_elevated(),
        windows_version: windows_version(),
        logical_cpus: logical_cpus(),
        physical_memory_bytes: windows_physical_memory_bytes(),
        volume_root,
    }
}

#[cfg(not(windows))]
fn detect_benchmark_environment(_: &Path) -> BenchmarkEnvironment {
    BenchmarkEnvironment {
        volume_root: "unsupported".to_owned(),
        file_system: "unsupported".to_owned(),
        drive_type: "unsupported".to_owned(),
        shell_elevated: "unsupported".to_owned(),
        windows_version: "unsupported".to_owned(),
        logical_cpus: logical_cpus(),
        physical_memory_bytes: "unsupported".to_owned(),
    }
}

#[cfg(windows)]
fn windows_volume_root(path: &Path) -> Option<String> {
    use std::path::{Component, Prefix};

    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut components = path.components();
    let prefix = match components.next()? {
        Component::Prefix(prefix) => prefix.kind(),
        _ => return None,
    };

    match prefix {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            Some(format!("{}:\\", letter as char))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => Some(format!(
            "\\\\{}\\{}\\",
            server.to_string_lossy(),
            share.to_string_lossy()
        )),
        Prefix::DeviceNS(_) | Prefix::Verbatim(_) => None,
    }
}

#[cfg(windows)]
fn windows_file_system_name(volume_root: &str) -> String {
    if volume_root == "unknown" {
        return "unknown".to_owned();
    }

    use windows::{Win32::Storage::FileSystem::GetVolumeInformationW, core::PCWSTR};

    let root_wide = to_wide(volume_root);
    let mut fs_name = [0_u16; 32];

    // SAFETY: `root_wide` is null-terminated and `fs_name` is a valid mutable UTF-16 buffer.
    let result = unsafe {
        GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut fs_name),
        )
    };

    match result {
        Ok(()) => trim_nul_utf16(&fs_name),
        Err(error) => format!("unknown({error})"),
    }
}

#[cfg(windows)]
fn windows_drive_type(volume_root: &str) -> String {
    if volume_root == "unknown" {
        return "unknown".to_owned();
    }

    use windows::{Win32::Storage::FileSystem::GetDriveTypeW, core::PCWSTR};

    let root_wide = to_wide(volume_root);
    // SAFETY: `root_wide` is null-terminated and valid for the duration of the call.
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };
    drive_type_label(drive_type).to_owned()
}

#[cfg(windows)]
fn drive_type_label(drive_type: u32) -> &'static str {
    match drive_type {
        0 => "unknown",
        1 => "no_root_dir",
        2 => "removable",
        3 => "fixed",
        4 => "remote",
        5 => "cdrom",
        6 => "ramdisk",
        _ => "unrecognized",
    }
}

#[cfg(windows)]
fn windows_shell_elevated() -> String {
    use std::{ffi::c_void, mem::size_of};

    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle for the current process. `token` is a
    // valid output pointer and is closed before returning if it is opened.
    let token_result = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if let Err(error) = token_result {
        return format!("unknown({error})");
    }

    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0_u32;
    // SAFETY: `elevation` points to a properly sized TOKEN_ELEVATION buffer and `returned` is a
    // valid output pointer. `token` is a process token handle opened above.
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    // SAFETY: `token` was opened by OpenProcessToken and is no longer used after this call.
    let _ = unsafe { CloseHandle(token) };

    match result {
        Ok(()) => bool_label(elevation.TokenIsElevated != 0).to_owned(),
        Err(error) => format!("unknown({error})"),
    }
}

#[cfg(windows)]
fn windows_physical_memory_bytes() -> String {
    use std::mem::size_of;

    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..MEMORYSTATUSEX::default()
    };

    // SAFETY: `status` is initialized with its documented size and points to writable memory.
    match unsafe { GlobalMemoryStatusEx(&mut status) } {
        Ok(()) => status.ullTotalPhys.to_string(),
        Err(error) => format!("unknown({error})"),
    }
}

#[cfg(windows)]
fn windows_version() -> String {
    let product_name = registry_string("ProductName").unwrap_or_else(|| "Windows".to_owned());
    let display_version =
        registry_string("DisplayVersion").or_else(|| registry_string("ReleaseId"));
    let build = registry_string("CurrentBuildNumber").or_else(|| registry_string("CurrentBuild"));
    let ubr = registry_dword("UBR");

    match (display_version, build, ubr) {
        (Some(display), Some(build), Some(ubr)) => {
            format!("{product_name} {display} build {build}.{ubr}")
        }
        (Some(display), Some(build), None) => format!("{product_name} {display} build {build}"),
        (_, Some(build), Some(ubr)) => format!("{product_name} build {build}.{ubr}"),
        (_, Some(build), None) => format!("{product_name} build {build}"),
        _ => product_name,
    }
}

#[cfg(windows)]
fn registry_string(value_name: &str) -> Option<String> {
    use std::ffi::c_void;

    use windows::{
        Win32::System::Registry::{
            HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
        },
        core::PCWSTR,
    };

    const WINDOWS_NT_CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let subkey = to_wide(WINDOWS_NT_CURRENT_VERSION);
    let value = to_wide(value_name);
    let mut buffer = vec![0_u16; 256];
    let mut bytes = (buffer.len() * size_of_u16()) as u32;

    // SAFETY: `subkey`, `value`, and `buffer` are valid for the duration of the call. The buffer
    // size is supplied in bytes as required by RegGetValueW.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            None,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    if status.0 != 0 {
        return None;
    }

    let units = (bytes as usize / size_of_u16()).min(buffer.len());
    let value = trim_nul_utf16(&buffer[..units]);
    (!value.is_empty()).then_some(value)
}

#[cfg(windows)]
fn registry_dword(value_name: &str) -> Option<u32> {
    use std::ffi::c_void;

    use windows::{
        Win32::System::Registry::{
            HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
        },
        core::PCWSTR,
    };

    const WINDOWS_NT_CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let subkey = to_wide(WINDOWS_NT_CURRENT_VERSION);
    let value = to_wide(value_name);
    let mut data = 0_u32;
    let mut bytes = std::mem::size_of::<u32>() as u32;

    // SAFETY: `subkey` and `value` are null-terminated and `data` is a valid DWORD output buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY,
            None,
            Some((&mut data as *mut u32).cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    if status.0 == 0 { Some(data) } else { None }
}

#[cfg(windows)]
fn size_of_u16() -> usize {
    std::mem::size_of::<u16>()
}

#[cfg(windows)]
fn bool_label(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn trim_nul_utf16(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|code_unit| *code_unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

fn scanner_label(scanner: ScannerMode) -> &'static str {
    match scanner {
        ScannerMode::Auto => "auto",
        ScannerMode::Fallback => "fallback",
        ScannerMode::Ntfs => "ntfs",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Args, AuditStatus, BenchmarkEnvironment, Command, CountingWriter, ExportMeasurement,
        ExportSummary, MeasurementSummary, PublicClaimId, ScanMeasurement, SuiteAuditRow,
        SuiteManifest, SuiteOptions, SuiteReport, SuiteRunContext, compare_summary_to_claim,
        compare_summary_to_competitors, json_string, parse_competitor_measurements,
        parse_measurements, per_million, public_claim, ratio_decimal, scan_measurements_to_rows,
        selected_claims, shell_quote_arg, single_line_value, suite_audit_rows, suite_audit_status,
        suite_same_machine_comparisons, summarize_export_measurements, summarize_rows,
        write_competitor_template, write_export_measurements, write_measurements,
        write_public_comparisons, write_same_machine_comparisons, write_suite_audit,
        write_suite_manifest, write_suite_metadata, write_suite_report, write_summary,
    };
    use clap::Parser;

    #[test]
    fn write_measurements_should_emit_csv_rows() {
        let measurements = [ScanMeasurement {
            iteration: 1,
            scanner: "fallback",
            fallback: false,
            elapsed_ms: 10,
            first_result_ms: 2,
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

        assert!(output.contains("1,fallback,0,10,2,3,2,1,0,100,90,80,70,30000000,4"));
    }

    #[test]
    fn write_export_measurements_should_emit_csv_rows() {
        let measurements = [ExportMeasurement {
            iteration: 1,
            scanner: "fallback",
            fallback: false,
            scan_elapsed_ms: 10,
            export_elapsed_ms: 3,
            total_elapsed_ms: 13,
            export_bytes: 120,
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

        write_export_measurements(&mut output, &measurements).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("1,fallback,0,10,3,13,120,3,2,1,0,100,90,80,70,30000000,4"));
    }

    #[test]
    fn summarize_export_measurements_should_compute_export_median() {
        let measurements = [
            export_measurement(1, 10, 3, 13),
            export_measurement(2, 20, 7, 27),
            export_measurement(3, 30, 5, 35),
        ];

        let summary = summarize_export_measurements(&measurements).unwrap();

        assert_eq!(summary.export_elapsed_ms_median, 5);
        assert_eq!(summary.export_elapsed_ms_min, 3);
        assert_eq!(summary.export_elapsed_ms_max, 7);
    }

    #[test]
    fn scan_measurements_to_rows_should_preserve_first_result() {
        let measurements = [ScanMeasurement {
            iteration: 1,
            scanner: "fallback",
            fallback: false,
            elapsed_ms: 10,
            first_result_ms: 2,
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

        let rows = scan_measurements_to_rows(&measurements);

        assert_eq!(rows[0].first_result_ms, 2);
    }

    #[test]
    fn counting_writer_should_track_written_bytes() {
        let mut writer = CountingWriter::new(Vec::new());

        std::io::Write::write_all(&mut writer, b"diskloom").unwrap();

        assert_eq!(writer.bytes(), 8);
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
        assert_eq!(summary.first_result_ms_median, 20);
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
            first_result_ms_min: 1,
            first_result_ms_median: 2,
            first_result_ms_max: 3,
            peak_working_set_bytes_max: 110,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let mut output = Vec::new();

        write_summary(&mut output, &summary).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("3,fallback,0,3,3,10,20,30,1,2,3,110,95,31666666"));
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
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };

        let comparison =
            compare_summary_to_claim(&summary, public_claim(PublicClaimId::WizTreeSsd460Gb));

        assert_eq!(comparison.diskloom_vs_claim_min_ratio, "0.200");
        assert_eq!(comparison.diskloom_vs_claim_max_ratio, "0.200");
        assert_eq!(comparison.diskloom_median_position, "below_public_range");
        assert_eq!(comparison.claim_scan_scope, "ntfs_mft");
        assert_eq!(
            comparison.comparison_applicability,
            "not_aligned_requires_ntfs_mft"
        );
        assert_eq!(
            comparison.validity,
            "reference_only_vendor_claim_not_same_machine"
        );
    }

    #[test]
    fn compare_summary_to_claim_should_support_public_ranges() {
        let summary = MeasurementSummary {
            runs: 3,
            scanners: "ntfs".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 5_900,
            elapsed_ms_median: 6_000,
            elapsed_ms_max: 6_100,
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };

        let comparison = compare_summary_to_claim(
            &summary,
            public_claim(PublicClaimId::WizTreeSsd500GbTypical),
        );

        assert_eq!(comparison.claim_elapsed_ms_min, 3_000);
        assert_eq!(comparison.claim_elapsed_ms_max, 8_000);
        assert_eq!(comparison.diskloom_vs_claim_min_ratio, "2.000");
        assert_eq!(comparison.diskloom_vs_claim_max_ratio, "0.750");
        assert_eq!(comparison.diskloom_median_position, "within_public_range");
        assert_eq!(comparison.comparison_applicability, "aligned_ntfs_mft");
    }

    #[test]
    fn compare_summary_to_claim_should_mark_median_above_public_range() {
        let summary = MeasurementSummary {
            runs: 3,
            scanners: "ntfs".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 8_900,
            elapsed_ms_median: 9_000,
            elapsed_ms_max: 9_100,
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };

        let comparison = compare_summary_to_claim(
            &summary,
            public_claim(PublicClaimId::WizTreeSsd500GbTypical),
        );

        assert_eq!(comparison.diskloom_median_position, "above_public_range");
    }

    #[test]
    fn compare_summary_to_claim_should_flag_mixed_scanner_runs() {
        let summary = MeasurementSummary {
            runs: 3,
            scanners: "fallback+ntfs".to_owned(),
            fallback_runs: 1,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 5_900,
            elapsed_ms_median: 6_000,
            elapsed_ms_max: 6_100,
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };

        let comparison =
            compare_summary_to_claim(&summary, public_claim(PublicClaimId::WizTreeSsd460Gb));

        assert_eq!(
            comparison.comparison_applicability,
            "mixed_or_fallback_not_aligned"
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
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let comparison =
            compare_summary_to_claim(&summary, public_claim(PublicClaimId::WizTreeSsd460Gb));
        let mut output = Vec::new();

        write_public_comparisons(&mut output, &[comparison]).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("claim_scan_scope,claim_elapsed_ms_min,claim_elapsed_ms_max"));
        assert!(output.contains("comparison_applicability"));
        assert!(output.contains("diskloom_vs_claim_min_ratio,diskloom_vs_claim_max_ratio"));
        assert!(output.contains("diskloom_median_position"));
        assert!(output.contains("not_aligned_requires_ntfs_mft"));
        assert!(output.contains("below_public_range"));
        assert!(output.contains("wiztree-ssd-460gb"));
        assert!(output.contains("reference_only_vendor_claim_not_same_machine"));
    }

    #[test]
    fn write_public_comparisons_should_emit_multiple_rows() {
        let summary = MeasurementSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 500,
            elapsed_ms_median: 1_046,
            elapsed_ms_max: 1_100,
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let comparisons = [
            compare_summary_to_claim(
                &summary,
                public_claim(PublicClaimId::WizTreeSsd500GbTypical),
            ),
            compare_summary_to_claim(&summary, public_claim(PublicClaimId::WizTreeSsd460Gb)),
            compare_summary_to_claim(&summary, public_claim(PublicClaimId::WizTreeHdd25Gb)),
        ];
        let mut output = Vec::new();

        write_public_comparisons(&mut output, &comparisons).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("wiztree-ssd-460gb"));
        assert!(output.contains("wiztree-hdd-25gb"));
        assert!(output.contains("wiztree-ssd-500gb-typical"));
    }

    #[test]
    fn selected_claims_should_default_to_all_public_claims() {
        let claims = selected_claims(&[]);

        assert_eq!(
            claims,
            vec![
                PublicClaimId::WizTreeSsd500GbTypical,
                PublicClaimId::WizTreeSsd460Gb,
                PublicClaimId::WizTreeHdd25Gb
            ]
        );
    }

    #[test]
    fn compare_public_cli_should_default_to_all_claims() {
        let args =
            Args::try_parse_from(["diskloom-bench", "compare-public", "target/bench.csv"]).unwrap();

        let Command::ComparePublic { claim, .. } = args.command else {
            panic!("expected compare-public command");
        };
        assert!(claim.is_empty());
    }

    #[test]
    fn compare_public_cli_should_accept_multiple_claims() {
        let args = Args::try_parse_from([
            "diskloom-bench",
            "compare-public",
            "target/bench.csv",
            "--claim",
            "wiztree-ssd-500gb-typical",
            "--claim",
            "wiztree-ssd-460gb",
        ])
        .unwrap();

        let Command::ComparePublic { claim, .. } = args.command else {
            panic!("expected compare-public command");
        };
        assert_eq!(
            claim,
            vec![
                PublicClaimId::WizTreeSsd500GbTypical,
                PublicClaimId::WizTreeSsd460Gb
            ]
        );
    }

    #[test]
    fn compare_competitor_cli_should_accept_context_labels() {
        let args = Args::try_parse_from([
            "diskloom-bench",
            "compare-competitor",
            "target/bench.csv",
            "target/competitors.csv",
            "--dataset-label",
            "workstation-c",
            "--cache-state",
            "warm",
        ])
        .unwrap();

        let Command::CompareCompetitor {
            dataset_label,
            cache_state,
            ..
        } = args.command
        else {
            panic!("expected compare-competitor command");
        };
        assert_eq!(dataset_label, "workstation-c");
        assert_eq!(cache_state, "warm");
    }

    #[test]
    fn competitor_template_cli_should_accept_examples_flag() {
        let args =
            Args::try_parse_from(["diskloom-bench", "competitor-template", "--examples"]).unwrap();

        let Command::CompetitorTemplate { examples } = args.command else {
            panic!("expected competitor-template command");
        };
        assert!(examples);
    }

    #[test]
    fn parse_competitor_measurements_should_parse_optional_memory() {
        let input = "\
tool,version,dataset_label,cache_state,scanner_scope,elapsed_ms,peak_private_bytes,notes
WizTree,4.25,workstation-c,warm,ntfs_mft,600,1000,manual run
WizTree,4.25,workstation-c,warm,ntfs_mft,700,,
";

        let rows = parse_competitor_measurements(input).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tool, "WizTree");
        assert_eq!(rows[0].peak_private_bytes, Some(1000));
        assert_eq!(rows[1].peak_private_bytes, None);
    }

    #[test]
    fn compare_summary_to_competitors_should_group_and_mark_context() {
        let diskloom_summary = MeasurementSummary {
            runs: 3,
            scanners: "ntfs".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 900,
            elapsed_ms_median: 1_000,
            elapsed_ms_max: 1_100,
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 120,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let rows = parse_competitor_measurements(
            "\
tool,version,dataset_label,cache_state,scanner_scope,elapsed_ms,peak_private_bytes
WizTree,4.25,workstation-c,warm,ntfs_mft,500,40
WizTree,4.25,workstation-c,warm,ntfs_mft,700,50
TreeSize,9.0,other,warm,traversal,2000,
",
        )
        .unwrap();

        let comparisons =
            compare_summary_to_competitors(&diskloom_summary, &rows, "workstation-c", "warm")
                .unwrap();

        assert_eq!(comparisons.len(), 2);
        assert_eq!(comparisons[1].tool, "WizTree");
        assert_eq!(comparisons[1].competitor_elapsed_ms_median, 600);
        assert_eq!(comparisons[1].diskloom_vs_competitor_median_ratio, "1.666");
        assert_eq!(comparisons[1].context_match, "matched");
        assert_eq!(comparisons[1].scanner_scope_match, "aligned_ntfs_mft");
        assert_eq!(comparisons[1].validity, "same_machine_user_supplied");
        assert_eq!(comparisons[1].diskloom_private_bytes_delta, Some(45));
    }

    #[test]
    fn write_same_machine_comparisons_should_emit_csv_rows() {
        let comparison = sample_same_machine_comparison();
        let mut output = Vec::new();

        write_same_machine_comparisons(&mut output, &[comparison]).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("tool,version,dataset_label,cache_state"));
        assert!(output.contains("WizTree,4.25,repo-smoke,warm,ntfs_mft,matched,aligned_ntfs_mft"));
        assert!(output.contains("same_machine_user_supplied"));
    }

    #[test]
    fn write_competitor_template_should_emit_header_only_by_default() {
        let mut output = Vec::new();

        write_competitor_template(&mut output, false).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert_eq!(
            output,
            "tool,version,dataset_label,cache_state,scanner_scope,elapsed_ms,peak_private_bytes,notes\n"
        );
    }

    #[test]
    fn write_competitor_template_should_emit_example_scope_rows() {
        let mut output = Vec::new();

        write_competitor_template(&mut output, true).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("WizTree,example,example-dataset,warm,ntfs_mft"));
        assert!(output.contains("TreeSize,example,example-dataset,warm,traversal"));
    }

    #[test]
    fn suite_same_machine_comparisons_should_return_empty_without_input() {
        let options = SuiteOptions {
            path: std::path::PathBuf::from("."),
            output_dir: std::path::PathBuf::from("target/bench-suite"),
            dataset_label: "repo-smoke".to_owned(),
            cache_state: "warm".to_owned(),
            iterations: 3,
            sample_ms: 10,
            progress_every: 1024,
            scanner: super::ScannerMode::Fallback,
            include_directories: true,
            claims: Vec::new(),
            competitor_csv: None,
        };

        let comparisons =
            suite_same_machine_comparisons(&options, &sample_measurement_summary()).unwrap();

        assert!(comparisons.is_empty());
    }

    #[test]
    fn suite_same_machine_comparisons_should_read_competitor_csv() {
        let competitor_csv = std::env::temp_dir().join(format!(
            "diskloom-suite-competitors-{}-{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &competitor_csv,
            "\
tool,version,dataset_label,cache_state,scanner_scope,elapsed_ms,peak_private_bytes
WizTree,4.25,repo-smoke,warm,traversal,500,40
WizTree,4.25,repo-smoke,warm,traversal,700,50
",
        )
        .unwrap();
        let options = SuiteOptions {
            path: std::path::PathBuf::from("."),
            output_dir: std::path::PathBuf::from("target/bench-suite"),
            dataset_label: "repo-smoke".to_owned(),
            cache_state: "warm".to_owned(),
            iterations: 3,
            sample_ms: 10,
            progress_every: 1024,
            scanner: super::ScannerMode::Fallback,
            include_directories: true,
            claims: Vec::new(),
            competitor_csv: Some(competitor_csv.clone()),
        };

        let comparisons =
            suite_same_machine_comparisons(&options, &sample_measurement_summary()).unwrap();
        std::fs::remove_file(competitor_csv).unwrap();

        assert_eq!(comparisons[0].scanner_scope_match, "aligned_traversal");
        assert_eq!(comparisons[0].validity, "same_machine_user_supplied");
    }

    #[test]
    fn compare_summary_to_competitors_should_reject_scope_mismatches() {
        let rows = parse_competitor_measurements(
            "\
tool,version,dataset_label,cache_state,scanner_scope,elapsed_ms,peak_private_bytes
WizTree,4.25,repo-smoke,warm,ntfs_mft,500,40
",
        )
        .unwrap();

        let comparisons = compare_summary_to_competitors(
            &sample_measurement_summary(),
            &rows,
            "repo-smoke",
            "warm",
        )
        .unwrap();

        assert_eq!(comparisons[0].validity, "scanner_scope_mismatch");
    }

    #[test]
    fn suite_cli_should_default_context_labels() {
        let args =
            Args::try_parse_from(["diskloom-bench", "suite", ".", "target/bench-suite"]).unwrap();

        let Command::Suite {
            dataset_label,
            cache_state,
            ..
        } = args.command
        else {
            panic!("expected suite command");
        };
        assert_eq!(dataset_label, "unspecified");
        assert_eq!(cache_state, "unknown");
    }

    #[test]
    fn write_suite_report_should_mark_public_claims_reference_only() {
        let scan_summary = MeasurementSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 500,
            elapsed_ms_median: 1_046,
            elapsed_ms_max: 1_100,
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let export_summary = ExportSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            export_bytes_min: 100,
            export_bytes_max: 120,
            scan_elapsed_ms_median: 10,
            export_elapsed_ms_min: 3,
            export_elapsed_ms_median: 5,
            export_elapsed_ms_max: 7,
            total_elapsed_ms_median: 15,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let comparisons = [compare_summary_to_claim(
            &scan_summary,
            public_claim(PublicClaimId::WizTreeSsd460Gb),
        )];
        let run_context = sample_run_context();
        let environment = sample_environment();
        let audit_rows = [SuiteAuditRow::new(
            "public_claims",
            AuditStatus::Warning,
            "Public WizTree rows are reference-only; same-machine competitor runs are required for speed claims.",
        )];
        let mut output = Vec::new();

        write_suite_report(
            &mut output,
            &SuiteReport {
                path: std::path::Path::new("."),
                output_dir: std::path::Path::new("target/bench-suite"),
                dataset_label: "repo-smoke",
                cache_state: "warm",
                scanner: super::ScannerMode::Fallback,
                iterations: 3,
                sample_ms: 10,
                progress_every: 1024,
                include_directories: true,
                run_context: &run_context,
                environment: &environment,
                scan_summary: &scan_summary,
                export_summary: &export_summary,
                comparisons: &comparisons,
                same_machine_comparisons: &[],
                audit_rows: &audit_rows,
            },
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("reference_only_vendor_claim_not_same_machine"));
        assert!(output.contains("must not be used to claim DiskLoom is faster than WizTree"));
        assert!(output.contains("applicability"));
        assert!(output.contains("not_aligned_requires_ntfs_mft"));
        assert!(output.contains("position"));
        assert!(output.contains("below_public_range"));
        assert!(output.contains("ratio vs min"));
        assert!(output.contains("ratio vs max"));
        assert!(output.contains("## Environment"));
        assert!(output.contains("Git revision"));
        assert!(output.contains("Dataset label: `repo-smoke`"));
        assert!(output.contains("Cache state: `warm`"));
        assert!(output.contains("## Benchmark Audit"));
        assert!(output.contains("Overall status: `warning`"));
        assert!(output.contains("## Same-Machine Competitor Comparisons"));
        assert!(output.contains("No same-machine competitor rows were supplied"));
    }

    #[test]
    fn write_suite_report_should_include_same_machine_comparisons() {
        let scan_summary = sample_measurement_summary();
        let export_summary = ExportSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            export_bytes_min: 100,
            export_bytes_max: 120,
            scan_elapsed_ms_median: 10,
            export_elapsed_ms_min: 3,
            export_elapsed_ms_median: 5,
            export_elapsed_ms_max: 7,
            total_elapsed_ms_median: 15,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let same_machine_comparisons = [sample_same_machine_comparison()];
        let run_context = sample_run_context();
        let environment = sample_environment();
        let mut output = Vec::new();

        write_suite_report(
            &mut output,
            &SuiteReport {
                path: std::path::Path::new("."),
                output_dir: std::path::Path::new("target/bench-suite"),
                dataset_label: "repo-smoke",
                cache_state: "warm",
                scanner: super::ScannerMode::Fallback,
                iterations: 3,
                sample_ms: 10,
                progress_every: 1024,
                include_directories: true,
                run_context: &run_context,
                environment: &environment,
                scan_summary: &scan_summary,
                export_summary: &export_summary,
                comparisons: &[],
                same_machine_comparisons: &same_machine_comparisons,
                audit_rows: &[],
            },
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("| WizTree | 4.25 | matched | ntfs_mft | aligned_ntfs_mft |"));
    }

    #[test]
    fn write_suite_metadata_should_include_publication_checklist() {
        let options = SuiteOptions {
            path: std::path::PathBuf::from("."),
            output_dir: std::path::PathBuf::from("target/bench-suite"),
            dataset_label: "repo-smoke".to_owned(),
            cache_state: "warm".to_owned(),
            iterations: 3,
            sample_ms: 10,
            progress_every: 1024,
            scanner: super::ScannerMode::Fallback,
            include_directories: true,
            claims: Vec::new(),
            competitor_csv: None,
        };
        let run_context = sample_run_context();
        let environment = sample_environment();
        let mut output = Vec::new();

        write_suite_metadata(
            &mut output,
            &options,
            &run_context,
            &environment,
            &[
                "wiztree-ssd-500gb-typical",
                "wiztree-ssd-460gb",
                "wiztree-hdd-25gb",
            ],
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("publication_checklist:"));
        assert!(output.contains("command_line=diskloom-bench suite . target/bench-suite"));
        assert!(output.contains("git_revision=abcdef123456"));
        assert!(output.contains("git_dirty=false"));
        assert!(output.contains("dataset_label=repo-smoke"));
        assert!(output.contains("cache_state=warm"));
        assert!(output.contains("detected_filesystem=NTFS"));
        assert!(output.contains("detected_logical_cpus=8"));
        assert!(output.contains("detected_physical_memory_bytes=17179869184"));
        assert!(output.contains(
            "public_claims=wiztree-ssd-500gb-typical,wiztree-ssd-460gb,wiztree-hdd-25gb"
        ));
        assert!(output.contains("same_machine_competitor_runs="));
        assert!(output.contains("reference_only_vendor_claim_not_same_machine"));
    }

    #[test]
    fn suite_audit_rows_should_fail_missing_context_and_warn_reference_claims() {
        let options = SuiteOptions {
            path: std::path::PathBuf::from("."),
            output_dir: std::path::PathBuf::from("target/bench-suite"),
            dataset_label: "unspecified".to_owned(),
            cache_state: "unknown".to_owned(),
            iterations: 1,
            sample_ms: 10,
            progress_every: 1024,
            scanner: super::ScannerMode::Fallback,
            include_directories: true,
            claims: Vec::new(),
            competitor_csv: None,
        };
        let summary = MeasurementSummary {
            runs: 1,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 500,
            elapsed_ms_median: 500,
            elapsed_ms_max: 500,
            first_result_ms_min: 10,
            first_result_ms_median: 10,
            first_result_ms_max: 10,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let comparisons = [compare_summary_to_claim(
            &summary,
            public_claim(PublicClaimId::WizTreeSsd460Gb),
        )];
        let same_machine_comparisons = [sample_same_machine_comparison()];

        let rows = suite_audit_rows(
            &options,
            &sample_run_context(),
            &comparisons,
            &same_machine_comparisons,
        );

        assert!(
            rows.iter()
                .any(|row| { row.check == "dataset_label" && row.status == AuditStatus::Fail })
        );
        assert!(
            rows.iter()
                .any(|row| { row.check == "public_claims" && row.status == AuditStatus::Warning })
        );
        assert!(rows.iter().any(|row| {
            row.check == "same_machine_competitors" && row.status == AuditStatus::Pass
        }));
        assert_eq!(suite_audit_status(&rows), AuditStatus::Fail);
    }

    #[test]
    fn write_suite_audit_should_escape_csv_messages() {
        let rows = [SuiteAuditRow::new(
            "sample",
            AuditStatus::Warning,
            "contains, comma and \"quote\"",
        )];
        let mut output = Vec::new();

        write_suite_audit(&mut output, &rows).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("sample,warning,\"contains, comma and \"\"quote\"\"\""));
    }

    #[test]
    fn write_suite_manifest_should_emit_json_bundle_summary() {
        let options = SuiteOptions {
            path: std::path::PathBuf::from("."),
            output_dir: std::path::PathBuf::from("target/bench-suite"),
            dataset_label: "repo-smoke".to_owned(),
            cache_state: "warm".to_owned(),
            iterations: 3,
            sample_ms: 10,
            progress_every: 1024,
            scanner: super::ScannerMode::Fallback,
            include_directories: true,
            claims: Vec::new(),
            competitor_csv: None,
        };
        let scan_summary = MeasurementSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 500,
            elapsed_ms_median: 1_046,
            elapsed_ms_max: 1_100,
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let export_summary = ExportSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            export_bytes_min: 100,
            export_bytes_max: 120,
            scan_elapsed_ms_median: 10,
            export_elapsed_ms_min: 3,
            export_elapsed_ms_median: 5,
            export_elapsed_ms_max: 7,
            total_elapsed_ms_median: 15,
            peak_working_set_bytes_max: 100,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        };
        let comparisons = [compare_summary_to_claim(
            &scan_summary,
            public_claim(PublicClaimId::WizTreeSsd460Gb),
        )];
        let same_machine_comparisons = [sample_same_machine_comparison()];
        let run_context = sample_run_context();
        let environment = sample_environment();
        let audit_rows = suite_audit_rows(
            &options,
            &run_context,
            &comparisons,
            &same_machine_comparisons,
        );
        let manifest = SuiteManifest {
            options: &options,
            run_context: &run_context,
            environment: &environment,
            scan_summary: &scan_summary,
            export_summary: &export_summary,
            comparisons: &comparisons,
            same_machine_comparisons: &same_machine_comparisons,
            audit_rows: &audit_rows,
        };
        let mut output = Vec::new();

        write_suite_manifest(&mut output, &manifest).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("\"schema\": \"diskloom.benchmark-suite.v1\""));
        assert!(output.contains("\"audit_status\": \"warning\""));
        assert!(output.contains("\"dataset_label\": \"repo-smoke\""));
        assert!(output.contains("\"cache_state\": \"warm\""));
        assert!(output.contains("\"competitor_csv\": \"\""));
        assert!(output.contains("\"audit.csv\""));
        assert!(output.contains("\"manifest.json\""));
        assert!(output.contains("\"claim_id\": \"wiztree-ssd-460gb\""));
        assert!(output.contains("\"same-machine-comparison.csv\""));
        assert!(output.contains("\"same_machine_comparisons\""));
        assert!(output.contains("\"tool\": \"WizTree\""));
        assert!(output.contains("\"scanner_scope_match\": \"aligned_ntfs_mft\""));
    }

    #[test]
    fn shell_quote_arg_should_quote_spaces_and_quotes() {
        assert_eq!(shell_quote_arg("simple"), "simple");
        assert_eq!(shell_quote_arg("two words"), "\"two words\"");
        assert_eq!(shell_quote_arg("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn single_line_value_should_trim_newlines_and_empty_values() {
        assert_eq!(
            single_line_value(" warm\r\nsecond run ", "unknown"),
            "warm  second run"
        );
        assert_eq!(single_line_value("\n\t", "unknown"), "unknown");
    }

    #[test]
    fn json_string_should_escape_control_characters() {
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }

    #[cfg(windows)]
    #[test]
    fn drive_type_label_should_name_known_win32_values() {
        assert_eq!(super::drive_type_label(3), "fixed");
        assert_eq!(super::drive_type_label(4), "remote");
    }

    #[test]
    fn ratio_decimal_should_format_fixed_precision() {
        assert_eq!(ratio_decimal(1_046, 5_230), "0.200");
        assert_eq!(ratio_decimal(5_230, 5_230), "1.000");
        assert_eq!(ratio_decimal(5_230, 0), "n/a");
    }

    fn export_measurement(
        iteration: usize,
        scan_elapsed_ms: u128,
        export_elapsed_ms: u128,
        total_elapsed_ms: u128,
    ) -> ExportMeasurement {
        ExportMeasurement {
            iteration,
            scanner: "fallback",
            fallback: false,
            scan_elapsed_ms,
            export_elapsed_ms,
            total_elapsed_ms,
            export_bytes: 120,
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
        }
    }

    fn sample_environment() -> BenchmarkEnvironment {
        BenchmarkEnvironment {
            volume_root: "C:\\".to_owned(),
            file_system: "NTFS".to_owned(),
            drive_type: "fixed".to_owned(),
            shell_elevated: "false".to_owned(),
            windows_version: "10.0.0".to_owned(),
            logical_cpus: "8".to_owned(),
            physical_memory_bytes: "17179869184".to_owned(),
        }
    }

    fn sample_run_context() -> SuiteRunContext {
        SuiteRunContext {
            command_line: "diskloom-bench suite . target/bench-suite".to_owned(),
            git_revision: "abcdef123456".to_owned(),
            git_dirty: "false".to_owned(),
        }
    }

    fn sample_measurement_summary() -> MeasurementSummary {
        MeasurementSummary {
            runs: 3,
            scanners: "fallback".to_owned(),
            fallback_runs: 0,
            entries_min: 3,
            entries_max: 3,
            elapsed_ms_min: 900,
            elapsed_ms_median: 1_000,
            elapsed_ms_max: 1_100,
            first_result_ms_min: 10,
            first_result_ms_median: 20,
            first_result_ms_max: 30,
            peak_working_set_bytes_max: 120,
            peak_private_bytes_max: 95,
            peak_private_bytes_per_million_entries_max: 31_666_666,
        }
    }

    fn sample_same_machine_comparison() -> super::SameMachineComparison {
        super::SameMachineComparison {
            tool: "WizTree".to_owned(),
            version: "4.25".to_owned(),
            dataset_label: "repo-smoke".to_owned(),
            cache_state: "warm".to_owned(),
            scanner_scope: "ntfs_mft".to_owned(),
            context_match: "matched",
            scanner_scope_match: "aligned_ntfs_mft",
            competitor_runs: 2,
            diskloom_runs: 3,
            competitor_elapsed_ms_min: 500,
            competitor_elapsed_ms_median: 600,
            competitor_elapsed_ms_max: 700,
            diskloom_elapsed_ms_min: 900,
            diskloom_elapsed_ms_median: 1_000,
            diskloom_elapsed_ms_max: 1_100,
            diskloom_vs_competitor_median_ratio: "1.666".to_owned(),
            competitor_peak_private_bytes_max: Some(50),
            diskloom_peak_private_bytes_max: 95,
            diskloom_private_bytes_delta: Some(45),
            validity: "same_machine_user_supplied",
        }
    }
}
