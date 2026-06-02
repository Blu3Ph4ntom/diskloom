import { render } from "preact";
import type { JSX } from "preact";
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./styles.css";

type StartupDto = {
  path: string | null;
  scanner: "auto" | "ntfs" | "fallback";
  scan: boolean;
  elevated: boolean;
};

type DriveDto = {
  path: string;
  label: string;
  isNtfs: boolean;
  totalBytes: number | null;
  freeBytes: number | null;
};

type ScanProgressDto = {
  entries: number;
  files: number;
  directories: number;
  inaccessible: number;
  elapsedMs: number;
};

type ScanCompleteDto = ScanProgressDto & {
  scannerLabel: string;
  fallbackReason: string | null;
  sizeBytes: number;
  allocatedBytes: number;
};

type TreeRowDto = {
  id: number;
  name: string;
  depth: number;
  isDir: boolean;
  expanded: boolean;
  childCount: number;
  sizeBytes: number;
  allocatedBytes: number;
  modifiedUnix: number;
};

type TreeViewportDto = {
  offset: number;
  total: number;
  rows: TreeRowDto[];
};

const ROW_HEIGHT = 38;
const OVERSCAN_ROWS = 28;
const EMPTY_PROGRESS: ScanProgressDto = {
  entries: 0,
  files: 0,
  directories: 0,
  inaccessible: 0,
  elapsedMs: 0,
};

