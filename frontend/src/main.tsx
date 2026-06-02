import { render } from "preact";
import type { JSX } from "preact";
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./styles.css";

type ScannerMode = "auto" | "ntfs" | "fallback";

type StartupDto = {
  path: string | null;
  scanner: ScannerMode;
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
const OVERSCAN_ROWS = 28;

function App() {
  const [path, setPath] = useState(".");
  const [scanner, setScanner] = useState<ScannerMode>("auto");
  const [drives, setDrives] = useState<DriveDto[]>([]);
  const [scanning, setScanning] = useState(false);
  const [status, setStatus] = useState("Ready");
  const [detail, setDetail] = useState("");
  const [progress, setProgress] = useState<ScanProgressDto>({
    entries: 0,
    files: 0,
    directories: 0,
    inaccessible: 0,
    elapsedMs: 0,
  });
  const [complete, setComplete] = useState<ScanCompleteDto | null>(null);
  const [rows, setRows] = useState<TreeRowDto[]>([]);
  const [rowOffset, setRowOffset] = useState(0);
  const [visibleTotal, setVisibleTotal] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(460);
  const [selectedPath, setSelectedPath] = useState("");
  const [error, setError] = useState("");
  const treeRef = useRef<HTMLDivElement>(null);
  const autoScanStarted = useRef(false);

  const rowLimit = useMemo(
    () => Math.max(80, Math.ceil(viewportHeight / ROW_HEIGHT) + OVERSCAN_ROWS * 2),
    [viewportHeight],
  );

  const loadRows = useCallback(
    async (offset: number) => {
      const viewport = await invoke<TreeViewportDto>("get_visible_rows", {
        offset: Math.max(0, offset),
        limit: rowLimit,
      });
      setRows(viewport.rows);
      setRowOffset(viewport.offset);
      setVisibleTotal(viewport.total);
    },
    [rowLimit],
  );

  const startScan = useCallback(async () => {
    setError("");
    setComplete(null);
    setRows([]);
    setVisibleTotal(0);
    setSelectedPath("");
    setStatus("Scanning");
    setDetail(path);
    setScanning(true);
    setProgress({
      entries: 0,
      files: 0,
      directories: 0,
      inaccessible: 0,
      elapsedMs: 0,
    });
    try {
      await invoke("start_scan", { path, scanner });
    } catch (scanError) {
      setScanning(false);
      setStatus("Scan failed");
      setError(String(scanError));
    }
  }, [path, scanner]);

  const cancelScan = useCallback(async () => {
    await invoke("cancel_scan");
    setStatus("Cancelling");
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
      });
      setRows(viewport.rows);
      setRowOffset(viewport.offset);
      setVisibleTotal(viewport.total);
    },
    [rowLimit, rowOffset],
  );

  const selectRow = useCallback(async (row: TreeRowDto) => {
    const selected = await invoke<string | null>("select_entry", { id: row.id });
    setSelectedPath(selected ?? "");
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
    let cleanup = false;
    void invoke<DriveDto[]>("discover_drives").then((items) => {
      if (!cleanup) {
        setDrives(items);
      }
    });
    void invoke<StartupDto>("get_startup").then((startup) => {
      if (cleanup) {
        return;
      }
      if (startup.path) {
        setPath(startup.path);
      }
      setScanner(startup.scanner);
      if (startup.scan && !autoScanStarted.current) {
        autoScanStarted.current = true;
        window.setTimeout(() => void startScan(), 120);
      }
    });
    return () => {
      cleanup = true;
    };
  }, [startScan]);

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
    const unlisten = [
      listen<string>("scan-started", (event) => {
        setScanning(true);
        setStatus("Scanning");
        setDetail(event.payload);
      }),
      listen<ScanProgressDto>("scan-progress", (event) => {
        setProgress(event.payload);
      }),
      listen<ScanCompleteDto>("scan-complete", async (event) => {
        setScanning(false);
        setStatus("Complete");
        setComplete(event.payload);
        setProgress(event.payload);
        setDetail(event.payload.fallbackReason ?? event.payload.scannerLabel);
        const element = treeRef.current;
        if (element) {
          element.scrollTop = 0;
        }
        await loadRows(0);
      }),
      listen<string>("scan-error", (event) => {
        setScanning(false);
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
  }, [loadRows]);

  const totals = complete ?? progress;
  const topSpacer = rowOffset * ROW_HEIGHT;
  const bottomSpacer = Math.max(0, visibleTotal - rowOffset - rows.length) * ROW_HEIGHT;

  return (
    <main className="app-shell">
      <section className="command-bar">
        <div className="brand-block">
          <div className="brand">DiskLoom</div>
          <div className="state-dot" data-active={scanning} />
        </div>

        <label className="path-field">
          <span>Path</span>
          <input value={path} onInput={(event) => setPath(event.currentTarget.value)} />
        </label>

        <div className="mode-group" role="radiogroup" aria-label="Scanner">
          {(["auto", "ntfs", "fallback"] as ScannerMode[]).map((mode) => (
            <button
              key={mode}
              className="mode-button"
              data-active={scanner === mode}
              disabled={scanning}
              onClick={() => setScanner(mode)}
              type="button"
            >
              {modeLabel(mode)}
            </button>
          ))}
        </div>

        <div className="scan-actions">
          <button className="primary-button" disabled={scanning} onClick={startScan} type="button">
            Scan
          </button>
          <button className="quiet-button" disabled={!scanning} onClick={cancelScan} type="button">
            Cancel
          </button>
        </div>
      </section>

      <section className="drive-strip" aria-label="Drives">
        {drives.map((drive) => (
          <button
            key={drive.path}
            className="drive-chip"
            data-active={drive.path.toLowerCase() === path.toLowerCase()}
            disabled={scanning}
            onClick={() => setPath(drive.path)}
            type="button"
          >
            {drive.label}
          </button>
        ))}
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
            <p>{error || selectedPath || detail}</p>
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
                  {row.expanded ? "v" : ">"}
                </span>
                <span className="name-text">{row.name}</span>
              </span>
              <span>{formatBytes(row.sizeBytes)}</span>
              <span>{formatBytes(row.allocatedBytes)}</span>
              <span>{row.childCount > 0 ? formatCount(row.childCount) : ""}</span>
            </button>
          ))}
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

function modeLabel(mode: ScannerMode) {
  if (mode === "ntfs") {
    return "NTFS";
  }
  return mode[0].toUpperCase() + mode.slice(1);
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
