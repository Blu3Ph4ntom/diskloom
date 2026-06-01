use std::{
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
    time::Instant,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use diskloom_scan::{FallbackScanner, ScanOptions};

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

#[derive(Debug, Clone, Copy)]
struct ScanMeasurement {
    iteration: usize,
    elapsed_ms: u128,
    entries: u64,
    files: u64,
    directories: u64,
    inaccessible: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Scan { path, iterations } => run_scan(path, iterations),
        Command::Dataset {
            root,
            dirs,
            files_per_dir,
            bytes_per_file,
        } => create_dataset(root, dirs, files_per_dir, bytes_per_file),
    }
}

fn run_scan(path: PathBuf, iterations: usize) -> Result<()> {
    let mut measurements = Vec::with_capacity(iterations);

    for iteration in 1..=iterations {
        let started = Instant::now();
        let (_, summary) = FallbackScanner::scan(ScanOptions {
            root: path.clone(),
            follow_symlinks: false,
        })
        .with_context(|| format!("scan failed for {}", path.display()))?;
        measurements.push(ScanMeasurement {
            iteration,
            elapsed_ms: started.elapsed().as_millis(),
            entries: summary.entries,
            files: summary.files,
            directories: summary.directories,
            inaccessible: summary.inaccessible,
        });
    }

    write_measurements(&mut io::stdout().lock(), &measurements)?;
    Ok(())
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
        "iteration,elapsed_ms,entries,files,directories,inaccessible"
    )?;
    for measurement in measurements {
        writeln!(
            writer,
            "{},{},{},{},{},{}",
            measurement.iteration,
            measurement.elapsed_ms,
            measurement.entries,
            measurement.files,
            measurement.directories,
            measurement.inaccessible
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ScanMeasurement, write_measurements};

    #[test]
    fn write_measurements_should_emit_csv_rows() {
        let measurements = [ScanMeasurement {
            iteration: 1,
            elapsed_ms: 10,
            entries: 3,
            files: 2,
            directories: 1,
            inaccessible: 0,
        }];
        let mut output = Vec::new();

        write_measurements(&mut output, &measurements).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("1,10,3,2,1,0"));
    }
}
