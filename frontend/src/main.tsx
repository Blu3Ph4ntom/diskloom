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
};

type TreeViewportDto = {
  offset: number;
  total: number;
  rows: TreeRowDto[];
};

const ROW_HEIGHT = 34;
const OVERSCAN_ROWS = 30;
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
  const [drives, setDrives] = useState<DriveDto[]>([]);
  const [scanning, setScanning] = useState(false);
  const [status, setStatus] = useState("Ready");
  const [detail, setDetail] = useState("Type a drive or folder path.");
  const [progress, setProgress] = useState<ScanProgressDto>(EMPTY_PROGRESS);
  const [complete, setComplete] = useState<ScanCompleteDto | null>(null);
  const [rows, setRows] = useState<TreeRowDto[]>([]);
  const [rowOffset, setRowOffset] = useState(0);
  const [visibleTotal, setVisibleTotal] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(480);
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
    setPath(preferredDrive(drives));
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
  const activeDrive = normalizeScanTarget(path).toLowerCase();

  return (
    <main className="app-shell">
      <section className="topbar">
        <div className="brand-block">
          <div className="brand-mark">DL</div>
          <div>
            <div className="brand">DiskLoom</div>
            <div className="tagline">See your disk clearly.</div>
          </div>
          <div className="state-dot" data-active={scanning} />
        </div>

        <label className="path-field">
          <span>Drive or folder</span>
          <input
            autoFocus
            spellcheck={false}
            value={path}
            placeholder={"C:\\"}
            onInput={(event) => setPath(event.currentTarget.value)}
          />
        </label>

        <label className="search-field">
          <span>Search</span>
          <input
            spellcheck={false}
            value={query}
            placeholder="Name or path"
            onInput={(event) => setQuery(event.currentTarget.value)}
          />
        </label>

        <div className="top-actions">
          {scanning ? (
            <button className="action-button" onClick={cancelScan} type="button">
              Stop
            </button>
          ) : null}
          <button
            className="danger-button"
            disabled={selectedId === null || scanning}
            onClick={deleteSelected}
            type="button"
          >
            Recycle
          </button>
        </div>
      </section>

      <section className="drive-strip" aria-label="Drives">
        {drives.map((drive) => {
          const drivePath = normalizeScanTarget(drive.path).toLowerCase();
          return (
            <button
              key={drive.path}
              className="drive-chip"
              data-active={drivePath === activeDrive}
              onClick={() => setPath(drive.path)}
              type="button"
            >
              {drive.label}
            </button>
          );
        })}
      </section>

      <section className="metric-strip">
        <Metric label="Status" value={status} strong />
        <Metric label="Entries" value={formatCount(totals.entries)} />
        <Metric label="Files" value={formatCount(totals.files)} />
        <Metric label="Folders" value={formatCount(totals.directories)} />
        <Metric label="Size" value={complete ? formatBytes(complete.sizeBytes) : "-"} />
        <Metric label="Allocated" value={complete ? formatBytes(complete.allocatedBytes) : "-"} />
        <Metric label="Elapsed" value={`${formatCount(totals.elapsedMs)} ms`} />
      </section>

      <section className="tree-surface">
        <header className="tree-titlebar">
          <div>
            <h1>Files and folders</h1>
            <p className={error ? "error-text" : ""}>{error || selectedPath || detail}</p>
          </div>
          <div className="visible-count">{formatCount(visibleTotal)} visible</div>
        </header>

        <div className="table-header">
          <div>Name</div>
          <div>Size</div>
          <div>Allocated</div>
          <div>Items</div>
        </div>

        <div className="tree-viewport" ref={treeRef} onScroll={handleScroll}>
          <div style={{ height: topSpacer }} />
          {rows.map((row) => (
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
                <span className="name-text">{row.name}</span>
              </span>
              <span>{formatBytes(row.sizeBytes)}</span>
              <span>{formatBytes(row.allocatedBytes)}</span>
              <span>{row.childCount > 0 ? formatCount(row.childCount) : ""}</span>
            </button>
          ))}
          {rows.length === 0 ? (
            <div className="empty-state">
              {scanning
                ? `${formatCount(progress.entries)} entries scanned`
                : error
                  ? "Scan did not finish."
                  : "No files visible."}
            </div>
          ) : null}
          <div style={{ height: bottomSpacer }} />
        </div>
      </section>
    </main>
  );
}

function Metric(props: { label: string; value: string; strong?: boolean }) {
  return (
    <div className="metric" data-strong={props.strong}>
      <span>{props.label}</span>
      <strong>{props.value}</strong>
    </div>
  );
}

function preferredDrive(drives: DriveDto[]) {
  return drives.find((drive) => drive.path.toLowerCase() === "c:\\")?.path ?? drives[0]?.path ?? "";
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

render(<App />, document.getElementById("app")!);