function App() {
  const [ready, setReady] = useState(false);
  const [path, setPath] = useState("");
  const [query, setQuery] = useState("");
  const [commandText, setCommandText] = useState("");
  const [drives, setDrives] = useState<DriveDto[]>([]);
  const [scanning, setScanning] = useState(false);
  const [status, setStatus] = useState("Ready");
  const [detail, setDetail] = useState("Type a drive, folder, or search.");
  const [progress, setProgress] = useState<ScanProgressDto>(EMPTY_PROGRESS);
  const [complete, setComplete] = useState<ScanCompleteDto | null>(null);
  const [rows, setRows] = useState<TreeRowDto[]>([]);
  const [rowOffset, setRowOffset] = useState(0);
  const [visibleTotal, setVisibleTotal] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(520);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [selectedPath, setSelectedPath] = useState("");
  const [error, setError] = useState("");

  const treeRef = useRef<HTMLDivElement>(null);
  const scanningRef = useRef(false);
  const queuedScanPath = useRef("");
  const lastStartedPath = useRef("");

  const rowLimit = useMemo(
    () => Math.max(90, Math.ceil(viewportHeight / ROW_HEIGHT) + OVERSCAN_ROWS * 2),
    [viewportHeight],
  );

  const activeDrive = normalizeScanTarget(path).toLowerCase();
  const activeDriveInfo = useMemo(
    () => drives.find((drive) => normalizeScanTarget(drive.path).toLowerCase() === activeDrive),
    [activeDrive, drives],
  );
  const largestVisibleRow = useMemo(
    () => rows.reduce((largest, row) => Math.max(largest, row.sizeBytes), 0),
    [rows],
  );
  const rowBarBase = Math.max(largestVisibleRow, complete?.sizeBytes ?? 0, 1);

  const loadRows = useCallback(
    async (offset: number) => {
      const viewport = await invoke<TreeViewportDto>("get_visible_rows", {
        offset: Math.max(0, offset),
        limit: rowLimit,
        query,
      });
      setRows(viewport.rows);
      setRowOffset(viewport.offset);
      setVisibleTotal(viewport.total);
    },
    [query, rowLimit],
  );

  const startScanFor = useCallback(async (rawPath: string) => {
    const target = normalizeScanTarget(rawPath);
    if (!target) {
      return;
    }

    if (scanningRef.current) {
      queuedScanPath.current = target;
      setStatus("Cancelling");
      try {
        await invoke("cancel_scan");
      } catch {
        // Best effort. The running scan will either stop or finish.
      }
      return;
    }

    lastStartedPath.current = target;
    queuedScanPath.current = "";
    setError("");
    setComplete(null);
    setRows([]);
    setVisibleTotal(0);
    setSelectedId(null);
    setSelectedPath("");
    setStatus("Scanning");
    setDetail(target);
    setScanning(true);
    scanningRef.current = true;
    setProgress(EMPTY_PROGRESS);

    try {
      await invoke("start_scan", { path: target, scanner: "auto" });
    } catch (scanError) {
      const message = String(scanError);
      if (message.toLowerCase().includes("already running")) {
        queuedScanPath.current = target;
        setStatus("Cancelling");
        try {
          await invoke("cancel_scan");
        } catch {
          // Best effort. The running scan will either stop or finish.
        }
        return;
      }
      scanningRef.current = false;
      setScanning(false);
      setStatus("Scan failed");
      setError(message);
    }
  }, []);

  const cancelScan = useCallback(async () => {
    queuedScanPath.current = "";
    setStatus("Cancelling");
    await invoke("cancel_scan");
  }, []);

  const toggleRow = useCallback(
    async (row: TreeRowDto) => {
      if (row.childCount === 0) {
        return;
      }
      const viewport = await invoke<TreeViewportDto>("toggle_entry", {
        id: row.id,
        offset: rowOffset,
        limit: rowLimit,
        query,
      });
      setRows(viewport.rows);
      setRowOffset(viewport.offset);
      setVisibleTotal(viewport.total);
    },
    [query, rowLimit, rowOffset],
  );

  const selectRow = useCallback(async (row: TreeRowDto) => {
    const selected = await invoke<string | null>("select_entry", { id: row.id });
    setSelectedId(row.id);
    setSelectedPath(selected ?? "");
  }, []);

  const deleteSelected = useCallback(async () => {
    if (selectedId === null || !selectedPath) {
      return;
    }
    const confirmed = window.confirm(`Move to Recycle Bin?\n\n${selectedPath}`);
    if (!confirmed) {
      return;
    }
    setError("");
    try {
      await invoke("delete_entry", { id: selectedId });
      setSelectedId(null);
      setSelectedPath("");
      await startScanFor(path);
    } catch (deleteError) {
      setStatus("Delete failed");
      setError(String(deleteError));
    }
  }, [path, selectedId, selectedPath, startScanFor]);

  const handleCommandInput = useCallback((value: string) => {
    setCommandText(value);
    if (looksLikePath(value)) {
      setQuery("");
      setPath(normalizeScanTarget(value));
      return;
    }
    setQuery(value);
  }, []);

  const chooseDrive = useCallback((drive: DriveDto) => {
    setQuery("");
    setPath(drive.path);
    setCommandText(drive.path);
  }, []);

  const handleScroll = useCallback(() => {
    const element = treeRef.current;
    if (!element) {
      return;
    }
    const nextOffset = Math.max(0, Math.floor(element.scrollTop / ROW_HEIGHT) - OVERSCAN_ROWS);
    if (Math.abs(nextOffset - rowOffset) > 12) {
      void loadRows(nextOffset);
    }
  }, [loadRows, rowOffset]);

  useEffect(() => {
    scanningRef.current = scanning;
  }, [scanning]);

  useEffect(() => {
    let cleanup = false;
    void invoke<StartupDto>("get_startup").then((startup) => {
      if (cleanup) {
        return;
      }
      if (startup.path) {
        setPath(startup.path);
        setCommandText(startup.path);
      }
      setReady(true);
    });
    void invoke<DriveDto[]>("discover_drives").then((items) => {
      if (!cleanup) {
        setDrives(items);
      }
    });
    return () => {
      cleanup = true;
    };
  }, []);

  useEffect(() => {
    if (!ready || path.trim() || drives.length === 0) {
      return;
    }
    const nextPath = preferredDrive(drives);
    setPath(nextPath);
    setCommandText(nextPath);
  }, [drives, path, ready]);

  useEffect(() => {
    if (!ready) {
      return;
    }
    const target = normalizeScanTarget(path);
    if (!target) {
      setStatus("Ready");
      return;
    }
    const delay = isDriveRoot(target) ? 180 : 650;
    const timer = window.setTimeout(() => {
      if (
        !scanningRef.current &&
        complete &&
        lastStartedPath.current.toLowerCase() === target.toLowerCase()
      ) {
        return;
      }
      void startScanFor(target);
    }, delay);
    return () => window.clearTimeout(timer);
  }, [complete, path, ready, startScanFor]);

  useEffect(() => {
    const element = treeRef.current;
    if (!element) {
      return;
    }
    const observer = new ResizeObserver(([entry]) => {
      setViewportHeight(entry.contentRect.height);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!complete) {
      return;
    }
    const element = treeRef.current;
    if (element) {
      element.scrollTop = 0;
    }
    void loadRows(0);
  }, [complete, loadRows]);

  useEffect(() => {
    const runQueuedScan = () => {
      const queued = queuedScanPath.current;
      queuedScanPath.current = "";
      if (!queued) {
        return false;
      }
      window.setTimeout(() => void startScanFor(queued), 80);
      return true;
    };

    const unlisten = [
      listen<string>("scan-started", (event) => {
        setScanning(true);
        scanningRef.current = true;
        setStatus("Scanning");
        setDetail(event.payload);
      }),
      listen<ScanProgressDto>("scan-progress", (event) => {
        setProgress(event.payload);
      }),
      listen<ScanCompleteDto>("scan-complete", (event) => {
        setScanning(false);
        scanningRef.current = false;
        setStatus("Complete");
        setComplete(event.payload);
        setProgress(event.payload);
        setDetail(event.payload.fallbackReason ?? event.payload.scannerLabel);
        void loadRows(0);
        runQueuedScan();
      }),
      listen<string>("scan-error", (event) => {
        setScanning(false);
        scanningRef.current = false;
        if (runQueuedScan()) {
          setError("");
          return;
        }
        setStatus("Scan failed");
        setError(event.payload);
      }),
    ];

    return () => {
      void Promise.all(unlisten).then((listeners) => {
        for (const unlisten of listeners) {
          unlisten();
        }
      });
    };
  }, [loadRows, startScanFor]);

  const totals = complete ?? progress;
  const topSpacer = rowOffset * ROW_HEIGHT;
  const bottomSpacer = Math.max(0, visibleTotal - rowOffset - rows.length) * ROW_HEIGHT;
  const subline = error || selectedPath || detail;
  const activeUsedPercent = activeDriveInfo ? driveUsedPercent(activeDriveInfo) : null;

  return (
    <main className="app-shell" data-scanning={scanning}>
      <aside className="drive-rail">
        <div className="rail-title">
          <button className="icon-button" type="button" aria-label="Menu">
            =
          </button>
          <div className="product-lockup">
            <span className="app-icon">DL</span>
            <span>DiskLoom</span>
          </div>
        </div>

        <div className="drive-list">
          {drives.map((drive) => {
            const drivePath = normalizeScanTarget(drive.path).toLowerCase();
            const usedPercent = driveUsedPercent(drive);
            return (
              <button
                key={drive.path}
                className="drive-card"
                data-active={drivePath === activeDrive}
                onClick={() => chooseDrive(drive)}
                type="button"
              >
                <span className="drive-icon" />
                <span className="drive-copy">
                  <span className="drive-name">{driveTitle(drive)}</span>
                  <span className="rail-meter">
                    <span style={{ width: `${usedPercent ?? 0}%` }} />
                  </span>
                  <span className="drive-meta">{driveCapacityLine(drive)}</span>
                </span>
              </button>
            );
          })}
        </div>
      </aside>

      <section className="main-surface">
        <header className="command-bar">
          <label className="command-field">
            <span className="search-glyph" />
            <input
              autoFocus
              spellcheck={false}
              value={commandText}
              placeholder="Search or enter path..."
              onInput={(event) => handleCommandInput(event.currentTarget.value)}
            />
          </label>
          <div className="command-actions">
            <button className="icon-button" type="button" aria-label="Open folder">
              []
            </button>
            <button className="icon-button" type="button" aria-label="Info">
              i
            </button>
            {scanning ? (
              <button className="text-action" onClick={cancelScan} type="button">
                Stop
              </button>
            ) : (
              <button
                className="text-action danger"
                disabled={selectedId === null}
                onClick={deleteSelected}
                type="button"
              >
                Recycle
              </button>
            )}
          </div>
        </header>

        <section className="scan-readout">
          <div>
            <div className="readout-status">
              <span className="live-dot" data-active={scanning} />
              <strong>{status}</strong>
              <span>{subline}</span>
            </div>
            <div className="readout-counts">
              <span>{formatCount(totals.entries)} entries</span>
              <span>{formatCount(totals.files)} files</span>
              <span>{formatCount(totals.directories)} folders</span>
              <span>{formatCount(totals.elapsedMs)} ms</span>
            </div>
          </div>
          <div className="readout-capacity">
            <span>{complete ? formatBytes(complete.sizeBytes) : "Scanning"}</span>
            <div className="capacity-track" data-live={scanning}>
              <span style={{ width: `${activeUsedPercent ?? 0}%` }} />
            </div>
            <span>{activeUsedPercent === null ? "" : `${Math.round(activeUsedPercent)}% full`}</span>
          </div>
        </section>

        <section className="table-shell">
          <div className="table-header">
            <button type="button">
              Name
              <span>^</span>
            </button>
            <button type="button">
              Size
              <span>v</span>
            </button>
            <button type="button">
              Allocated
              <span>v</span>
            </button>
            <button type="button">
              Last Modified
              <span>v</span>
            </button>
          </div>

          <div className="tree-viewport" ref={treeRef} onScroll={handleScroll}>
            <div style={{ height: topSpacer }} />
            {rows.map((row) => {
              const share = Math.max(2, Math.min(100, (row.sizeBytes / rowBarBase) * 100));
              return (
                <button
                  key={row.id}
                  className="tree-row"
                  data-selected={selectedId === row.id}
                  style={{ "--depth": row.depth } as JSX.CSSProperties}
                  onClick={() => void selectRow(row)}
                  onDblClick={() => void toggleRow(row)}
                  type="button"
                >
                  <span className="row-name">
                    <span
                      className="disclosure"
                      data-visible={row.childCount > 0}
                      onClick={(event) => {
                        event.stopPropagation();
                        void toggleRow(row);
                      }}
                    >
                      {row.childCount === 0 ? "" : row.expanded ? "v" : ">"}
                    </span>
                    <span className="item-icon" data-kind={row.isDir ? "dir" : "file"} />
                    <span className="name-text">{row.name}</span>
                  </span>
                  <span className="size-cell">
                    <span>{formatBytes(row.sizeBytes)}</span>
                    <span className="row-meter">
                      <span style={{ width: `${share}%` }} />
                    </span>
                  </span>
                  <span className="numeric-cell">{formatBytes(row.allocatedBytes)}</span>
                  <span className="date-cell">{formatModified(row.modifiedUnix)}</span>
                </button>
              );
            })}
            {rows.length === 0 ? (
              scanning ? (
                <StreamingRows progress={progress} />
              ) : (
                <div className="empty-state">{error ? "Scan did not finish." : "No files visible."}</div>
              )
            ) : null}
            <div style={{ height: bottomSpacer }} />
          </div>
        </section>
      </section>
    </main>
  );
}

function StreamingRows(props: { progress: ScanProgressDto }) {
  const rows = Array.from({ length: 14 }, (_, index) => index);
  return (
    <div className="streaming-state">
      <div className="stream-message">
        <strong>{formatCount(props.progress.entries)} entries scanned</strong>
        <span>Results are building in the scanner thread.</span>
      </div>
      {rows.map((row) => (
        <div key={row} className="stream-row" style={{ "--w": `${44 + ((row * 13) % 42)}%` } as JSX.CSSProperties}>
          <span />
          <i />
          <b />
          <em />
        </div>
      ))}
    </div>
  );
}

function preferredDrive(drives: DriveDto[]) {
  return drives.find((drive) => drive.path.toLowerCase() === "c:\\")?.path ?? drives[0]?.path ?? "";
}

function looksLikePath(value: string) {
  const trimmed = value.trim();
  return /^[a-zA-Z]:/.test(trimmed) || trimmed.startsWith("\\\\") || trimmed.includes(":\\");
}

function normalizeScanTarget(value: string) {
  const trimmed = value.trim();
  if (/^[a-zA-Z]:$/.test(trimmed)) {
    return `${trimmed}\\`;
  }
  return trimmed;
}

function isDriveRoot(value: string) {
  return /^[a-zA-Z]:[\\/]*$/.test(value.trim());
}

function driveTitle(drive: DriveDto) {
  const driveRoot = drive.path.replace("\\", "");
  return `${driveRoot} ${drive.isNtfs ? "Local Disk" : drive.label.replace(driveRoot, "").trim() || "Drive"}`;
}

function driveUsedPercent(drive: DriveDto) {
  if (!drive.totalBytes || drive.freeBytes === null) {
    return null;
  }
  const used = Math.max(0, drive.totalBytes - drive.freeBytes);
  return Math.min(100, Math.max(0, (used / drive.totalBytes) * 100));
}

function driveCapacityLine(drive: DriveDto) {
  const usedPercent = driveUsedPercent(drive);
  if (!drive.totalBytes || usedPercent === null) {
    return drive.label;
  }
  return `${Math.round(usedPercent)}% full of ${formatBytes(drive.totalBytes)}`;
}

function formatBytes(bytes: number) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit + 1 < units.length) {
    value /= 1024;
    unit += 1;
  }
  if (unit === 0) {
    return `${bytes} B`;
  }
  if (value >= 100) {
    return `${value.toFixed(0)} ${units[unit]}`;
  }
  if (value >= 10) {
    return `${value.toFixed(1)} ${units[unit]}`;
  }
  return `${value.toFixed(2)} ${units[unit]}`;
}

function formatCount(value: number) {
  return Math.trunc(value).toLocaleString("en-US");
}

function formatModified(unix: number) {
  if (!unix) {
    return "-";
  }
  return new Date(unix * 1000).toLocaleString(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

render(<App />, document.getElementById("app")!);
