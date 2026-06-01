use std::{
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ScannerMode {
    Auto,
    Fallback,
    Ntfs,
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

#[cfg(test)]
mod tests {
    use super::{ScanMeasurement, per_million, write_measurements};

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
}
