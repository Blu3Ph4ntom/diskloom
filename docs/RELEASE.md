# Release Notes

## DiskLoom 0.1.0

First public Windows preview.

### Included

- `diskloom.exe`: Windows GUI disk analyzer.
- `dlm.exe`: command-line analyzer.
- `DiskLoomSetup-x64.exe`: native Windows installer that installs both binaries and adds DiskLoom to PATH.

### Notes

- Direct NTFS MFT scanning can require administrator access.
- Fallback traversal works without elevation.
- DiskLoom is built to challenge WizTree, but benchmark wins are not claimed until reproducible public results prove them.

### Install

Download `DiskLoomSetup-x64.exe` from the latest release and run the installer.

For terminal use after installation:

```powershell
dlm
dlm C:\ --scanner auto --limit 25
dlm C:\Users --csv users.csv
```
