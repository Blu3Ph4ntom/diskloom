import { render } from "preact";
import type { JSX } from "preact";
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  ChevronDown,
  ChevronRight,
  ChevronUp,
  File,
  FileText,
  Filter,
  Folder,
  FolderOpen,
  HardDrive,
  Info,
  Menu,
  RefreshCw,
  Search,
  Trash2,
} from "lucide-preact";
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
  windowsUsedBytes: number | null;
  unattributedBytes: number;
};

type DeleteEventDto = {
  path: string;
  permanently: boolean;
  elapsedMs: number;
};

type DeleteErrorDto = DeleteEventDto & {
  error: string;
};

type TooltipState = {
  text: string;
  x: number;
  y: number;
} | null;

type TreeRowDto = {
  id: number;
  name: string;
  path: string;
  depth: number;
  isDir: boolean;
  expanded: boolean;
  childCount: number;
  sizeBytes: number;
  allocatedBytes: number;
  modifiedUnix: number;
  synthetic?: boolean;
  detail?: string;
};

type TreeViewportDto = {
  offset: number;
  total: number;
  rows: TreeRowDto[];
};

type SortKey = "name" | "size" | "allocated" | "modified";
type DeleteMode = "recycle" | "permanent";

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
  const [deleting, setDeleting] = useState(false);
  const [deletePath, setDeletePath] = useState("");
  const [deleteElapsedMs, setDeleteElapsedMs] = useState(0);
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
  const [loadedPath, setLoadedPath] = useState("");
  const [sortKey, setSortKey] = useState<SortKey>("size");
  const [sortDescending, setSortDescending] = useState(true);
  const [railCollapsed, setRailCollapsed] = useState(false);
  const [infoOpen, setInfoOpen] = useState(false);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [tooltip, setTooltip] = useState<TooltipState>(null);

  const treeRef = useRef<HTMLDivElement>(null);
  const pathRef = useRef("");
  const tooltipElementRef = useRef<HTMLElement | null>(null);
  const scanningRef = useRef(false);
  const queuedScanPath = useRef("");
  const lastStartedPath = useRef("");
  const loadedPathRef = useRef("");
  const pendingDeleteRefreshPath = useRef("");
  const deleteStartedAt = useRef<number | null>(null);

  const rowLimit = useMemo(
    () => Math.max(90, Math.ceil(viewportHeight / ROW_HEIGHT) + OVERSCAN_ROWS * 2),
    [viewportHeight],
  );

  const activeDriveRoot = driveRootFromTarget(path);
  const activeDriveInfo = useMemo(
    () => drives.find((drive) => normalizeScanTarget(drive.path).toLowerCase() === activeDriveRoot),
    [activeDriveRoot, drives],
  );
  const largestVisibleRow = useMemo(
    () => rows.reduce((largest, row) => Math.max(largest, displayAllocatedBytes(row, activeDriveInfo)), 0),
    [activeDriveInfo, rows],
  );
  const accountingRow = useMemo(
    () => buildAccountingRow(complete, loadedPath || path),
    [complete, loadedPath, path],
  );
  const displayRows = useMemo(
    () => injectAccountingRow(rows, accountingRow, query),
    [accountingRow, query, rows],
  );
  const rowBarBase = Math.max(
    largestVisibleRow,
    complete?.allocatedBytes ?? 0,
    complete?.windowsUsedBytes ?? 0,
    complete?.unattributedBytes ?? 0,
    1,
  );

  const loadRows = useCallback(
    async (offset: number) => {
      const viewport = await invoke<TreeViewportDto>("get_visible_rows", {
        offset: Math.max(0, offset),
        limit: rowLimit,
        query,
        sortKey,
        sortDescending,
      });
      setRows(viewport.rows);
      setRowOffset(viewport.offset);
      setVisibleTotal(viewport.total);
    },
    [query, rowLimit, sortDescending, sortKey],
  );

  const startScanFor = useCallback(async (rawPath: string, options?: { force?: boolean }) => {
    const target = normalizeScanTarget(rawPath);
    if (!target) {
      return;
    }
    if (
      !options?.force &&
      scanningRef.current &&
      lastStartedPath.current.toLowerCase() === target.toLowerCase()
    ) {
      setStatus("Scanning");
      return;
    }
    if (
      !options?.force &&
      loadedPathRef.current.toLowerCase() === target.toLowerCase() &&
      !scanningRef.current
    ) {
      setStatus("Complete");
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
      await invoke("start_scan", { path: target, scanner: "auto", force: options?.force ?? false });
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
        sortKey,
        sortDescending,
      });
      setRows(viewport.rows);
      setRowOffset(viewport.offset);
      setVisibleTotal(viewport.total);
    },
    [query, rowLimit, rowOffset, sortDescending, sortKey],
  );

  const selectRow = useCallback(async (row: TreeRowDto) => {
    if (row.synthetic) {
      setSelectedId(null);
      setSelectedPath(row.detail ?? row.name);
      return;
    }
    const selected = await invoke<string | null>("select_entry", { id: row.id });
    setSelectedId(row.id);
    setSelectedPath(selected ?? "");
  }, []);

  const deleteSelected = useCallback(async (mode: DeleteMode) => {
    if (selectedId === null || !selectedPath) {
      return;
    }
    const permanently = mode === "permanent";
    const refreshTarget = normalizeScanTarget(path) || loadedPathRef.current;
    pendingDeleteRefreshPath.current = refreshTarget;
    deleteStartedAt.current = performance.now();
    setDeleteElapsedMs(0);
    setDeleting(true);
    setDeletePath(selectedPath);
    setStatus(permanently ? "Deleting" : "Moving to Recycle Bin");
    setDetail(selectedPath);
    setError("");
    setDeleteDialogOpen(false);
    try {
      await invoke("delete_entry", { id: selectedId, permanently });
      setSelectedId(null);
      setSelectedPath("");
    } catch (deleteError) {
      pendingDeleteRefreshPath.current = "";
      deleteStartedAt.current = null;
      setDeleting(false);
      setStatus("Delete failed");
      setError(String(deleteError));
    }
  }, [path, selectedId, selectedPath]);

  const openExplorer = useCallback(async (targetPath?: string, targetId?: number) => {
    setError("");
    try {
      await invoke("open_path", { path: targetPath ?? path, id: targetId ?? selectedId });
    } catch (openError) {
      setError(String(openError));
    }
  }, [path, selectedId]);

  const showProperties = useCallback(async (targetPath?: string, targetId?: number) => {
    setError("");
    try {
      await invoke("show_path_properties", { path: targetPath ?? path, id: targetId ?? selectedId });
    } catch (propertiesError) {
      setError(String(propertiesError));
    }
  }, [path, selectedId]);

  const refreshDrives = useCallback(async () => {
    const items = await invoke<DriveDto[]>("discover_drives");
    setDrives(items);
  }, []);

  const refreshScan = useCallback(async () => {
    await refreshDrives();
    const target = normalizeScanTarget(path);
    if (!target) {
      return;
    }
    setStatus("Refreshing");
    await startScanFor(target, { force: true });
  }, [path, refreshDrives, startScanFor]);

  const changeSort = useCallback((nextKey: SortKey) => {
    if (nextKey === sortKey) {
      setSortDescending((value) => !value);
      return;
    }
    setSortKey(nextKey);
    setSortDescending(nextKey !== "name");
  }, [sortKey]);

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
    pathRef.current = path;
  }, [path]);

  useEffect(() => {
    scanningRef.current = scanning;
  }, [scanning]);

  useEffect(() => {
    loadedPathRef.current = loadedPath;
  }, [loadedPath]);

  useEffect(() => {
    if (!deleting) {
      return;
    }
    const timer = window.setInterval(() => {
      if (deleteStartedAt.current !== null) {
        setDeleteElapsedMs(Math.max(0, performance.now() - deleteStartedAt.current));
      }
    }, 180);
    return () => window.clearInterval(timer);
  }, [deleting]);

  useEffect(() => {
    const showTooltip = (target: EventTarget | null) => {
      if (!(target instanceof Element)) {
        tooltipElementRef.current = null;
        setTooltip(null);
        return;
      }
      const element = target.closest<HTMLElement>("[data-tooltip]");
      const text = element?.dataset.tooltip?.trim();
      if (!element || !text) {
        tooltipElementRef.current = null;
        setTooltip(null);
        return;
      }
      tooltipElementRef.current = element;
      const rect = element.getBoundingClientRect();
      const estimatedWidth = Math.min(360, Math.max(112, text.length * 7 + 18));
      const estimatedHeight = text.length > 42 ? 46 : 30;
      const belowY = rect.bottom + 10;
      setTooltip({
        text,
        x: Math.min(
          window.innerWidth - estimatedWidth - 8,
          Math.max(8, rect.left + rect.width / 2 - estimatedWidth / 2),
        ),
        y: belowY + estimatedHeight > window.innerHeight - 8
          ? Math.max(8, rect.top - estimatedHeight - 10)
          : belowY,
      });
    };

    const hideTooltip = () => {
      tooltipElementRef.current = null;
      setTooltip(null);
    };
    const handlePointerOver = (event: PointerEvent) => showTooltip(event.target);
    const handlePointerOut = (event: PointerEvent) => {
      const active = tooltipElementRef.current;
      if (
        active &&
        event.relatedTarget instanceof Node &&
        active.contains(event.relatedTarget)
      ) {
        return;
      }
      hideTooltip();
    };
    const handleFocusIn = (event: FocusEvent) => showTooltip(event.target);

    document.addEventListener("pointerover", handlePointerOver);
    document.addEventListener("pointerout", handlePointerOut);
    document.addEventListener("focusin", handleFocusIn);
    document.addEventListener("focusout", hideTooltip);
    document.addEventListener("pointerdown", hideTooltip);
    document.addEventListener("scroll", hideTooltip, true);
    window.addEventListener("resize", hideTooltip);
    return () => {
      document.removeEventListener("pointerover", handlePointerOver);
      document.removeEventListener("pointerout", handlePointerOut);
      document.removeEventListener("focusin", handleFocusIn);
      document.removeEventListener("focusout", hideTooltip);
      document.removeEventListener("pointerdown", hideTooltip);
      document.removeEventListener("scroll", hideTooltip, true);
      window.removeEventListener("resize", hideTooltip);
    };
  }, []);

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
    const delay = isDriveRoot(target) ? 180 : 1100;
    const timer = window.setTimeout(() => {
      if (
        !scanningRef.current &&
        complete &&
        loadedPathRef.current.toLowerCase() === target.toLowerCase()
      ) {
        return;
      }
      void startScanFor(target);
    }, delay);
    return () => window.clearTimeout(timer);
  }, [complete, path, ready, startScanFor]);

  useEffect(() => {
    if (!complete || scanning) {
      return;
    }
    const timer = window.setTimeout(() => {
      const element = treeRef.current;
      if (element) {
        element.scrollTop = 0;
      }
      void loadRows(0);
    }, 120);
    return () => window.clearTimeout(timer);
  }, [complete, loadRows, query, scanning, sortDescending, sortKey]);

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
        setLoadedPath(lastStartedPath.current);
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
      listen<DeleteEventDto>("delete-started", (event) => {
        deleteStartedAt.current = performance.now();
        setDeleteElapsedMs(0);
        setDeleting(true);
        setDeletePath(event.payload.path);
        setStatus(event.payload.permanently ? "Deleting" : "Moving to Recycle Bin");
        setDetail(event.payload.path);
      }),
      listen<DeleteEventDto>("delete-complete", (event) => {
        const refreshTarget = pendingDeleteRefreshPath.current;
        pendingDeleteRefreshPath.current = "";
        deleteStartedAt.current = null;
        setDeleteElapsedMs(event.payload.elapsedMs);
        setDeleting(false);
        setDeletePath("");
        setSelectedId(null);
        setSelectedPath("");
        setStatus("Deleted");
        setDetail(`${shortPathLabel(event.payload.path)} · ${formatElapsed(event.payload.elapsedMs)}`);

        if (
          refreshTarget &&
          normalizeScanTarget(pathRef.current).toLowerCase() === refreshTarget.toLowerCase()
        ) {
          void startScanFor(refreshTarget, { force: true });
        }
      }),
      listen<DeleteErrorDto>("delete-error", (event) => {
        pendingDeleteRefreshPath.current = "";
        deleteStartedAt.current = null;
        setDeleteElapsedMs(event.payload.elapsedMs);
        setDeleting(false);
        setStatus("Delete failed");
        setDetail(event.payload.path);
        setError(event.payload.error);
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
  const selectedRow = displayRows.find((row) => row.id === selectedId) ?? null;
  const infoPath = selectedPath || path;

  return (
    <main className="app-shell" data-scanning={scanning || deleting} data-rail-collapsed={railCollapsed}>
        <aside className="drive-rail">
        <div className="rail-title">
          <div className="product-mark" aria-hidden="true">
            <LogoMark />
          </div>
          <button
            className="icon-button has-tooltip"
            type="button"
            aria-label="Toggle drives"
            data-tooltip={railCollapsed ? "Show drives" : "Collapse drives"}
            onClick={() => setRailCollapsed((value) => !value)}
          >
            <Menu size={21} strokeWidth={1.8} />
          </button>
        </div>

        <div className="drive-list">
          {drives.map((drive) => {
            const drivePath = normalizeScanTarget(drive.path).toLowerCase();
            const usedPercent = driveUsedPercent(drive);
            return (
              <button
                key={drive.path}
                className="drive-card"
                data-active={drivePath === activeDriveRoot}
                data-root={drive.path.replace("\\", "")}
                onClick={() => chooseDrive(drive)}
                type="button"
              >
                <span className="drive-icon">
                  <HardDrive size={29} strokeWidth={1.45} />
                </span>
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
            <Search className="search-glyph" size={22} strokeWidth={1.75} />
            <input
              autoFocus
              spellcheck={false}
              value={commandText}
              placeholder="Search or enter path..."
              onInput={(event) => handleCommandInput(event.currentTarget.value)}
            />
          </label>
          <div className="scan-timer" data-active={scanning || deleting}>
            <span className="live-dot" data-active={scanning || deleting} />
            <span>
              {deleting
                ? `${formatElapsed(deleteElapsedMs)} · deleting ${shortPathLabel(deletePath)}`
                : scanning
                ? `${formatElapsed(totals.elapsedMs)} · ${formatCount(totals.entries)} scanned`
                : complete
                  ? complete.elapsedMs === 0
                    ? "cached"
                    : `${formatElapsed(complete.elapsedMs)} scan`
                  : status}
            </span>
          </div>
          <div className="command-actions">
            <button
              className="icon-button has-tooltip"
              type="button"
              aria-label="Show selected in folder"
              data-tooltip="Show selected in folder"
              onClick={() => void openExplorer()}
            >
              <FolderOpen size={21} strokeWidth={1.7} />
            </button>
            <button
              className="icon-button has-tooltip"
              type="button"
              aria-label="Scan details"
              data-tooltip="Scan details"
              onClick={() => setInfoOpen((value) => !value)}
            >
              <Info size={21} strokeWidth={1.7} />
            </button>
            <button
              className="icon-button has-tooltip"
              type="button"
              aria-label="Force rescan"
              data-tooltip="Force rescan"
              disabled={deleting}
              onClick={() => void refreshScan()}
            >
              <RefreshCw size={20} strokeWidth={1.7} />
            </button>
            {scanning ? (
              <button className="text-action" onClick={cancelScan} type="button">
                Stop
              </button>
            ) : deleting ? (
              <button
                className="text-action"
                disabled
                type="button"
              >
                Deleting
              </button>
            ) : (
              <button
                className="text-action danger"
                disabled={selectedId === null}
                onClick={() => setDeleteDialogOpen(true)}
                type="button"
              >
                <Trash2 size={17} strokeWidth={1.8} />
                Recycle
              </button>
            )}
            {infoOpen ? (
              <section className="info-popover" aria-label="Scan details">
                <div className="info-title">
                  <strong>{selectedRow ? "Selected item" : "Current scan"}</strong>
                  <span>{status}</span>
                </div>
                <dl>
                  <div>
                    <dt>Path</dt>
                    <dd>{infoPath || "-"}</dd>
                  </div>
                  <div>
                    <dt>Scanner</dt>
                    <dd>{complete?.fallbackReason ?? complete?.scannerLabel ?? detail}</dd>
                  </div>
                  <div>
                    <dt>Entries</dt>
                    <dd>{formatCount(totals.entries)}</dd>
                  </div>
                  <div>
                    <dt>Size</dt>
                    <dd>{formatBytes(selectedRow?.sizeBytes ?? complete?.sizeBytes ?? 0)}</dd>
                  </div>
                  <div>
                    <dt>Allocated</dt>
                    <dd>{formatBytes(selectedRow?.allocatedBytes ?? complete?.allocatedBytes ?? 0)}</dd>
                  </div>
                  {complete?.windowsUsedBytes !== null && complete?.windowsUsedBytes !== undefined ? (
                    <div>
                      <dt>Windows used</dt>
                      <dd>{formatBytes(complete.windowsUsedBytes)}</dd>
                    </div>
                  ) : null}
                  {complete?.unattributedBytes ? (
                    <div>
                      <dt>Unattributed</dt>
                      <dd>{formatBytes(complete.unattributedBytes)}</dd>
                    </div>
                  ) : null}
                  <div>
                    <dt>Elapsed</dt>
                    <dd>{complete?.elapsedMs === 0 ? "cached" : formatElapsed(totals.elapsedMs)}</dd>
                  </div>
                </dl>
                <div className="info-actions">
                  <button type="button" onClick={() => void openExplorer()}>
                    Show in folder
                  </button>
                  <button type="button" onClick={() => void showProperties()}>
                    Properties
                  </button>
                </div>
              </section>
            ) : null}
          </div>
        </header>

        <section className="scan-readout">
          <div>
            <div className="readout-status">
              <span className="live-dot" data-active={scanning || deleting} />
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
            <span>
              {complete
                ? `${formatBytes(complete.sizeBytes)} size · ${formatBytes(complete.allocatedBytes)} allocated${
                    complete.unattributedBytes ? ` · ${formatBytes(complete.unattributedBytes)} unattributed` : ""
                  }`
                : "Scanning"}
            </span>
            <div className="capacity-track" data-live={scanning || deleting}>
              <span style={{ width: `${activeUsedPercent ?? 0}%` }} />
            </div>
            <span>{activeUsedPercent === null ? "" : `${Math.round(activeUsedPercent)}% full`}</span>
          </div>
        </section>

        <section className="table-shell">
          <div className="table-header">
            <button type="button" onClick={() => changeSort("name")} data-active={sortKey === "name"}>
              Name
              {sortKey === "name" && sortDescending ? (
                <ChevronDown size={15} strokeWidth={1.8} />
              ) : (
                <ChevronUp size={15} strokeWidth={1.8} />
              )}
            </button>
            <button type="button" onClick={() => changeSort("size")} data-active={sortKey === "size"}>
              Size
              {sortKey === "size" && !sortDescending ? (
                <ChevronUp size={15} strokeWidth={1.8} />
              ) : (
                <ChevronDown size={15} strokeWidth={1.8} />
              )}
              <Filter size={17} strokeWidth={1.7} />
            </button>
            <button
              type="button"
              onClick={() => changeSort("allocated")}
              data-active={sortKey === "allocated"}
            >
              Allocated
              {sortKey === "allocated" && !sortDescending ? (
                <ChevronUp size={15} strokeWidth={1.8} />
              ) : (
                <ChevronDown size={15} strokeWidth={1.8} />
              )}
              <Filter size={17} strokeWidth={1.7} />
            </button>
            <button type="button" onClick={() => changeSort("modified")} data-active={sortKey === "modified"}>
              Last Modified
              {sortKey === "modified" && !sortDescending ? (
                <ChevronUp size={15} strokeWidth={1.8} />
              ) : (
                <ChevronDown size={15} strokeWidth={1.8} />
              )}
              <Filter size={17} strokeWidth={1.7} />
            </button>
          </div>

          <div className="tree-viewport" ref={treeRef} onScroll={handleScroll}>
            <div style={{ height: topSpacer }} />
            {displayRows.map((row) => {
              const displaySize = displaySizeBytes(row, activeDriveInfo);
              const displayAllocated = displayAllocatedBytes(row, activeDriveInfo);
              const share = Math.max(2, Math.min(100, (displayAllocated / rowBarBase) * 100));
              const shareLabel = Math.round(Math.min(100, (displayAllocated / rowBarBase) * 100));
              return (
                <div
                  key={row.synthetic ? `synthetic-${row.path}` : row.id}
                  className="tree-row"
                  data-selected={!row.synthetic && selectedId === row.id}
                  data-synthetic={row.synthetic ? "true" : "false"}
                  style={{ "--depth": row.depth } as JSX.CSSProperties}
                  onClick={() => void selectRow(row)}
                  onDblClick={() => {
                    if (!row.synthetic) {
                      void toggleRow(row);
                    }
                  }}
                  onKeyDown={(event) => handleRowKeyDown(event, row, selectRow, toggleRow)}
                  role="button"
                  tabIndex={0}
                >
                  <span className="row-name">
                    <span
                      className="disclosure"
                      data-visible={!row.synthetic && row.childCount > 0}
                      onClick={(event) => {
                        event.stopPropagation();
                        if (!row.synthetic) {
                          void toggleRow(row);
                        }
                      }}
                    >
                      {row.synthetic || row.childCount === 0 ? null : row.expanded ? (
                        <ChevronDown size={14} strokeWidth={1.8} />
                      ) : (
                        <ChevronRight size={14} strokeWidth={1.8} />
                      )}
                    </span>
                    <span className="item-icon" data-kind={row.synthetic ? "system" : row.isDir ? "dir" : "file"}>
                      {row.synthetic ? (
                        <Info size={21} strokeWidth={1.45} />
                      ) : row.isDir ? (
                        <Folder size={23} strokeWidth={1.35} />
                      ) : row.name.includes(".") ? (
                        <FileText size={22} strokeWidth={1.35} />
                      ) : (
                        <File size={22} strokeWidth={1.35} />
                      )}
                    </span>
                    <span className="name-text">{row.name}</span>
                    <button
                      className="row-folder-button has-tooltip"
                      type="button"
                      aria-label={row.isDir ? "Open folder" : "Show in folder"}
                      data-tooltip={row.isDir ? "Open folder" : "Show in folder"}
                      disabled={row.synthetic}
                      onClick={(event) => {
                        event.stopPropagation();
                        if (!row.synthetic) {
                          void openExplorer(row.path, row.id);
                        }
                      }}
                    >
                      <FolderOpen size={16} strokeWidth={1.75} />
                    </button>
                  </span>
                  <span className="size-cell">
                    <span>{formatBytes(displaySize)}</span>
                  </span>
                  <span className="size-cell allocated-cell">
                    <span>{formatBytes(displayAllocated)}</span>
                    <span className="row-meter">
                      <span style={{ width: `${share}%` }} />
                    </span>
                    <span className="percent-cell">{shareLabel}%</span>
                  </span>
                  <span className="date-cell">{formatModified(row.modifiedUnix)}</span>
                </div>
              );
            })}
            {displayRows.length === 0 ? (
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
      {deleteDialogOpen ? (
        <div className="dialog-backdrop" role="presentation" onClick={() => setDeleteDialogOpen(false)}>
          <section className="delete-dialog" role="dialog" aria-modal="true" onClick={(event) => event.stopPropagation()}>
            <h2>Delete selected item?</h2>
            <p>{selectedPath}</p>
            <div className="dialog-actions">
              <button type="button" disabled={deleting} onClick={() => setDeleteDialogOpen(false)}>
                Cancel
              </button>
              <button
                type="button"
                disabled={deleting}
                onClick={() => void deleteSelected("recycle")}
              >
                Move to Recycle Bin
              </button>
              <button
                className="danger"
                type="button"
                disabled={deleting}
                onClick={() => void deleteSelected("permanent")}
              >
                Permanently delete
              </button>
            </div>
          </section>
        </div>
      ) : null}
      <TooltipLayer tooltip={tooltip} />
    </main>
  );
}

function LogoMark() {
  return <img className="logo-mark" src="/icon.png" alt="" draggable={false} />;
}

function TooltipLayer(props: { tooltip: TooltipState }) {
  if (!props.tooltip) {
    return null;
  }
  return (
    <div
      className="app-tooltip"
      style={{ left: props.tooltip.x, top: props.tooltip.y } as JSX.CSSProperties}
      role="tooltip"
    >
      {props.tooltip.text}
    </div>
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

function handleRowKeyDown(
  event: JSX.TargetedKeyboardEvent<HTMLDivElement>,
  row: TreeRowDto,
  selectRow: (row: TreeRowDto) => Promise<void>,
  toggleRow: (row: TreeRowDto) => Promise<void>,
) {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    void selectRow(row);
  }
  if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
    event.preventDefault();
    void toggleRow(row);
  }
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

function shortPathLabel(value: string) {
  const trimmed = value.trim().replace(/[\\/]+$/, "");
  if (!trimmed) {
    return "item";
  }
  const match = /[^\\/]+$/.exec(trimmed);
  return match?.[0] || trimmed;
}

function isDriveRoot(value: string) {
  return /^[a-zA-Z]:[\\/]*$/.test(value.trim());
}

function buildAccountingRow(complete: ScanCompleteDto | null, targetPath: string): TreeRowDto | null {
  if (!complete?.unattributedBytes || !isDriveRoot(targetPath)) {
    return null;
  }
  const root = normalizeScanTarget(targetPath);
  return {
    id: -1,
    name: "Unattributed / system reserved",
    path: `${root}::diskloom-accounting-gap`,
    depth: 1,
    isDir: false,
    expanded: false,
    childCount: 0,
    sizeBytes: complete.unattributedBytes,
    allocatedBytes: complete.unattributedBytes,
    modifiedUnix: 0,
    synthetic: true,
    detail:
      "Windows reports this space as used, but it is not exposed as normal file-tree entries. It can include restore points, shadow copies, NTFS metadata, journals, reserved clusters, or protected system allocation.",
  };
}

function injectAccountingRow(rows: TreeRowDto[], accountingRow: TreeRowDto | null, query: string) {
  if (!accountingRow || query.trim()) {
    return rows;
  }
  const rootIndex = rows.findIndex((row) => row.depth === 0);
  if (rootIndex < 0) {
    return rows;
  }
  return [
    ...rows.slice(0, rootIndex + 1),
    accountingRow,
    ...rows.slice(rootIndex + 1),
  ];
}

function driveRootFromTarget(value: string) {
  const target = normalizeScanTarget(value).toLowerCase();
  const match = /^([a-z]):/.exec(target);
  if (!match) {
    return target;
  }
  return `${match[1]}:\\`;
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

function displaySizeBytes(row: TreeRowDto, activeDrive: DriveDto | undefined) {
  if (!isActiveDriveRootRow(row, activeDrive) || !activeDrive?.totalBytes || activeDrive.freeBytes === null) {
    return row.sizeBytes;
  }
  return Math.max(0, activeDrive.totalBytes - activeDrive.freeBytes);
}

function displayAllocatedBytes(row: TreeRowDto, activeDrive: DriveDto | undefined) {
  if (!isActiveDriveRootRow(row, activeDrive) || !activeDrive?.totalBytes || activeDrive.freeBytes === null) {
    return row.allocatedBytes;
  }
  return Math.max(0, activeDrive.totalBytes - activeDrive.freeBytes);
}

function isActiveDriveRootRow(row: TreeRowDto, activeDrive: DriveDto | undefined) {
  if (!activeDrive) {
    return false;
  }
  return normalizeScanTarget(row.path).toLowerCase() === normalizeScanTarget(activeDrive.path).toLowerCase();
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

function formatElapsed(ms: number) {
  if (ms < 1000) {
    return `${formatCount(ms)} ms`;
  }
  if (ms < 60_000) {
    const seconds = ms / 1000;
    return `${seconds < 10 ? seconds.toFixed(1) : seconds.toFixed(0)} s`;
  }
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1000);
  return `${minutes}m ${seconds}s`;
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
