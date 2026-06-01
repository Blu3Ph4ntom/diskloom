# WizTree Public Claims Baseline

This file tracks source-backed WizTree claims that DiskLoom should challenge with reproducible measurements. Public claims are useful benchmark targets, but they are not proof that DiskLoom is faster or slower on current hardware.

Accessed: 2026-06-01.

## Sources

- [WizTree home page](https://diskanalyzer.com/) positions WizTree as "The FASTEST Disk Space Analyzer", links to a 46x WinDirStat comparison, says similar applications can take minutes where WizTree takes seconds, and describes direct NTFS MFT reading.
- [WizTree vs WinDirStat](https://diskanalyzer.com/wiztree-vs-windirstat) publishes two historical cold-start comparisons: 4.34 seconds vs 3 min 20 sec on a 25 GB HDD, and 5.23 seconds vs 1 min 55 sec on a 460 GB SSD.
- [WizTree about page](https://diskanalyzer.com/about) explains that its NTFS fast path reads the MFT directly and can also scan non-NTFS drives, network drives, USB drives, and individual directories.
- [WizTree FAQ](https://diskanalyzer.com/faq) states that high-speed NTFS scanning needs admin rights and explains allocated-size behavior.
- [WizTree download page](https://diskanalyzer.com/download) lists the current Windows release, portable build, CSV export/import, duplicate locating, and recent scan-progress improvements.

## Claim Matrix

| Public claim or capability | DiskLoom benchmark implication | Current DiskLoom status |
| --- | --- | --- |
| Full NTFS drive scans complete in seconds. | Run `diskloom-bench scan C:\ --scanner ntfs --iterations 5 --sample-ms 10` from an elevated shell and publish raw CSV plus median/range. | Harness supports this; direct NTFS scanner is still early and needs correctness hardening before public claims. |
| WizTree publishes 46x and 22x faster results versus WinDirStat on older systems. | Treat those as historical reference points, not current proof. DiskLoom must publish same-machine comparisons against current WizTree, TreeSize, WinDirStat, and Explorer traversal. | Methodology exists; competitor automation is not implemented yet. |
| Direct MFT reading is the core speed advantage on NTFS. | DiskLoom's `ntfs` scanner must be measured separately from `fallback` so failures and fallback behavior are visible. | `diskloom-bench` records `scanner` and `fallback` columns. |
| High-speed NTFS scanning requires admin rights. | Runs must record whether the shell was elevated, and `auto` mode must show if it fell back. | CLI, GUI, and benchmark auto modes report fallback behavior. |
| Non-NTFS drives, network drives, USB drives, and folders are supported through non-MFT scanning. | Benchmark fallback traversal separately on folders, network paths, and removable drives. | Non-admin fallback scanner exists and emits timing/memory data. |
| Hard links are handled without double-counting where possible. | Include synthetic hard-link datasets and compare allocated-size totals against Windows/WizTree. | Initial NTFS hard-link flagging exists; full correctness proof is still pending. |
| CSV export/import and duplicate location are product capabilities. | Benchmark CSV export time and duplicate grouping separately from scan time. | CSV export and first-pass duplicate candidates exist; import and content hashing are not done. |

## DiskLoom Commands

Run all benchmark commands from a release build. Use the same shell elevation state for every tool in a comparison set.

```powershell
cargo build --release -p diskloom-bench -p diskloom-cli
target\release\diskloom-bench.exe scan C:\ --scanner ntfs --iterations 5 --sample-ms 10
target\release\diskloom-bench.exe scan C:\ --scanner auto --iterations 5 --sample-ms 10
target\release\diskloom-bench.exe scan C:\ --scanner fallback --iterations 5 --sample-ms 10
```

Capture and summarize a run before comparing it with the claim matrix:

```powershell
target\release\diskloom-bench.exe scan C:\ --scanner ntfs --iterations 5 --sample-ms 10 > target\bench-ntfs.csv
target\release\diskloom-bench.exe summarize target\bench-ntfs.csv
target\release\diskloom-bench.exe compare-public target\bench-ntfs.csv --claim wiztree-ssd-460gb
```

Use synthetic datasets for controlled fallback measurements:

```powershell
target\release\diskloom-bench.exe dataset D:\diskloom-bench-1m --dirs 1000 --files-per-dir 1000 --bytes-per-file 0
target\release\diskloom-bench.exe scan D:\diskloom-bench-1m --scanner fallback --iterations 5 --sample-ms 10
```

## Reporting Rules

- Do not claim DiskLoom is faster than WizTree until same-machine results prove it.
- Publish the raw DiskLoom CSV rows, the exact commands, dataset shape, shell elevation, filesystem, storage type, cold/warm cache state, and tool versions.
- Compare DiskLoom's NTFS fast path against WizTree's NTFS scan path, and DiskLoom fallback against WizTree non-admin/folder traversal.
- Separate scan time, time to first visible result, CSV export time, peak working set, private bytes, and UI responsiveness.
- Mark public WizTree claims as vendor-published claims unless independently reproduced.
- Treat `compare-public` output as a source-labeled reference row only. It is useful for tracking progress against public claims, but it is not a same-machine competitor benchmark.
