# Benchmark Methodology

DiskLoom benchmark claims must be reproducible and conservative.

## Metrics

- Time to first visible result.
- Full scan time.
- Peak working set.
- Private bytes.
- UI responsiveness during scan.
- CSV export time.
- Memory use per million files.
- Behavior on 1M, 5M, and 10M file datasets.

## Comparisons

Compare against:

- WizTree.
- TreeSize.
- WinDirStat.
- Windows Explorer search or traversal behavior where relevant.

## Rules

- Run release builds.
- Record hardware, Windows version, filesystem, drive type, and dataset shape.
- Repeat runs and report median and range.
- Separate cold-cache and warm-cache results.
- Avoid claims that are not backed by published data.
- Publish commands, dataset generator settings, raw output, and analysis scripts.

## Current Harness

Repeated fallback scan timing:

```powershell
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5
```

CLI scanner modes:

```powershell
cargo run --release -p diskloom-cli -- scan C:\ --scanner auto --limit 25
cargo run --release -p diskloom-cli -- scan C:\ --scanner ntfs --limit 25
cargo run --release -p diskloom-cli -- scan C:\ --scanner fallback --limit 25
```

Synthetic dataset generation:

```powershell
cargo run --release -p diskloom-bench -- dataset D:\diskloom-bench --dirs 1000 --files-per-dir 1000 --bytes-per-file 0
```

The harness currently emits CSV rows for elapsed time and entry counts. Peak working set, private bytes, UI responsiveness, direct scanner timing, and competitor automation still need dedicated collectors before public claims are made.
