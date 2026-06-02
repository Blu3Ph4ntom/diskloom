# Elevated Scan Broker Design

## Goal

DiskLoom should use the direct NTFS MFT scanner by default for NTFS drive-backed paths while avoiding a UAC prompt every time the installed app or CLI is opened.

Windows does not let an installer permanently grant administrator rights to a normal future process. The installer can either make the app require elevation on every launch, install a resident service, or register a privileged on-demand component. DiskLoom uses the on-demand component because v1 has a hard no-background-service requirement.

## Architecture

The native Windows installer registers an on-demand scheduled task named `DiskLoomElevatedScan`. The task runs the installed `diskloom.exe` with a hidden worker argument and highest available privileges. It has no trigger that runs in the background; DiskLoom starts it only for a direct NTFS scan when the foreground process is not elevated.

The GUI and CLI write a constrained scan request into the current user's DiskLoom app data directory, run the scheduled task, wait for a compact graph snapshot, then load that snapshot into the normal in-process query and UI paths. If the task is missing, portable and development builds can still fall back to the existing one-shot UAC path or the fallback traversal scanner.

## Data Flow

1. Foreground process detects an NTFS drive-backed target.
2. If already elevated, it scans the raw volume directly.
3. If not elevated, it writes a request containing the path and broker-owned output paths.
4. It starts `DiskLoomElevatedScan`.
5. The elevated worker reads the request, validates output paths, scans the raw volume, writes a compact graph snapshot, and exits.
6. The foreground process imports the snapshot and applies normal sorting, filtering, shell actions, and exports.

## Security

The broker accepts structured scan requests only. It does not accept arbitrary command lines or shell fragments. Output files must stay under DiskLoom's broker directory. The worker exits after a single request.

## Error Handling

If the scheduled task is absent, disabled, or fails, DiskLoom reports the broker failure and uses the next allowed path for that mode. Explicit fallback mode never invokes the broker. Explicit NTFS mode surfaces the error if neither direct elevation nor broker execution works.

## Testing

Tests cover request path validation, graph snapshot import/export, CLI scanner selection, and installer task registration command construction. Manual Windows verification covers installer creation, task registration, non-elevated CLI direct scans, GUI direct scans, and uninstall task cleanup.
