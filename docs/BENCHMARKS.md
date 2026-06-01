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

