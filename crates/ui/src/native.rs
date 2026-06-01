use std::{
    ffi::c_void,
    mem::zeroed,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Instant,
};

use anyhow::{Result, bail};
use diskloom_core::{EntryFlags, EntryId, FileGraph};
use diskloom_ntfs::{NtfsScanControl, NtfsScanProgress, NtfsScanner};
use diskloom_query::{
    FileTypeStat, TreemapBounds, TreemapItem, file_type_stats, layout_treemap,
    top_entries_by_own_size,
};
use diskloom_scan::{FallbackScanner, ScanControl, ScanOptions, ScanSummary};
use diskloom_windows::{
    VolumeKind, discover_volumes, is_process_elevated, relaunch_current_process_elevated,
};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM},
    Graphics::Gdi::{
        CreateSolidBrush, DEFAULT_GUI_FONT, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE,
        DT_VCENTER, DeleteObject, DrawTextW, FillRect, GetStockObject, HBRUSH, HDC, HFONT,
        InvalidateRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
        UpdateWindow,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
        Input::KeyboardAndMouse::EnableWindow,
        WindowsAndMessaging::{
            BS_PUSHBUTTON, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW,
            DefWindowProcW, DispatchMessageW, ES_AUTOHSCROLL, GWLP_USERDATA, GetClientRect,
            GetMessageW, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW,
            LoadCursorW, MSG, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOW,
            SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, WM_APP,
            WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY,
            WM_ERASEBKGND, WM_MOUSEWHEEL, WM_NCCREATE, WM_PAINT, WM_SETFONT, WM_SIZE, WNDCLASSW,
            WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
        },
    },
};

const UI_PROGRESS_EVERY: u64 = 1_024;
const TREE_ROW_LIMIT: usize = 700;
const FILE_ROW_LIMIT: usize = 700;
const TYPE_ROW_LIMIT: usize = 120;
const TREEMAP_LIMIT: usize = 160;

const TOP_HEIGHT: i32 = 64;
const RAIL_WIDTH: i32 = 312;
const ROW_HEIGHT: i32 = 27;
const WM_SCAN_MESSAGE: u32 = WM_APP + 1;
const WM_START_SCAN: u32 = WM_APP + 2;

const IDC_PATH: u16 = 1001;
const IDC_SCAN: u16 = 1002;
const IDC_CANCEL: u16 = 1003;
const IDC_AUTO: u16 = 1004;
const IDC_NTFS: u16 = 1005;
const IDC_FALLBACK: u16 = 1006;
const IDC_TAB_TREE: u16 = 1101;
const IDC_TAB_FILES: u16 = 1102;
const IDC_TAB_TYPES: u16 = 1103;
const IDC_TAB_TREEMAP: u16 = 1104;
const IDC_DRIVE_BASE: u16 = 2000;

const COLOR_BG: u32 = rgb(18, 20, 22);
const COLOR_TOP: u32 = rgb(26, 28, 30);
const COLOR_RAIL: u32 = rgb(22, 24, 26);
const COLOR_PANEL: u32 = rgb(30, 33, 36);
const COLOR_PANEL_ALT: u32 = rgb(37, 40, 43);
const COLOR_CONTROL: u32 = rgb(17, 19, 21);
const COLOR_LINE: u32 = rgb(52, 56, 60);
const COLOR_TEXT: u32 = rgb(232, 235, 236);
const COLOR_MUTED: u32 = rgb(154, 158, 162);
const COLOR_WARN: u32 = rgb(224, 164, 82);
const COLOR_ERROR: u32 = rgb(238, 112, 104);

const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | ((green as u32) << 8) | ((blue as u32) << 16)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Tree,
    Files,
    Types,
    Treemap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiScannerMode {
    Auto,
    Ntfs,
    Fallback,
}

impl UiScannerMode {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Ntfs => "NTFS",
            Self::Fallback => "Fallback",
        }
    }

    fn arg_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ntfs => "ntfs",
            Self::Fallback => "fallback",
        }
    }

    fn from_arg(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "ntfs" => Some(Self::Ntfs),
            "fallback" => Some(Self::Fallback),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeShortcut {
    root: String,
    label: String,
    is_ntfs: bool,
}

#[derive(Debug)]
enum UiState {
    Idle,
    Scanning(Option<UiScanProgress>),
    Complete(Box<ScanResult>),
    Error(String),
}

#[derive(Debug)]
enum ScanMessage {
    Progress(UiScanProgress),
    Complete(Box<ScanResult>),
    Error(String),
}

#[derive(Debug, Clone, Copy)]
struct UiScanProgress {
    summary: ScanSummary,
    elapsed_ms: u128,
}

#[derive(Debug)]
struct ScanResult {
    graph: Arc<FileGraph>,
    summary: ScanSummary,
    elapsed_ms: u128,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
    total_size: u64,
    total_allocated: u64,
    tree_rows: Vec<TreeRow>,
    file_rows: Vec<FileRow>,
    type_rows: Vec<FileTypeStat>,
    treemap_items: Vec<TreemapItem>,
}

#[derive(Debug, Clone, Copy)]
struct TreeRow {
    id: EntryId,
    depth: usize,
    kind: &'static str,
    size: u64,
    allocated: u64,
}

#[derive(Debug, Clone, Copy)]
struct FileRow {
    id: EntryId,
    size: u64,
    allocated: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ChildRange {
    start: u32,
    len: u32,
}

#[derive(Debug)]
struct UiScanOutcome {
    graph: FileGraph,
    summary: ScanSummary,
    scanner_label: &'static str,
    fallback_reason: Option<String>,
}

struct NativeApp {
    hwnd: HWND,
    hinstance: HINSTANCE,
    font: HFONT,
    control_brush: HBRUSH,
    path_edit: HWND,
    scan_button: HWND,
    cancel_button: HWND,
    auto_button: HWND,
    ntfs_button: HWND,
    fallback_button: HWND,
    tab_tree_button: HWND,
    tab_files_button: HWND,
    tab_types_button: HWND,
    tab_treemap_button: HWND,
    drive_buttons: Vec<HWND>,
    volumes: Vec<VolumeShortcut>,
    scanner_mode: UiScannerMode,
    active_tab: ActiveTab,
    state: UiState,
    receiver: Option<Receiver<ScanMessage>>,
    scan_cancel: Option<Arc<AtomicBool>>,
    status: String,
    scroll_top: usize,
    start_on_launch: bool,
    initial_path: String,
}

impl Drop for NativeApp {
    fn drop(&mut self) {
        if !self.control_brush.is_null() {
            // SAFETY: `control_brush` is an owned GDI brush created by this app.
            unsafe {
                let _ = DeleteObject(self.control_brush);
            }
        }
    }
}

pub fn run_from_env_args() -> Result<()> {
    NativeApp::from_env_args()?.run()
}

impl NativeApp {
    fn from_env_args() -> Result<Self> {
        let volumes = discover_volume_shortcuts();
        let system_drive = std::env::var("SystemDrive").ok();
        let mut app = Self::new(
            default_scan_path_from(system_drive.as_deref(), &volumes),
            volumes,
        )?;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--path" => {
                    if let Some(path) = args.next() {
                        app.initial_path = path;
                    }
                }
                "--scanner" => {
                    if let Some(scanner) = args
                        .next()
                        .and_then(|value| UiScannerMode::from_arg(&value))
                    {
                        app.scanner_mode = scanner;
                    }
                }
                "--scan" => app.start_on_launch = true,
                _ => {}
            }
        }

        Ok(app)
    }

    fn new(initial_path: String, volumes: Vec<VolumeShortcut>) -> Result<Self> {
        // SAFETY: Stock GUI font is owned by the system and valid for the process lifetime.
        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) as HFONT };
        // SAFETY: CreateSolidBrush returns an owned brush handle or null on failure.
        let control_brush = unsafe { CreateSolidBrush(COLOR_CONTROL) };
        if control_brush.is_null() {
            bail!(
                "failed to create UI brush: {}",
                std::io::Error::last_os_error()
            );
        }

        Ok(Self {
            hwnd: null_mut(),
            hinstance: null_mut(),
            font,
            control_brush,
            path_edit: null_mut(),
            scan_button: null_mut(),
            cancel_button: null_mut(),
            auto_button: null_mut(),
            ntfs_button: null_mut(),
            fallback_button: null_mut(),
            tab_tree_button: null_mut(),
            tab_files_button: null_mut(),
            tab_types_button: null_mut(),
            tab_treemap_button: null_mut(),
            drive_buttons: Vec::new(),
            volumes,
            scanner_mode: UiScannerMode::Auto,
            active_tab: ActiveTab::Tree,
            state: UiState::Idle,
            receiver: None,
            scan_cancel: None,
            status: "Ready".to_owned(),
            scroll_top: 0,
            start_on_launch: false,
            initial_path,
        })
    }

    fn run(mut self) -> Result<()> {
        // SAFETY: DPI awareness can be requested once at process startup. Failure only means
        // Windows keeps the default awareness context, so the result is intentionally ignored.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }

        // SAFETY: Null module name asks for this process module handle.
        let hinstance = unsafe { GetModuleHandleW(null()) };
        if hinstance.is_null() {
            bail!(
                "failed to get module handle: {}",
                std::io::Error::last_os_error()
            );
        }
        self.hinstance = hinstance;

        let class_name = wide_null("DiskLoomNativeWindow");
        let title = wide_null("DiskLoom");
        // SAFETY: Loading the shared arrow cursor does not transfer ownership.
        let cursor = unsafe { LoadCursorW(null_mut(), IDC_ARROW) };
        let window_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            hCursor: cursor,
            lpszClassName: class_name.as_ptr(),
            ..unsafe { zeroed() }
        };

        // SAFETY: `window_class` points to valid data for the duration of the call.
        let atom = unsafe { RegisterClassW(&window_class) };
        if atom == 0 {
            bail!(
                "failed to register window class: {}",
                std::io::Error::last_os_error()
            );
        }

        let mut app = Box::new(self);
        let app_ptr = app.as_mut() as *mut Self;
        // SAFETY: Class/title buffers are null-terminated. `app_ptr` stays alive until the
        // message loop exits and is stored as window user data in WM_NCCREATE.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1320,
                780,
                null_mut(),
                null_mut(),
                hinstance,
                app_ptr.cast::<c_void>(),
            )
        };
        if hwnd.is_null() {
            bail!(
                "failed to create window: {}",
                std::io::Error::last_os_error()
            );
        }

        let raw = Box::into_raw(app);
        // SAFETY: `hwnd` is a valid top-level window created above.
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
        }

        let result = message_loop();
        // SAFETY: `raw` was created by Box::into_raw above and is dropped after the window loop.
        unsafe {
            drop(Box::from_raw(raw));
        }
        result
    }

    fn create_controls(&mut self) -> Result<()> {
        self.path_edit = self.create_control(
            "EDIT",
            &self.initial_path,
            WS_BORDER | ES_AUTOHSCROLL as u32 | WS_TABSTOP,
            IDC_PATH,
        )?;
        self.scan_button = self.create_control("BUTTON", "Scan", button_style(), IDC_SCAN)?;
        self.cancel_button = self.create_control("BUTTON", "Cancel", button_style(), IDC_CANCEL)?;
        self.auto_button = self.create_control("BUTTON", "Auto", button_style(), IDC_AUTO)?;
        self.ntfs_button = self.create_control("BUTTON", "NTFS", button_style(), IDC_NTFS)?;
        self.fallback_button =
            self.create_control("BUTTON", "Fallback", button_style(), IDC_FALLBACK)?;
        self.tab_tree_button =
            self.create_control("BUTTON", "Tree", button_style(), IDC_TAB_TREE)?;
        self.tab_files_button =
            self.create_control("BUTTON", "Files", button_style(), IDC_TAB_FILES)?;
        self.tab_types_button =
            self.create_control("BUTTON", "Types", button_style(), IDC_TAB_TYPES)?;
        self.tab_treemap_button =
            self.create_control("BUTTON", "Treemap", button_style(), IDC_TAB_TREEMAP)?;

        for (idx, volume) in self.volumes.iter().enumerate() {
            let id = IDC_DRIVE_BASE.saturating_add(idx as u16);
            let button = self.create_control("BUTTON", &volume.label, button_style(), id)?;
            self.drive_buttons.push(button);
        }

        self.set_running_controls(false);
        self.layout_controls();
        if self.start_on_launch {
            // SAFETY: Posting to our own valid window handle schedules scan startup after
            // controls are fully created.
            unsafe {
                let _ = PostMessageW(self.hwnd, WM_START_SCAN, 0, 0);
            }
        }
        Ok(())
    }

    fn create_control(&self, class: &str, text: &str, style: u32, id: u16) -> Result<HWND> {
        let class_w = wide_null(class);
        let text_w = wide_null(text);
        // SAFETY: Class/text buffers are null-terminated for the duration of the call. Parent
        // window and module handles are valid after WM_NCCREATE.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_w.as_ptr(),
                text_w.as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                10,
                10,
                self.hwnd,
                id as usize as HMENU,
                self.hinstance,
                null_mut(),
            )
        };
        if hwnd.is_null() {
            bail!(
                "failed to create {class} control: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: The child window is valid and `font` is the shared stock GUI font.
        unsafe {
            let _ = SendMessageW(hwnd, WM_SETFONT, self.font as usize, TRUE as isize);
        }
        Ok(hwnd)
    }

    fn layout_controls(&self) {
        let rect = self.client_rect();
        let width = rect.right.saturating_sub(rect.left);
        let main_left = RAIL_WIDTH + 1;
        let main_width = width.saturating_sub(main_left);

        move_window(self.path_edit, 18, 128, RAIL_WIDTH - 36, 26);
        move_window(self.auto_button, 18, 184, 72, 30);
        move_window(self.ntfs_button, 98, 184, 72, 30);
        move_window(self.fallback_button, 178, 184, 94, 30);
        move_window(self.scan_button, 18, 258, 108, 34);
        move_window(self.cancel_button, 136, 258, 108, 34);

        let mut x = 18;
        let mut y = 222;
        for button in &self.drive_buttons {
            move_window(*button, x, y, 78, 28);
            x += 86;
            if x + 78 > RAIL_WIDTH - 18 {
                x = 18;
                y += 34;
            }
        }

        let tab_y = TOP_HEIGHT + 18;
        move_window(self.tab_tree_button, main_left + 24, tab_y, 74, 30);
        move_window(self.tab_files_button, main_left + 104, tab_y, 74, 30);
        move_window(self.tab_types_button, main_left + 184, tab_y, 74, 30);
        move_window(self.tab_treemap_button, main_left + 264, tab_y, 104, 30);

        let _ = main_width;
    }

    fn on_command(&mut self, id: u16) {
        match id {
            IDC_SCAN => self.start_scan(),
            IDC_CANCEL => self.cancel_scan(),
            IDC_AUTO => self.set_scanner_mode(UiScannerMode::Auto),
            IDC_NTFS => self.set_scanner_mode(UiScannerMode::Ntfs),
            IDC_FALLBACK => self.set_scanner_mode(UiScannerMode::Fallback),
            IDC_TAB_TREE => self.set_active_tab(ActiveTab::Tree),
            IDC_TAB_FILES => self.set_active_tab(ActiveTab::Files),
            IDC_TAB_TYPES => self.set_active_tab(ActiveTab::Types),
            IDC_TAB_TREEMAP => self.set_active_tab(ActiveTab::Treemap),
            id if id >= IDC_DRIVE_BASE => {
                let index = usize::from(id - IDC_DRIVE_BASE);
                if let Some(volume) = self.volumes.get(index) {
                    self.set_path_text(&volume.root);
                    self.status = format!("Path set to {}", volume.root);
                    self.invalidate();
                }
            }
            _ => {}
        }
    }

    fn set_scanner_mode(&mut self, mode: UiScannerMode) {
        self.scanner_mode = mode;
        self.status = format!("Scanner set to {}", mode.label());
        self.invalidate();
    }

    fn set_active_tab(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
        self.scroll_top = 0;
        self.invalidate();
    }

    fn start_scan(&mut self) {
        if matches!(self.state, UiState::Scanning(_)) {
            return;
        }

        let path_text = self.path_text();
        let trimmed = path_text.trim();
        let path = PathBuf::from(if trimmed.is_empty() { "." } else { trimmed });
        let scanner_mode = self.scanner_mode;

        match maybe_relaunch_ui_scan_elevated(&path, scanner_mode) {
            Ok(true) => {
                self.status = "Administrator access requested for direct NTFS scanning".to_owned();
                self.invalidate();
                return;
            }
            Ok(false) => {}
            Err(error) => {
                self.state = UiState::Error(error);
                self.invalidate();
                return;
            }
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.scan_cancel = Some(Arc::clone(&cancel));
        self.state = UiState::Scanning(None);
        self.status = format!("Scanning {}", path.display());
        self.scroll_top = 0;
        self.set_running_controls(true);
        self.invalidate();

        let hwnd = self.hwnd as isize;
        thread::spawn(move || {
            let started = Instant::now();
            let progress_sender = sender.clone();
            let mut on_progress = |summary| {
                let _ = progress_sender.send(ScanMessage::Progress(UiScanProgress {
                    summary,
                    elapsed_ms: started.elapsed().as_millis(),
                }));
                post_scan_message(hwnd);
            };

            let result = scan_path(path, scanner_mode, &cancel, &mut on_progress).map(|outcome| {
                let graph = Arc::new(outcome.graph);
                let tree_rows = tree_rows_from_graph(&graph, TREE_ROW_LIMIT);
                let file_rows = file_rows_from_graph(&graph, FILE_ROW_LIMIT);
                let type_rows = file_type_stats(&graph, TYPE_ROW_LIMIT);
                let treemap_items = treemap_items_from_graph(&graph, TREEMAP_LIMIT);
                let (total_size, total_allocated) = graph_totals(&graph);

                ScanResult {
                    graph,
                    summary: outcome.summary,
                    elapsed_ms: started.elapsed().as_millis(),
                    scanner_label: outcome.scanner_label,
                    fallback_reason: outcome.fallback_reason,
                    total_size,
                    total_allocated,
                    tree_rows,
                    file_rows,
                    type_rows,
                    treemap_items,
                }
            });

            let message = match result {
                Ok(result) => ScanMessage::Complete(Box::new(result)),
                Err(error) => ScanMessage::Error(error),
            };
            let _ = sender.send(message);
            post_scan_message(hwnd);
        });
    }

    fn cancel_scan(&mut self) {
        if let Some(cancel) = &self.scan_cancel {
            cancel.store(true, Ordering::Relaxed);
            self.status = "Cancelling scan".to_owned();
            self.invalidate();
        }
    }

    fn receive_scan(&mut self) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };

        let mut keep_receiver = true;
        loop {
            match receiver.try_recv() {
                Ok(ScanMessage::Progress(progress)) => {
                    self.state = UiState::Scanning(Some(progress));
                }
                Ok(ScanMessage::Complete(result)) => {
                    self.status = format!(
                        "{} entries scanned in {} ms",
                        format_count(result.summary.entries),
                        result.elapsed_ms
                    );
                    self.state = UiState::Complete(result);
                    self.scan_cancel = None;
                    self.scroll_top = 0;
                    self.set_running_controls(false);
                    keep_receiver = false;
                    break;
                }
                Ok(ScanMessage::Error(error)) => {
                    self.state = UiState::Error(error);
                    self.status = "Scan failed".to_owned();
                    self.scan_cancel = None;
                    self.set_running_controls(false);
                    keep_receiver = false;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scan_cancel = None;
                    self.set_running_controls(false);
                    keep_receiver = false;
                    break;
                }
            }
        }

        if keep_receiver {
            self.receiver = Some(receiver);
        }
        self.invalidate();
    }

    fn set_running_controls(&self, running: bool) {
        enable_window(self.scan_button, !running);
        enable_window(self.cancel_button, running);
        enable_window(self.auto_button, !running);
        enable_window(self.ntfs_button, !running);
        enable_window(self.fallback_button, !running);
        enable_window(self.path_edit, !running);
        for button in &self.drive_buttons {
            enable_window(*button, !running);
        }
    }

    fn paint(&self) {
        // SAFETY: BeginPaint/EndPaint are paired for the current WM_PAINT message.
        unsafe {
            let mut ps = zeroed();
            let hdc = windows_sys::Win32::Graphics::Gdi::BeginPaint(self.hwnd, &mut ps);
            self.draw(hdc);
            windows_sys::Win32::Graphics::Gdi::EndPaint(self.hwnd, &ps);
        }
    }

    fn draw(&self, hdc: HDC) {
        let rect = self.client_rect();
        fill(hdc, rect, COLOR_BG);
        fill(
            hdc,
            RECT {
                left: 0,
                top: 0,
                right: rect.right,
                bottom: TOP_HEIGHT,
            },
            COLOR_TOP,
        );
        fill(
            hdc,
            RECT {
                left: 0,
                top: TOP_HEIGHT,
                right: RAIL_WIDTH,
                bottom: rect.bottom,
            },
            COLOR_RAIL,
        );
        fill(
            hdc,
            RECT {
                left: RAIL_WIDTH,
                top: TOP_HEIGHT,
                right: RAIL_WIDTH + 1,
                bottom: rect.bottom,
            },
            COLOR_LINE,
        );

        self.select_font(hdc);
        draw_text(
            hdc,
            rect_xy(18, 16, 220, 42),
            "DiskLoom",
            COLOR_TEXT,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        draw_text(
            hdc,
            rect_xy(136, 16, 360, 42),
            "See your disk clearly.",
            COLOR_MUTED,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        self.draw_rail(hdc);
        self.draw_main(hdc, rect);
    }

    fn draw_rail(&self, hdc: HDC) {
        draw_label(hdc, 18, 90, "Scan");
        draw_label(hdc, 18, 110, "Path");
        draw_label(hdc, 18, 166, "Scanner");
        draw_text(
            hdc,
            rect_xy(18, 318, RAIL_WIDTH - 18, 344),
            "Status",
            COLOR_TEXT,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        draw_text(
            hdc,
            rect_xy(18, 346, RAIL_WIDTH - 18, 372),
            &self.status,
            COLOR_MUTED,
            DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
        );

        let scanner = format!("Current: {}", self.scanner_mode.label());
        draw_text(
            hdc,
            rect_xy(18, 376, RAIL_WIDTH - 18, 402),
            &scanner,
            COLOR_MUTED,
            DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
        );

        match &self.state {
            UiState::Idle => {
                draw_text(
                    hdc,
                    rect_xy(18, 426, RAIL_WIDTH - 18, 454),
                    "Ready for direct NTFS or fallback scan",
                    COLOR_MUTED,
                    DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
                );
            }
            UiState::Scanning(progress) => {
                let text = progress.map_or_else(
                    || "Starting scan".to_owned(),
                    |progress| {
                        format!(
                            "{} entries, {} files, {} dirs, {} ms",
                            format_count(progress.summary.entries),
                            format_count(progress.summary.files),
                            format_count(progress.summary.directories),
                            progress.elapsed_ms
                        )
                    },
                );
                draw_text(
                    hdc,
                    rect_xy(18, 426, RAIL_WIDTH - 18, 454),
                    &text,
                    COLOR_WARN,
                    DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
                );
            }
            UiState::Complete(result) => {
                draw_metric(hdc, 18, 420, "Size", &format_bytes(result.total_size));
                draw_metric(
                    hdc,
                    18,
                    450,
                    "Allocated",
                    &format_bytes(result.total_allocated),
                );
                draw_metric(
                    hdc,
                    18,
                    480,
                    "Entries",
                    &format_count(result.summary.entries),
                );
                draw_metric(hdc, 18, 510, "Files", &format_count(result.summary.files));
                draw_metric(
                    hdc,
                    18,
                    540,
                    "Dirs",
                    &format_count(result.summary.directories),
                );
            }
            UiState::Error(error) => {
                draw_text(
                    hdc,
                    rect_xy(18, 426, RAIL_WIDTH - 18, 486),
                    error,
                    COLOR_ERROR,
                    DT_LEFT | DT_END_ELLIPSIS,
                );
            }
        }
    }

    fn draw_main(&self, hdc: HDC, client: RECT) {
        let left = RAIL_WIDTH + 1;
        fill(
            hdc,
            RECT {
                left,
                top: TOP_HEIGHT,
                right: client.right,
                bottom: client.bottom,
            },
            COLOR_BG,
        );

        let title = match self.active_tab {
            ActiveTab::Tree => "Tree",
            ActiveTab::Files => "Files",
            ActiveTab::Types => "Types",
            ActiveTab::Treemap => "Treemap",
        };
        draw_text(
            hdc,
            rect_xy(
                left + 24,
                TOP_HEIGHT + 58,
                client.right - 24,
                TOP_HEIGHT + 86,
            ),
            title,
            COLOR_TEXT,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );

        match &self.state {
            UiState::Idle => draw_empty_state(hdc, left, client.right, client.bottom),
            UiState::Scanning(progress) => {
                draw_scanning_state(hdc, left, client.right, client.bottom, *progress)
            }
            UiState::Error(error) => {
                draw_error_state(hdc, left, client.right, client.bottom, error)
            }
            UiState::Complete(result) => self.draw_result(hdc, client, result),
        }
    }

    fn draw_result(&self, hdc: HDC, client: RECT, result: &ScanResult) {
        let left = RAIL_WIDTH + 25;
        let right = client.right - 24;
        let summary = format!(
            "{} entries, {} files, {} dirs, {} ms, {}",
            format_count(result.summary.entries),
            format_count(result.summary.files),
            format_count(result.summary.directories),
            result.elapsed_ms,
            result.scanner_label
        );
        draw_text(
            hdc,
            rect_xy(left, TOP_HEIGHT + 92, right, TOP_HEIGHT + 118),
            &summary,
            COLOR_MUTED,
            DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
        );
        if let Some(reason) = &result.fallback_reason {
            let fallback = format!("Fallback reason: {reason}");
            draw_text(
                hdc,
                rect_xy(left, TOP_HEIGHT + 116, right, TOP_HEIGHT + 142),
                &fallback,
                COLOR_WARN,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
        }

        match self.active_tab {
            ActiveTab::Tree => self.draw_tree_table(hdc, client, result),
            ActiveTab::Files => self.draw_files_table(hdc, client, result),
            ActiveTab::Types => self.draw_types_table(hdc, client, result),
            ActiveTab::Treemap => self.draw_treemap(hdc, client, result),
        }
    }

    fn draw_tree_table(&self, hdc: HDC, client: RECT, result: &ScanResult) {
        let table = table_rect(client);
        draw_table_header(hdc, table, ["Name", "Size", "Allocated", "Kind"]);
        let visible = visible_rows(table);
        for (screen_idx, row) in result
            .tree_rows
            .iter()
            .skip(self.scroll_top)
            .take(visible)
            .enumerate()
        {
            let top = table.top + ROW_HEIGHT * (screen_idx as i32 + 1);
            draw_row_bg(hdc, table.left, table.right, top, screen_idx);
            let name = result.graph.name(row.id).unwrap_or_default();
            let indented = format!("{}{}", "  ".repeat(row.depth.min(8)), name);
            draw_text(
                hdc,
                rect_xy(table.left + 10, top, table.right - 310, top + ROW_HEIGHT),
                &indented,
                COLOR_TEXT,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
            draw_text(
                hdc,
                rect_xy(table.right - 300, top, table.right - 205, top + ROW_HEIGHT),
                &format_bytes(row.size),
                COLOR_TEXT,
                DT_RIGHT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
            draw_text(
                hdc,
                rect_xy(table.right - 195, top, table.right - 88, top + ROW_HEIGHT),
                &format_bytes(row.allocated),
                COLOR_MUTED,
                DT_RIGHT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
            draw_text(
                hdc,
                rect_xy(table.right - 76, top, table.right - 12, top + ROW_HEIGHT),
                row.kind,
                COLOR_MUTED,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
        }
    }

    fn draw_files_table(&self, hdc: HDC, client: RECT, result: &ScanResult) {
        let table = table_rect(client);
        draw_table_header(hdc, table, ["Path", "Size", "Allocated", ""]);
        let visible = visible_rows(table);
        for (screen_idx, row) in result
            .file_rows
            .iter()
            .skip(self.scroll_top)
            .take(visible)
            .enumerate()
        {
            let top = table.top + ROW_HEIGHT * (screen_idx as i32 + 1);
            draw_row_bg(hdc, table.left, table.right, top, screen_idx);
            let path = result
                .graph
                .reconstruct_path(row.id)
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            draw_text(
                hdc,
                rect_xy(table.left + 10, top, table.right - 250, top + ROW_HEIGHT),
                &path,
                COLOR_TEXT,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
            draw_text(
                hdc,
                rect_xy(table.right - 238, top, table.right - 128, top + ROW_HEIGHT),
                &format_bytes(row.size),
                COLOR_TEXT,
                DT_RIGHT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
            draw_text(
                hdc,
                rect_xy(table.right - 118, top, table.right - 12, top + ROW_HEIGHT),
                &format_bytes(row.allocated),
                COLOR_MUTED,
                DT_RIGHT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
        }
    }

    fn draw_types_table(&self, hdc: HDC, client: RECT, result: &ScanResult) {
        let table = table_rect(client);
        draw_table_header(hdc, table, ["Type", "Files", "Size", "Allocated"]);
        let visible = visible_rows(table);
        for (screen_idx, row) in result
            .type_rows
            .iter()
            .skip(self.scroll_top)
            .take(visible)
            .enumerate()
        {
            let top = table.top + ROW_HEIGHT * (screen_idx as i32 + 1);
            draw_row_bg(hdc, table.left, table.right, top, screen_idx);
            draw_text(
                hdc,
                rect_xy(table.left + 10, top, table.right - 360, top + ROW_HEIGHT),
                &row.extension,
                COLOR_TEXT,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
            draw_text(
                hdc,
                rect_xy(table.right - 350, top, table.right - 250, top + ROW_HEIGHT),
                &format_count(row.files),
                COLOR_TEXT,
                DT_RIGHT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
            draw_text(
                hdc,
                rect_xy(table.right - 238, top, table.right - 128, top + ROW_HEIGHT),
                &format_bytes(row.size),
                COLOR_TEXT,
                DT_RIGHT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
            draw_text(
                hdc,
                rect_xy(table.right - 118, top, table.right - 12, top + ROW_HEIGHT),
                &format_bytes(row.allocated),
                COLOR_MUTED,
                DT_RIGHT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
            );
        }
    }

    fn draw_treemap(&self, hdc: HDC, client: RECT, result: &ScanResult) {
        let bounds = RECT {
            left: RAIL_WIDTH + 25,
            top: TOP_HEIGHT + 146,
            right: client.right - 24,
            bottom: client.bottom - 24,
        };
        fill(hdc, bounds, COLOR_PANEL);
        let rects = layout_treemap(
            &result.treemap_items,
            TreemapBounds {
                x: bounds.left as f32,
                y: bounds.top as f32,
                width: (bounds.right - bounds.left).max(0) as f32,
                height: (bounds.bottom - bounds.top).max(0) as f32,
            },
        );
        let palette = [
            rgb(92, 142, 124),
            rgb(122, 116, 178),
            rgb(177, 126, 76),
            rgb(78, 135, 172),
            rgb(170, 92, 110),
        ];
        for (idx, item) in rects.iter().enumerate() {
            let rect = RECT {
                left: item.bounds.x as i32 + 1,
                top: item.bounds.y as i32 + 1,
                right: (item.bounds.x + item.bounds.width) as i32 - 1,
                bottom: (item.bounds.y + item.bounds.height) as i32 - 1,
            };
            if rect.right <= rect.left || rect.bottom <= rect.top {
                continue;
            }
            fill(hdc, rect, palette[idx % palette.len()]);
            if rect.right - rect.left > 96 && rect.bottom - rect.top > 26 {
                draw_text(
                    hdc,
                    inset_rect(rect, 6, 4),
                    &item.label,
                    COLOR_TEXT,
                    DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS,
                );
            }
        }
    }

    fn on_mouse_wheel(&mut self, wparam: WPARAM) {
        let delta = ((wparam >> 16) & 0xffff) as i16;
        let step = 4;
        if delta < 0 {
            self.scroll_top = self.scroll_top.saturating_add(step);
        } else {
            self.scroll_top = self.scroll_top.saturating_sub(step);
        }
        self.invalidate();
    }

    fn path_text(&self) -> String {
        window_text(self.path_edit)
    }

    fn set_path_text(&self, value: &str) {
        let wide = wide_null(value);
        // SAFETY: The edit control handle is valid and the UTF-16 buffer is null-terminated.
        unsafe {
            let _ = SetWindowTextW(self.path_edit, wide.as_ptr());
        }
    }

    fn client_rect(&self) -> RECT {
        // SAFETY: `hwnd` is a valid window once painting/layout is reached.
        unsafe {
            let mut rect = zeroed();
            let _ = GetClientRect(self.hwnd, &mut rect);
            rect
        }
    }

    fn select_font(&self, hdc: HDC) {
        // SAFETY: `font` is a valid stock font and `hdc` is valid during painting/control color.
        unsafe {
            let _ = SelectObject(hdc, self.font as _);
        }
    }

    fn invalidate(&self) {
        // SAFETY: `hwnd` is a valid window handle after creation. Null RECT invalidates all.
        unsafe {
            let _ = InvalidateRect(self.hwnd, null(), TRUE);
        }
    }
}

fn message_loop() -> Result<()> {
    loop {
        // SAFETY: `msg` is valid writable storage for GetMessageW.
        let mut msg: MSG = unsafe { zeroed() };
        // SAFETY: Standard Win32 message loop for all windows on the current thread.
        let code = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
        if code == -1 {
            bail!("message loop failed: {}", std::io::Error::last_os_error());
        }
        if code == 0 {
            break;
        }
        // SAFETY: Message was returned by GetMessageW and can be translated/dispatched.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: During WM_NCCREATE, lparam points to CREATESTRUCTW supplied by Windows.
        let create = unsafe { &*(lparam as *const CREATESTRUCTW) };
        let app = create.lpCreateParams.cast::<NativeApp>();
        if !app.is_null() {
            // SAFETY: `app` points to the boxed NativeApp passed to CreateWindowExW.
            unsafe {
                (*app).hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
            }
        }
        return TRUE as LRESULT;
    }

    let app = app_from_window(hwnd);
    if let Some(app) = app {
        match message {
            WM_CREATE => {
                if let Err(error) = app.create_controls() {
                    app.state = UiState::Error(error.to_string());
                }
                0
            }
            WM_SIZE => {
                app.layout_controls();
                0
            }
            WM_COMMAND => {
                app.on_command(low_word(wparam));
                0
            }
            WM_SCAN_MESSAGE => {
                app.receive_scan();
                0
            }
            WM_START_SCAN => {
                app.start_scan();
                0
            }
            WM_MOUSEWHEEL => {
                app.on_mouse_wheel(wparam);
                0
            }
            WM_ERASEBKGND => 1,
            WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                let hdc = wparam as HDC;
                // SAFETY: Control color messages provide a valid HDC for immediate setup.
                unsafe {
                    let _ = SetTextColor(hdc, COLOR_TEXT);
                    let _ = SetBkColor(hdc, COLOR_CONTROL);
                    let _ = SetBkMode(hdc, TRANSPARENT as i32);
                }
                app.control_brush as isize
            }
            WM_PAINT => {
                app.paint();
                0
            }
            WM_DESTROY => {
                // SAFETY: Posts quit for the current UI thread.
                unsafe {
                    PostQuitMessage(0);
                }
                0
            }
            _ => {
                // SAFETY: Forward unhandled messages to the default window procedure.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
    } else {
        // SAFETY: No app pointer is available before WM_NCCREATE; default handling is required.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}

fn app_from_window(hwnd: HWND) -> Option<&'static mut NativeApp> {
    // SAFETY: User data is set to a valid NativeApp pointer in WM_NCCREATE and remains alive
    // until the message loop exits.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut NativeApp };
    if ptr.is_null() {
        None
    } else {
        // SAFETY: See the note above; all access happens on the UI thread.
        Some(unsafe { &mut *ptr })
    }
}

fn post_scan_message(hwnd: isize) {
    // SAFETY: The HWND value was captured from the UI thread. If the window is already gone,
    // PostMessageW fails harmlessly and the result is ignored.
    unsafe {
        let _ = PostMessageW(hwnd as HWND, WM_SCAN_MESSAGE, 0, 0);
    }
}

fn move_window(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) {
    if hwnd.is_null() {
        return;
    }
    // SAFETY: The child window handle is valid after control creation.
    unsafe {
        let _ = MoveWindow(hwnd, x, y, width, height, TRUE);
    }
}

fn enable_window(hwnd: HWND, enabled: bool) {
    if hwnd.is_null() {
        return;
    }
    // SAFETY: The child window handle is valid after control creation.
    unsafe {
        let _ = EnableWindow(hwnd, i32::from(enabled));
    }
}

fn window_text(hwnd: HWND) -> String {
    if hwnd.is_null() {
        return String::new();
    }
    // SAFETY: The edit control handle is valid and Windows returns the text length.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buffer = vec![0_u16; len as usize + 1];
    // SAFETY: `buffer` is writable and includes room for the terminating NUL.
    let written = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..written.max(0) as usize])
}

fn low_word(value: WPARAM) -> u16 {
    (value & 0xffff) as u16
}

fn button_style() -> u32 {
    BS_PUSHBUTTON as u32 | WS_TABSTOP
}

fn fill(hdc: HDC, rect: RECT, color: u32) {
    // SAFETY: The HDC is valid during painting and the brush is deleted after use.
    unsafe {
        let brush = CreateSolidBrush(color);
        if !brush.is_null() {
            let _ = FillRect(hdc, &rect, brush);
            let _ = DeleteObject(brush);
        }
    }
}

fn draw_text(hdc: HDC, mut rect: RECT, text: &str, color: u32, flags: u32) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: `hdc` is valid during painting and `wide` points to UTF-16 text for this call.
    unsafe {
        let _ = SetTextColor(hdc, color);
        let _ = SetBkMode(hdc, TRANSPARENT as i32);
        let _ = DrawTextW(hdc, wide.as_ptr(), wide.len() as i32, &mut rect, flags);
    }
}

fn draw_label(hdc: HDC, x: i32, y: i32, text: &str) {
    draw_text(
        hdc,
        rect_xy(x, y, RAIL_WIDTH - 18, y + 22),
        text,
        COLOR_MUTED,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
}

fn draw_metric(hdc: HDC, x: i32, y: i32, label: &str, value: &str) {
    draw_text(
        hdc,
        rect_xy(x, y, x + 92, y + 24),
        label,
        COLOR_MUTED,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
    draw_text(
        hdc,
        rect_xy(x + 94, y, RAIL_WIDTH - 18, y + 24),
        value,
        COLOR_TEXT,
        DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
    );
}

fn draw_empty_state(hdc: HDC, left: i32, right: i32, bottom: i32) {
    let rect = RECT {
        left: left + 24,
        top: TOP_HEIGHT + 148,
        right: right - 24,
        bottom: bottom - 24,
    };
    fill(hdc, rect, COLOR_PANEL);
    draw_text(
        hdc,
        inset_rect(rect, 18, 16),
        "Choose a drive or path, then scan.",
        COLOR_MUTED,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
}

fn draw_scanning_state(
    hdc: HDC,
    left: i32,
    right: i32,
    bottom: i32,
    progress: Option<UiScanProgress>,
) {
    let rect = RECT {
        left: left + 24,
        top: TOP_HEIGHT + 148,
        right: right - 24,
        bottom: bottom - 24,
    };
    fill(hdc, rect, COLOR_PANEL);
    let text = progress.map_or_else(
        || "Starting scan".to_owned(),
        |progress| {
            format!(
                "{} entries, {} files, {} directories, {} ms",
                format_count(progress.summary.entries),
                format_count(progress.summary.files),
                format_count(progress.summary.directories),
                progress.elapsed_ms
            )
        },
    );
    draw_text(
        hdc,
        inset_rect(rect, 18, 16),
        &text,
        COLOR_WARN,
        DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
    );
}

fn draw_error_state(hdc: HDC, left: i32, right: i32, bottom: i32, error: &str) {
    let rect = RECT {
        left: left + 24,
        top: TOP_HEIGHT + 148,
        right: right - 24,
        bottom: bottom - 24,
    };
    fill(hdc, rect, COLOR_PANEL);
    draw_text(
        hdc,
        inset_rect(rect, 18, 16),
        error,
        COLOR_ERROR,
        DT_LEFT | DT_END_ELLIPSIS,
    );
}

fn table_rect(client: RECT) -> RECT {
    RECT {
        left: RAIL_WIDTH + 25,
        top: TOP_HEIGHT + 146,
        right: client.right - 24,
        bottom: client.bottom - 24,
    }
}

fn draw_table_header(hdc: HDC, rect: RECT, labels: [&str; 4]) {
    fill(
        hdc,
        RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.top + ROW_HEIGHT,
        },
        COLOR_PANEL_ALT,
    );
    draw_text(
        hdc,
        rect_xy(
            rect.left + 10,
            rect.top,
            rect.right - 360,
            rect.top + ROW_HEIGHT,
        ),
        labels[0],
        COLOR_MUTED,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
    draw_text(
        hdc,
        rect_xy(
            rect.right - 350,
            rect.top,
            rect.right - 250,
            rect.top + ROW_HEIGHT,
        ),
        labels[1],
        COLOR_MUTED,
        DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
    );
    draw_text(
        hdc,
        rect_xy(
            rect.right - 238,
            rect.top,
            rect.right - 128,
            rect.top + ROW_HEIGHT,
        ),
        labels[2],
        COLOR_MUTED,
        DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
    );
    draw_text(
        hdc,
        rect_xy(
            rect.right - 118,
            rect.top,
            rect.right - 12,
            rect.top + ROW_HEIGHT,
        ),
        labels[3],
        COLOR_MUTED,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
}

fn draw_row_bg(hdc: HDC, left: i32, right: i32, top: i32, idx: usize) {
    let color = if idx.is_multiple_of(2) {
        COLOR_PANEL
    } else {
        COLOR_BG
    };
    fill(
        hdc,
        RECT {
            left,
            top,
            right,
            bottom: top + ROW_HEIGHT,
        },
        color,
    );
}

fn visible_rows(rect: RECT) -> usize {
    ((rect.bottom - rect.top - ROW_HEIGHT).max(0) / ROW_HEIGHT) as usize
}

fn rect_xy(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

fn inset_rect(rect: RECT, x: i32, y: i32) -> RECT {
    RECT {
        left: rect.left + x,
        top: rect.top + y,
        right: rect.right - x,
        bottom: rect.bottom - y,
    }
}

fn graph_totals(graph: &FileGraph) -> (u64, u64) {
    graph
        .ids()
        .filter_map(|id| {
            let entry = graph.entry(id)?;
            if entry.parent.is_some() {
                return None;
            }
            let stats = graph.stats(id)?;
            Some((stats.total_size.bytes(), stats.total_allocated.bytes()))
        })
        .fold(
            (0_u64, 0_u64),
            |(size, allocated), (next_size, next_allocated)| {
                (
                    size.saturating_add(next_size),
                    allocated.saturating_add(next_allocated),
                )
            },
        )
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit_idx = 0;
    while value >= 1024.0 && unit_idx + 1 < UNITS.len() {
        value /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit_idx])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit_idx])
    } else {
        format!("{value:.2} {}", UNITS[unit_idx])
    }
}

fn format_count(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::with_capacity(text.len() + text.len() / 3);
    for (idx, ch) in text.chars().rev().enumerate() {
        if idx > 0 && idx.is_multiple_of(3) {
            output.push(',');
        }
        output.push(ch);
    }
    output.chars().rev().collect()
}

fn discover_volume_shortcuts() -> Vec<VolumeShortcut> {
    discover_volumes()
        .unwrap_or_default()
        .into_iter()
        .map(|volume| {
            let is_ntfs = volume.kind == VolumeKind::Ntfs;
            let drive = volume.root.trim_end_matches('\\');
            let label = match volume.kind {
                VolumeKind::Ntfs => format!("{drive} NTFS"),
                VolumeKind::Other(name) if !name.is_empty() => format!("{drive} {name}"),
                VolumeKind::Other(_) | VolumeKind::Unknown => drive.to_owned(),
            };
            VolumeShortcut {
                root: volume.root,
                label,
                is_ntfs,
            }
        })
        .collect()
}

fn default_scan_path_from(system_drive: Option<&str>, volumes: &[VolumeShortcut]) -> String {
    if let Some(root) = system_drive.and_then(normalize_drive_root) {
        return volumes
            .iter()
            .find(|volume| volume.root.eq_ignore_ascii_case(&root))
            .map(|volume| volume.root.clone())
            .unwrap_or(root);
    }

    if let Some(volume) = volumes.iter().find(|volume| volume.is_ntfs) {
        return volume.root.clone();
    }

    volumes
        .first()
        .map(|volume| volume.root.clone())
        .unwrap_or_else(|| ".".to_owned())
}

fn normalize_drive_root(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches(['\\', '/']);
    let mut chars = trimmed.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next() != Some(':') || chars.next().is_some() {
        return None;
    }

    Some(format!("{}:\\", letter.to_ascii_uppercase()))
}

fn scan_path(
    path: PathBuf,
    mode: UiScannerMode,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<UiScanOutcome, String> {
    match mode {
        UiScannerMode::Fallback => scan_fallback(path, None, cancel, on_progress),
        UiScannerMode::Ntfs => scan_ntfs(&path, cancel, on_progress),
        UiScannerMode::Auto => {
            if drive_volume(&path).is_some() {
                match scan_ntfs(&path, cancel, on_progress) {
                    Ok(outcome) => Ok(outcome),
                    Err(error) => scan_fallback(path, Some(error), cancel, on_progress),
                }
            } else {
                scan_fallback(path, None, cancel, on_progress)
            }
        }
    }
}

fn scan_fallback(
    path: PathBuf,
    fallback_reason: Option<String>,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<UiScanOutcome, String> {
    let (graph, summary) = FallbackScanner::scan_with_control(
        ScanOptions {
            root: path,
            follow_symlinks: false,
        },
        UI_PROGRESS_EVERY,
        |summary| {
            on_progress(summary);
            if cancel.load(Ordering::Relaxed) {
                ScanControl::Cancel
            } else {
                ScanControl::Continue
            }
        },
    )
    .map_err(|error| error.to_string())?;

    Ok(UiScanOutcome {
        graph,
        summary,
        scanner_label: "fallback traversal",
        fallback_reason,
    })
}

fn scan_ntfs(
    path: &Path,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(ScanSummary),
) -> Result<UiScanOutcome, String> {
    let volume = drive_volume(path).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let graph = NtfsScanner::scan_volume_with_control(&volume, UI_PROGRESS_EVERY, |progress| {
        on_progress(scan_summary_from_ntfs_progress(progress));
        if cancel.load(Ordering::Relaxed) {
            NtfsScanControl::Cancel
        } else {
            NtfsScanControl::Continue
        }
    })
    .map_err(|error| error.to_string())?;
    let summary = summary_from_graph(&graph);

    Ok(UiScanOutcome {
        graph,
        summary,
        scanner_label: "direct NTFS MFT",
        fallback_reason: None,
    })
}

fn scan_summary_from_ntfs_progress(progress: NtfsScanProgress) -> ScanSummary {
    ScanSummary {
        entries: progress.entries,
        inaccessible: progress.skipped,
        directories: progress.directories,
        files: progress.files,
    }
}

fn summary_from_graph(graph: &FileGraph) -> ScanSummary {
    let mut summary = ScanSummary {
        entries: graph.len() as u64,
        ..ScanSummary::default()
    };

    for id in graph.ids() {
        let Some(entry) = graph.entry(id) else {
            continue;
        };
        if entry.flags.contains(EntryFlags::DIRECTORY) {
            summary.directories += 1;
        } else {
            summary.files += 1;
        }
    }

    summary
}

fn drive_volume(path: &Path) -> Option<String> {
    let value = path.to_string_lossy();
    let trimmed = value.trim_end_matches(['\\', '/']);
    let mut chars = trimmed.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next()? != ':' || chars.next().is_some() {
        return None;
    }

    Some(format!("{}:", letter.to_ascii_uppercase()))
}

fn maybe_relaunch_ui_scan_elevated(
    path: &Path,
    scanner_mode: UiScannerMode,
) -> Result<bool, String> {
    if !ui_scan_needs_elevation(path, scanner_mode) || !should_request_elevation()? {
        return Ok(false);
    }

    let args = [
        "--path".to_owned(),
        path.to_string_lossy().into_owned(),
        "--scanner".to_owned(),
        scanner_mode.arg_value().to_owned(),
        "--scan".to_owned(),
    ];
    relaunch_current_process_elevated(args)
        .map_err(|error| format!("failed to request administrator access: {error}"))?;
    Ok(true)
}

fn ui_scan_needs_elevation(path: &Path, scanner_mode: UiScannerMode) -> bool {
    scanner_mode != UiScannerMode::Fallback && drive_volume(path).is_some()
}

fn should_request_elevation() -> Result<bool, String> {
    is_process_elevated()
        .map(|is_elevated| !is_elevated)
        .map_err(|error| format!("failed to check administrator elevation: {error}"))
}

fn tree_rows_from_graph(graph: &FileGraph, limit: usize) -> Vec<TreeRow> {
    if limit == 0 {
        return Vec::new();
    }

    let mut child_pairs = Vec::with_capacity(graph.len().saturating_sub(1));
    let mut roots = Vec::new();

    for id in graph.ids() {
        let Some(entry) = graph.entry(id) else {
            continue;
        };
        if let Some(parent) = entry.parent {
            child_pairs.push((parent, id));
        } else {
            roots.push(id);
        }
    }

    sort_entry_ids_by_total_size(graph, &mut roots);
    child_pairs.sort_by(|(left_parent, left_child), (right_parent, right_child)| {
        left_parent
            .0
            .cmp(&right_parent.0)
            .then_with(|| compare_entry_ids_by_total_size(graph, left_child, right_child))
    });

    let mut child_ids = Vec::with_capacity(child_pairs.len());
    let mut child_ranges = vec![ChildRange::default(); graph.len()];
    let mut pair_idx = 0;
    while pair_idx < child_pairs.len() {
        let parent = child_pairs[pair_idx].0;
        let start = child_ids.len();
        while pair_idx < child_pairs.len() && child_pairs[pair_idx].0 == parent {
            child_ids.push(child_pairs[pair_idx].1);
            pair_idx += 1;
        }
        child_ranges[parent.0 as usize] = ChildRange {
            start: start as u32,
            len: (child_ids.len() - start) as u32,
        };
    }

    let mut rows = Vec::with_capacity(limit.min(graph.len()));
    for root in roots {
        append_tree_rows(graph, &child_ids, &child_ranges, root, 0, limit, &mut rows);
        if rows.len() >= limit {
            break;
        }
    }

    rows
}

fn append_tree_rows(
    graph: &FileGraph,
    child_ids: &[EntryId],
    child_ranges: &[ChildRange],
    id: EntryId,
    depth: usize,
    limit: usize,
    rows: &mut Vec<TreeRow>,
) {
    if rows.len() >= limit {
        return;
    }

    if let Some(row) = tree_row_from_graph(graph, id, depth) {
        rows.push(row);
    }

    let Some(range) = child_ranges.get(id.0 as usize).copied() else {
        return;
    };
    let start = range.start as usize;
    let end = start
        .saturating_add(range.len as usize)
        .min(child_ids.len());
    for child in &child_ids[start..end] {
        append_tree_rows(
            graph,
            child_ids,
            child_ranges,
            *child,
            depth + 1,
            limit,
            rows,
        );
        if rows.len() >= limit {
            break;
        }
    }
}

fn tree_row_from_graph(graph: &FileGraph, id: EntryId, depth: usize) -> Option<TreeRow> {
    let stats = graph.stats(id)?;
    let entry = graph.entry(id)?;
    let kind = if entry.flags.contains(EntryFlags::DIRECTORY) {
        "dir"
    } else if entry.flags.contains(EntryFlags::SYMLINK) {
        "link"
    } else {
        "file"
    };

    Some(TreeRow {
        id,
        depth,
        kind,
        size: stats.total_size.bytes(),
        allocated: stats.total_allocated.bytes(),
    })
}

fn file_rows_from_graph(graph: &FileGraph, limit: usize) -> Vec<FileRow> {
    let ids = top_entries_by_own_size(
        graph,
        graph.ids().filter(|id| {
            let Some(stats) = graph.stats(*id) else {
                return false;
            };
            let Some(entry) = graph.entry(*id) else {
                return false;
            };
            !entry.flags.contains(EntryFlags::DIRECTORY) && stats.own_size.bytes() > 0
        }),
        limit,
    );

    ids.into_iter()
        .filter_map(|id| {
            let stats = graph.stats(id)?;
            Some(FileRow {
                id,
                size: stats.own_size.bytes(),
                allocated: stats.own_allocated.bytes(),
            })
        })
        .collect()
}

fn treemap_items_from_graph(graph: &FileGraph, limit: usize) -> Vec<TreemapItem> {
    let ids = top_entries_by_own_size(
        graph,
        graph.ids().filter(|id| {
            let Some(stats) = graph.stats(*id) else {
                return false;
            };
            let Some(entry) = graph.entry(*id) else {
                return false;
            };
            !entry.flags.contains(EntryFlags::DIRECTORY) && stats.own_size.bytes() > 0
        }),
        limit,
    );

    ids.into_iter()
        .filter_map(|id| {
            let stats = graph.stats(id)?;
            Some(TreemapItem {
                id,
                label: graph.name(id).unwrap_or_default().to_owned(),
                size: stats.own_size.bytes(),
            })
        })
        .collect()
}

fn sort_entry_ids_by_total_size(graph: &FileGraph, ids: &mut [EntryId]) {
    ids.sort_by(|left, right| compare_entry_ids_by_total_size(graph, left, right));
}

fn compare_entry_ids_by_total_size(
    graph: &FileGraph,
    left: &EntryId,
    right: &EntryId,
) -> std::cmp::Ordering {
    let left_size = graph
        .stats(*left)
        .map_or(0, |stats| stats.total_size.bytes());
    let right_size = graph
        .stats(*right)
        .map_or(0, |stats| stats.total_size.bytes());
    right_size.cmp(&left_size).then_with(|| {
        let left_name = graph.name(*left).unwrap_or_default();
        let right_name = graph.name(*right).unwrap_or_default();
        left_name.cmp(right_name)
    })
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use diskloom_core::{FileGraph, FileGraphBuilder, FileKind};

    use super::{
        VolumeShortcut, default_scan_path_from, file_rows_from_graph, format_bytes, format_count,
        normalize_drive_root, tree_rows_from_graph, ui_scan_needs_elevation,
    };

    fn sample_graph() -> FileGraph {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let big = builder
            .add_entry(Some(root), "big", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(big), "large.bin", FileKind::File, 100, 128, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "small.bin", FileKind::File, 10, 16, 0)
            .unwrap();
        builder.finish()
    }

    #[test]
    fn format_bytes_should_scale_binary_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
    }

    #[test]
    fn format_count_should_group_thousands() {
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn normalize_drive_root_should_accept_drive_roots() {
        assert_eq!(normalize_drive_root("c:").as_deref(), Some("C:\\"));
        assert_eq!(normalize_drive_root("d:\\").as_deref(), Some("D:\\"));
        assert_eq!(normalize_drive_root("e:/").as_deref(), Some("E:\\"));
        assert_eq!(normalize_drive_root("c:\\Users"), None);
    }

    #[test]
    fn default_scan_path_should_prefer_system_drive() {
        let volumes = [
            volume("D:\\", false),
            volume("C:\\", true),
            volume("E:\\", true),
        ];

        assert_eq!(default_scan_path_from(Some("c:"), &volumes), "C:\\");
    }

    #[test]
    fn ui_scan_needs_elevation_should_match_direct_drive_scans_only() {
        assert!(ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            super::UiScannerMode::Auto
        ));
        assert!(ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            super::UiScannerMode::Ntfs
        ));
        assert!(!ui_scan_needs_elevation(
            std::path::Path::new("C:\\"),
            super::UiScannerMode::Fallback
        ));
        assert!(!ui_scan_needs_elevation(
            std::path::Path::new("C:\\Users"),
            super::UiScannerMode::Auto
        ));
    }

    #[test]
    fn tree_rows_should_sort_children_by_total_size() {
        let graph = sample_graph();

        let rows = tree_rows_from_graph(&graph, 10);

        assert_eq!(graph.name(rows[1].id), Some("big"));
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn file_rows_should_sort_by_own_size() {
        let graph = sample_graph();

        let rows = file_rows_from_graph(&graph, 10);

        assert_eq!(graph.name(rows[0].id), Some("large.bin"));
    }

    fn volume(root: &str, is_ntfs: bool) -> VolumeShortcut {
        VolumeShortcut {
            root: root.to_owned(),
            label: root.to_owned(),
            is_ntfs,
        }
    }
}
