// src/shell/windows_gui.rs – Windows Native GUI (Phase 7+)
// Implements main window, Rich Edit, Rich Edit terminal, Button, flicker‑free resizing,
// drag‑and‑drop, file I/O, dark theme, menu bar, output batching, LSP integration,
// diagnostics, Run button, accelerator table (F5 works; Ctrl+O/S/Q now handled
// via subclass), DWM theming, full debug logging, and a status bar that shows
// a native progress bar and a separate status label.
//
// Status bar behaviour:
//   - Start: "0% - Ready" (label) + empty progress bar
//   - During compilation: phase name and increasing percentage
//   - End: "100% - Compilation complete" + full progress bar
//
// Auto‑scroll: terminal stays at the bottom unless the user scrolls up.
// Uses append‑only text insertion with EM_REPLACESEL and pending‑refresh coalescing.
//
// Clipboard support:
//   - Editor: Ctrl+C (copy), Ctrl+V (paste), Ctrl+X (cut), Ctrl+A (select all)
//   - Terminal: Ctrl+C (copy) – copies whole terminal if no selection
//
// Build actions: Build Debug, Build Release, Check, Test, Clean – implemented via
// threaded calls to runner.rs functions.
//
// Performance: Refresh rate limited to 200ms; terminal truncates to 5000 lines
// using in‑place deletion. Buffer stealing ensures compiler never blocks.
// Full logs can be saved to disk; the GUI shows only the tail.

#![allow(
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unsafe_op_in_unsafe_fn,
    unreachable_patterns
)]

use std::ffi::{OsStr, c_void};
use std::fs;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::lsp::{LspClient, WM_USER_DIAGNOSTICS, path_to_uri};
use super::runner::{build_file, check_file, clean_project, compile_and_run_file, run_tests};
use super::terminal::TerminalBuffer;
use crate::diagnostic::{
    Diagnostic, Level, WM_USER_PHASE_UPDATE, WM_USER_REFRESH, emit_phase_update, get_gui_hwnd,
    set_gui_hwnd, set_gui_phase_callback, set_gui_terminal,
};
use crate::find_vox_root;
use crate::{CacheConfig, host_triple};

// ============================================================================
// Performance tuning constants – scaled for large projects
// ============================================================================

/// Minimum time between terminal refreshes (200ms = 5 Hz)
const REFRESH_INTERVAL_MS: u64 = 200;

/// Maximum lines kept in the Rich Edit control (scrollback limit)
const UI_MAX_LINES: isize = 5000;

// ============================================================================
// Trace flags – set to true for debugging
// ============================================================================

const AUTO_SCROLL_TRACE: bool = false; // Enable to see scroll logs
const GUI_TRACE: bool = true; // Enable GUI trace logs (set to true for debugging)
const DRAG_DROP_TRACE: bool = false; // Enable detailed drag‑and‑drop tracing

// ============================================================================
// Wide string macro
// ============================================================================

macro_rules! w {
    ($s:expr) => {{
        const W: &[u16] = &{
            let mut buf = [0u16; $s.len() + 1];
            let mut i = 0;
            while i < $s.len() {
                buf[i] = $s.as_bytes()[i] as u16;
                i += 1;
            }
            buf[$s.len()] = 0;
            buf
        };
        W.as_ptr()
    }};
}

// ============================================================================
// Win32 Types & Constants
// ============================================================================

type HWND = *mut c_void;
type HINSTANCE = *mut c_void;
type HMODULE = *mut c_void;
type HICON = *mut c_void;
type HCURSOR = *mut c_void;
type HBRUSH = *mut c_void;
type HFONT = *mut c_void;
type HMENU = *mut c_void;
type HDC = *mut c_void;
type LPARAM = isize;
type WPARAM = usize;
type LRESULT = isize;
type UINT = u32;
type LONG = i32;
type DWORD = u32;
type BOOL = i32;
type LPCWSTR = *const u16;
type LPWSTR = *mut u16;
type LPVOID = *mut c_void;
type ATOM = u16;
type WNDPROC = Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>;
type HRESULT = i32;
type HACCEL = *mut c_void;
type LPCSTR = *const i8;

// MessageBox constants
const MB_YESNOCANCEL: UINT = 0x00000003;
const MB_ICONQUESTION: UINT = 0x00000020;
const MB_ICONERROR: UINT = 0x00000010;
const IDYES: i32 = 6;
const IDNO: i32 = 7;
const IDCANCEL: i32 = 2;

// Window styles
const WS_OVERLAPPEDWINDOW: DWORD = 0x00CF0000;
const WS_CLIPCHILDREN: DWORD = 0x02000000;
const WS_CHILD: DWORD = 0x40000000;
const WS_VISIBLE: DWORD = 0x10000000;
const WS_VSCROLL: DWORD = 0x00200000;
const WS_HSCROLL: DWORD = 0x00100000;
// WS_BORDER removed – we use a flat, borderless look
const WS_CAPTION: DWORD = 0x00C00000;
const WS_SYSMENU: DWORD = 0x00080000;
const WS_THICKFRAME: DWORD = 0x00040000;
const WS_MINIMIZEBOX: DWORD = 0x00020000;
const WS_MAXIMIZEBOX: DWORD = 0x00010000;
const WS_POPUP: DWORD = 0x80000000;

const WS_EX_ACCEPTFILES: DWORD = 0x00000010;

// Static control styles
const SS_LEFT: DWORD = 0x00000000;

// Progress bar styles
const PBS_SMOOTH: DWORD = 0x00000001;

// Common control classes
const PROGRESS_CLASS: &str = "msctls_progress32\0";

// Progress bar messages
const PBM_SETPOS: UINT = WM_USER + 2;

// Common control initialisation flags
const ICC_PROGRESS_CLASS: DWORD = 0x00000020;

// Edit control styles
const ES_MULTILINE: DWORD = 0x0004;
const ES_AUTOVSCROLL: DWORD = 0x0040;
const ES_AUTOHSCROLL: DWORD = 0x0080;
const ES_NOHIDESEL: DWORD = 0x0100;
const ES_SAVESEL: DWORD = 0x8000;
const ES_READONLY: DWORD = 0x0800;

const BS_PUSHBUTTON: DWORD = 0x00000000;

// Clipboard constants
const CF_TEXT: u32 = 1;
const GMEM_MOVEABLE: u32 = 0x0002;

// Edit control messages for clipboard
const WM_COPY: UINT = 0x0301;
const WM_PASTE: UINT = 0x0302;
const WM_CUT: UINT = 0x0300;
const WM_CLEAR: UINT = 0x0303;

const WM_CREATE: UINT = 0x0001;
const WM_DESTROY: UINT = 0x0002;
const WM_SIZE: UINT = 0x0005;
const WM_COMMAND: UINT = 0x0111;
const WM_DROPFILES: UINT = 0x0233;
const WM_CTLCOLORSTATIC: UINT = 0x0138;
const WM_CTLCOLOREDIT: UINT = 0x0133;
const WM_CTLCOLORBTN: UINT = 0x0135;
const WM_ERASEBKGND: UINT = 0x0014;
const WM_USER: UINT = 0x0400;
const WM_SETTEXT: UINT = 0x000C;
const WM_GETTEXTLENGTH: UINT = 0x000E;
const WM_KEYDOWN: UINT = 0x0100;
const WM_SYSKEYDOWN: UINT = 0x0104;
const WM_CHAR: UINT = 0x0102;
const WM_KEYUP: UINT = 0x0101;
const WM_TIMER: UINT = 0x0113; // added for throttling
const WM_INITMENUPOPUP: UINT = 0x0117; // for updating menu states
const WM_CLOSE: UINT = 0x0010;
const WM_CONTEXTMENU: UINT = 0x007B;

// Custom messages
// WM_USER_REFRESH is now imported from diagnostic.rs
// WM_USER_DIAGNOSTICS is defined in lsp.rs as WM_USER + 2
// WM_USER_PHASE_UPDATE is defined in diagnostic.rs as WM_USER + 3

// Undocumented UAH messages for dark menu bar painting
const WM_UAHDRAWMENU: UINT = 0x0091;
const WM_UAHDRAWMENUITEM: UINT = 0x0092;

// OBJID_MENU = -3
const OBJID_MENU: i32 = -3;

const EM_LIMITTEXT: UINT = 0x00C5;
const EM_SETBKGNDCOL: UINT = WM_USER + 67; // EM_SETBKGNDCOLOR
const EM_SETCHARFORMAT: UINT = WM_USER + 68;
const EM_GETCHARFORMAT: UINT = WM_USER + 69;
const EM_CHARFROMPOS: UINT = 0x00D7;
const EM_SETSEL: UINT = 0x00B1;
const EM_GETSEL: UINT = 0x00B0;
const EM_EXSETSEL: UINT = 0x0437;
const EM_REPLACESEL: UINT = 0x00C2;
const EM_SCROLLCARET: UINT = 0x00B7;
const EM_GETLINECOUNT: UINT = 0x00BA;
const EM_GETFIRSTVISIBLELINE: UINT = 0x00CE;
const EM_LINEINDEX: UINT = 0x00BB;
const EM_LINELENGTH: UINT = 0x00C1;
const EM_SETTEXTEX: UINT = WM_USER + 97; // Rich Edit specific
const EM_LINESCROLL: UINT = 0x00B6; // used to preserve scroll position
const EM_UNDO: UINT = 0x00C7;
const EM_REDO: UINT = 0x00C8;
const EM_CANUNDO: UINT = 0x00C6;
const EM_CANREDO: UINT = 0x00C9;
const EM_POSFROMCHAR: UINT = 0x00D6; // get position of a character

// Advanced typography options
const EM_SETTYPOGRAPHYOPTIONS: UINT = WM_USER + 202;
const TO_ADVANCEDTYPOGRAPHY: DWORD = 0x0001;

// Button notification
const BN_CLICKED: UINT = 0;

const CS_HREDRAW: UINT = 0x0002;
const CS_VREDRAW: UINT = 0x0001;

const GWLP_USERDATA: i32 = -21;
const GWLP_WNDPROC: i32 = -4;

const SWP_NOMOVE: UINT = 0x0002;
const SWP_NOZORDER: UINT = 0x0004;
const SWP_NOACTIVATE: UINT = 0x0010;

const DRAGQUERYFILE: UINT = 0xFFFFFFFF;

const COLOR_WINDOW: i32 = 5;
const COLOR_WINDOWTEXT: i32 = 8;

const IDC_ARROW: i32 = 32512;
const IDI_APPLICATION: i32 = 32512;

const MF_STRING: UINT = 0x0000;
const MF_SEPARATOR: UINT = 0x0800;
const MF_POPUP: UINT = 0x0010;
const MF_ENABLED: UINT = 0x0000;
const MF_GRAYED: UINT = 0x0001;
const MF_DISABLED: UINT = 0x0002;

const TPM_RIGHTBUTTON: UINT = 0x0002;
const TPM_LEFTALIGN: UINT = 0x0000;
const TPM_RETURNCMD: UINT = 0x0100;

// Menu IDs (now also used as accelerator command IDs)
const ID_FILE_OPEN: u16 = 1001;
const ID_FILE_SAVE: u16 = 1002;
const ID_FILE_EXIT: u16 = 1003;
const ID_RUN: u16 = 1004;
// New build action IDs
const ID_BUILD_DEBUG: u16 = 1005;
const ID_BUILD_RELEASE: u16 = 1006;
const ID_CLEAN: u16 = 1007;
const ID_TEST: u16 = 1008;
const ID_CHECK: u16 = 1009;
// Edit menu IDs
const ID_EDIT_UNDO: u16 = 2001;
const ID_EDIT_REDO: u16 = 2002;
const ID_EDIT_CUT: u16 = 2003;
const ID_EDIT_COPY: u16 = 2004;
const ID_EDIT_PASTE: u16 = 2005;
const ID_EDIT_DELETE: u16 = 2006;
const ID_EDIT_SELECT_ALL: u16 = 2007;

// Accelerator flags (virtual key codes) - no longer used, but kept for constants
const FVIRTKEY: u8 = 0x01;
const FCONTROL: u8 = 0x02;
const FSHIFT: u8 = 0x04;
const FALT: u8 = 0x08;
const VK_F5: u16 = 0x74;
const VK_O: u16 = 0x4F;
const VK_S: u16 = 0x53;
const VK_Q: u16 = 0x51;
const VK_C: u16 = 0x43;
const VK_V: u16 = 0x56;
const VK_X: u16 = 0x58;
const VK_A: u16 = 0x41;
const VK_Z: u16 = 0x5A;
const VK_Y: u16 = 0x59;
const VK_B: u16 = 0x42;
const VK_T: u16 = 0x54;
const VK_L: u16 = 0x4C;
const VK_CONTROL: i32 = 0x11;
const VK_SHIFT: i32 = 0x10;

// DWM attribute constants
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
const DWMWCP_ROUND: u32 = 2;
const DWMSBT_MAINWINDOW: u32 = 1;

// Older dark mode attribute (some builds need both)
const DWMWA_USE_IMMERSIVE_DARK_MODE_OLD: u32 = 19;
const DWMWA_CAPTION_COLOR: u32 = 35;

// CHARFORMAT2W masks and effects
const CFM_COLOR: u32 = 0x40000000;
const CFM_UNDERLINE: u32 = 0x00000004;
const CFE_UNDERLINE: u32 = 0x00000004;
const CFM_UNDERLINETYPE: u32 = 0x00800000;
const CFE_UNDERLINETYPE: u32 = 0x00800000;
const SCF_DEFAULT: WPARAM = 0x0000;
const SCF_SELECTION: WPARAM = 0x0001;
const SCF_WORD: WPARAM = 0x0002;
const SCF_ALL: WPARAM = 0x0004;

// Tooltip constants
const TOOLTIPS_CLASS: &str = "tooltips_class32\0";
const TTS_ALWAYSTIP: DWORD = 0x01;
const TTF_SUBCLASS: u32 = 0x0010;
const TTF_IDISHWND: u32 = 0x0001;
const TTF_CENTERTIP: u32 = 0x0002;
const TTM_ADDTOOL: UINT = WM_USER + 50;
const TTM_DELTOOL: UINT = WM_USER + 51;
const TTM_NEWTOOLRECT: UINT = WM_USER + 52;
const TTM_GETTOOLINFO: UINT = WM_USER + 53;
const TTM_SETTOOLINFO: UINT = WM_USER + 54;
const TTM_TRACKACTIVATE: UINT = WM_USER + 17;
const TTM_TRACKPOSITION: UINT = WM_USER + 18;
const TTM_UPDATETIPTEXT: UINT = WM_USER + 57;

// File dialog constants
const OFN_FILEMUSTEXIST: DWORD = 0x00001000;
const OFN_HIDEREADONLY: DWORD = 0x00000004;
const OFN_PATHMUSTEXIST: DWORD = 0x00000800;

// Scrollbar constants
const SB_VERT: i32 = 1;
const SIF_ALL: UINT = 0x0007;

// UIPI bypass constants
const MSGFLT_ALLOW: DWORD = 1;
const WM_COPYGLOBALDATA: UINT = 0x0049;
const WM_COPYDATA: UINT = 0x004A;

// Font and margin constants
const FW_NORMAL: i32 = 400;
const DEFAULT_CHARSET: DWORD = 1;
const OUT_DEFAULT_PRECIS: DWORD = 0;
const CLIP_DEFAULT_PRECIS: DWORD = 0;
const CLEARTYPE_QUALITY: DWORD = 5;
const FIXED_PITCH: DWORD = 1;
const FF_MODERN: DWORD = 48;
const EM_SETMARGINS: UINT = 0x00D3;
const EC_LEFTMARGIN: usize = 0x0001;
const EC_RIGHTMARGIN: usize = 0x0002;

// Drawing flags for DrawText
const DT_SINGLELINE: UINT = 0x0020;
const DT_CENTER: UINT = 0x0001;
const DT_VCENTER: UINT = 0x0004;
const DT_LEFT: UINT = 0x0000;

// ============================================================================
// FFI Declarations
// ============================================================================

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(lpModuleName: LPCWSTR) -> HINSTANCE;
    fn LoadLibraryA(lpLibFileName: LPCSTR) -> HMODULE;
    fn GetProcAddress(hModule: HMODULE, lpProcName: LPCSTR) -> *const c_void;
    fn LoadLibraryW(lpLibFileName: LPCWSTR) -> HMODULE;
    fn MultiByteToWideChar(
        CodePage: UINT,
        dwFlags: DWORD,
        lpMultiByteStr: *const i8,
        cbMultiByte: i32,
        lpWideCharStr: LPWSTR,
        cchWideChar: i32,
    ) -> i32;
    fn WideCharToMultiByte(
        CodePage: UINT,
        dwFlags: DWORD,
        lpWideCharStr: LPCWSTR,
        cchWideChar: i32,
        lpMultiByteStr: *mut i8,
        cbMultiByte: i32,
        lpDefaultChar: *const i8,
        lpUsedDefaultChar: *mut i32,
    ) -> i32;
    fn GetLastError() -> DWORD;
    fn GetTickCount() -> DWORD;
    fn GlobalAlloc(uFlags: u32, dwBytes: usize) -> *mut c_void;
    fn GlobalLock(hMem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(hMem: *mut c_void) -> BOOL;
    fn GlobalFree(hMem: *mut c_void) -> *mut c_void;
    fn GetConsoleWindow() -> HWND;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(lpWndClass: *const WNDCLASSEXW) -> ATOM;
    fn CreateWindowExW(
        dwExStyle: DWORD,
        lpClassName: LPCWSTR,
        lpWindowName: LPCWSTR,
        dwStyle: DWORD,
        x: i32,
        y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HMENU,
        hInstance: HINSTANCE,
        lpParam: LPVOID,
    ) -> HWND;
    fn DefWindowProcW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    fn PostQuitMessage(nExitCode: i32);
    fn GetMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: UINT, wMsgFilterMax: UINT) -> BOOL;
    fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    fn SetWindowTextW(hWnd: HWND, lpString: LPCWSTR) -> BOOL;
    fn GetWindowTextW(hWnd: HWND, lpString: LPWSTR, nMaxCount: i32) -> i32;
    fn GetWindowTextLengthW(hWnd: HWND) -> i32;
    fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;
    fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;
    fn BeginDeferWindowPos(nAmount: i32) -> *mut c_void;
    fn FreeConsole() -> BOOL;
    fn DeferWindowPos(
        hWinPosInfo: *mut c_void,
        hWnd: HWND,
        hWndInsertAfter: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        uFlags: UINT,
    ) -> *mut c_void;
    fn EndDeferWindowPos(hWinPosInfo: *mut c_void) -> BOOL;
    fn SetFocus(hWnd: HWND) -> HWND;
    fn GetFocus() -> HWND;
    fn GetClassNameW(hWnd: HWND, lpClassName: LPWSTR, nMaxCount: i32) -> i32;
    fn SendMessageW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
    fn LoadCursorW(hInstance: HINSTANCE, lpCursorName: LPCWSTR) -> HCURSOR;
    fn LoadIconW(hInstance: HINSTANCE, lpIconName: LPCWSTR) -> HICON;
    fn CreateMenu() -> HMENU;
    fn CreatePopupMenu() -> HMENU;
    fn AppendMenuW(hMenu: HMENU, uFlags: UINT, uIDNewItem: usize, lpNewItem: LPCWSTR) -> BOOL;
    fn DestroyMenu(hMenu: HMENU) -> BOOL;
    fn GetMenu(hWnd: HWND) -> HMENU;
    fn SetMenu(hWnd: HWND, hMenu: HMENU) -> BOOL;
    fn DrawMenuBar(hWnd: HWND) -> BOOL;
    fn FindWindowExW(
        hWndParent: HWND,
        hWndChildAfter: HWND,
        lpszClass: LPCWSTR,
        lpszWindow: LPCWSTR,
    ) -> HWND;
    fn PostMessageW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> BOOL;
    fn SetWindowSubclass(
        hWnd: HWND,
        pfnSubclass: usize,
        uIdSubclass: usize,
        dwRefData: usize,
    ) -> BOOL;
    fn RemoveWindowSubclass(hWnd: HWND, pfnSubclass: usize, uIdSubclass: usize) -> BOOL;
    fn DefSubclassProc(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    fn GetClientRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
    fn ClientToScreen(hWnd: HWND, lpPoint: *mut POINT) -> BOOL;
    fn SetWindowPos(
        hWnd: HWND,
        hWndInsertAfter: HWND,
        X: i32,
        Y: i32,
        cx: i32,
        cy: i32,
        uFlags: UINT,
    ) -> BOOL;
    fn GetParent(hWnd: HWND) -> HWND;
    fn CreateAcceleratorTableW(lpaccl: *const ACCEL, cEntries: i32) -> HACCEL;
    fn DestroyAcceleratorTable(hAccel: HACCEL) -> BOOL;
    fn TranslateAcceleratorW(hWnd: HWND, hAccTable: HACCEL, lpMsg: *const MSG) -> BOOL;
    fn GetKeyState(nVirtKey: i32) -> i16;
    fn OpenClipboard(hWndNewOwner: HWND) -> BOOL;
    fn CloseClipboard() -> BOOL;
    fn EmptyClipboard() -> BOOL;
    fn SetClipboardData(uFormat: UINT, hMem: *mut c_void) -> *mut c_void;
    fn GetClipboardData(uFormat: UINT) -> *mut c_void;
    fn UpdateWindow(hWnd: HWND) -> BOOL;
    fn RedrawWindow(hWnd: HWND, lprcUpdate: *const RECT, hrgnUpdate: HRGN, uFlags: UINT) -> BOOL;
    fn InvalidateRect(hWnd: HWND, lpRect: *const RECT, bErase: BOOL);
    fn GetScrollInfo(hWnd: HWND, fnBar: i32, lpsi: *mut SCROLLINFO) -> BOOL;
    // Added for timer-based throttling
    fn SetTimer(hWnd: HWND, nIDEvent: usize, uElapse: u32, lpTimerFunc: *const c_void) -> usize;
    fn KillTimer(hWnd: HWND, uIDEvent: usize) -> BOOL;
    // UIPI bypass
    fn ChangeWindowMessageFilterEx(
        hwnd: HWND,
        message: UINT,
        action: DWORD,
        pChangeFilterStruct: *mut c_void,
    ) -> BOOL;
    // Menu state management
    fn EnableMenuItem(hMenu: HMENU, uIDEnableItem: u32, uEnable: UINT) -> BOOL;
    // MessageBox
    fn MessageBoxW(hWnd: HWND, lpText: LPCWSTR, lpCaption: LPCWSTR, uType: UINT) -> i32;
    // TrackPopupMenu
    fn TrackPopupMenu(
        hMenu: HMENU,
        uFlags: UINT,
        x: i32,
        y: i32,
        nReserved: i32,
        hWnd: HWND,
        prcRect: *const RECT,
    ) -> BOOL;
    // GetCursorPos
    fn GetCursorPos(lpPoint: *mut POINT) -> BOOL;
    // Menu bar drawing helpers (UAH)
    fn GetWindowRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
    fn GetMenuBarInfo(hWnd: HWND, idObject: i32, idItem: i32, pmbi: *mut MENUBARINFO) -> BOOL;
    fn GetMenuStringW(
        hMenu: HMENU,
        uIDItem: u32,
        lpString: LPWSTR,
        nMaxCount: i32,
        uFlags: UINT,
    ) -> i32;
    fn DrawTextW(hdc: HDC, lpchText: LPCWSTR, cchText: i32, lprc: *mut RECT, format: UINT) -> i32;
}

// Additional GDI types
type HRGN = *mut c_void;

// RedrawWindow flags
const RDW_INVALIDATE: UINT = 0x0001;
const RDW_ERASE: UINT = 0x0004;
const RDW_UPDATENOW: UINT = 0x0100;
const RDW_ALLCHILDREN: UINT = 0x0080;
const RDW_FRAME: UINT = 0x0400; // redraw non-client area

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragAcceptFiles(hWnd: HWND, fAccept: BOOL);
    fn DragQueryFileW(hDrop: *mut c_void, iFile: UINT, lpszFile: LPWSTR, cch: UINT) -> UINT;
    fn DragFinish(hDrop: *mut c_void);
}

#[link(name = "comctl32")]
unsafe extern "system" {
    fn InitCommonControlsEx(lpInitCtrls: *const INITCOMMONCONTROLSEX) -> BOOL;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateSolidBrush(crColor: u32) -> HBRUSH;
    fn CreateFontW(
        nHeight: i32,
        nWidth: i32,
        nEscapement: i32,
        nOrientation: i32,
        fnWeight: i32,
        bItalic: DWORD,
        bUnderline: DWORD,
        bStrikeOut: DWORD,
        iCharSet: DWORD,
        iOutPrecision: DWORD,
        iClipPrecision: DWORD,
        iQuality: DWORD,
        iPitchAndFamily: DWORD,
        lpszFace: LPCWSTR,
    ) -> HFONT;
    fn DeleteObject(hObject: *mut c_void) -> BOOL;
    fn SetTextColor(hdc: HDC, color: u32) -> u32;
    fn SetBkColor(hdc: HDC, color: u32) -> u32;
    fn SelectObject(hdc: HDC, hObject: *mut c_void) -> *mut c_void;
    fn FillRect(hdc: HDC, lprc: *const RECT, hbr: HBRUSH) -> i32;
    fn SetBkMode(hdc: HDC, mode: i32) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn RevokeDragDrop(hwnd: HWND) -> HRESULT;
    fn OleInitialize(pvReserved: *mut c_void) -> HRESULT;
    fn OleUninitialize();
}

#[link(name = "comdlg32")]
unsafe extern "system" {
    fn GetOpenFileNameW(lpofn: *mut OPENFILENAMEW) -> BOOL;
}

#[link(name = "uxtheme")]
unsafe extern "system" {
    fn SetWindowTheme(hwnd: HWND, pszSubAppName: LPCWSTR, pszSubIdList: LPCWSTR) -> HRESULT;
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: HWND,
        dwAttribute: u32,
        pvAttribute: *const c_void,
        cbAttribute: u32,
    ) -> HRESULT;
}

// ============================================================================
// Dark mode menu helper (undocumented API)
// ============================================================================

type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;

/// Enables dark mode for native Win32 menus by calling the undocumented
/// `SetPreferredAppMode` (ordinal 135) from uxtheme.dll.
/// This only affects dropdown menus, not the horizontal menu bar.
unsafe fn enable_dark_mode_menus() {
    let uxtheme = LoadLibraryA(b"uxtheme.dll\0".as_ptr() as *const i8);
    if uxtheme.is_null() {
        return;
    }

    // Ordinal 135: SetPreferredAppMode
    let proc_135 = GetProcAddress(uxtheme, 135 as *const i8);
    if !proc_135.is_null() {
        let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 =
            mem::transmute(proc_135);
        set_preferred_app_mode(2); // 2 = Force Dark
    }

    // Ordinal 136: FlushMenuThemes (makes dropdowns update)
    let proc_136 = GetProcAddress(uxtheme, 136 as *const i8);
    if !proc_136.is_null() {
        let flush_menu_themes: unsafe extern "system" fn() = mem::transmute(proc_136);
        flush_menu_themes();
    }
}

// ============================================================================
// Undocumented UAH structures for dark menu bar painting
// ============================================================================

#[repr(C)]
struct DRAWITEMSTRUCT {
    pub CtlType: u32,
    pub CtlID: u32,
    pub itemID: u32,
    pub itemAction: u32,
    pub itemState: u32,
    pub hwndItem: HWND,
    pub hDC: HDC,
    pub rcItem: RECT,
    pub itemData: usize,
}

#[repr(C)]
struct UAHMENU {
    pub hmenu: isize, // HMENU
    pub hdc: isize,   // HDC
    pub dwFlags: u32,
}

#[repr(C)]
struct UAHMENUITEMMETRICS {
    pub cx: u32,
    pub cy: u32,
}

#[repr(C)]
struct UAHMENUPOPUPMETRICS {
    pub rgcx: [u32; 4],
    pub fUpdateMaxWidths: u32,
}

#[repr(C)]
struct UAHMENUITEM {
    pub iPosition: i32,
    pub umim: UAHMENUITEMMETRICS,
    pub umpm: UAHMENUPOPUPMETRICS,
}

#[repr(C)]
struct UAHDRAWMENUITEM {
    pub dis: DRAWITEMSTRUCT,
    pub um: UAHMENU,
    pub umi: UAHMENUITEM,
}

#[repr(C)]
struct MENUBARINFO {
    pub cbSize: u32,
    pub rcBar: RECT,
    pub hMenu: HMENU,
    pub hwndMenu: HWND,
    pub fBarFocused: BOOL,
    pub fFocused: BOOL,
}

// ============================================================================
// Structures for FFI
// ============================================================================

#[repr(C)]
struct WNDCLASSEXW {
    cbSize: UINT,
    style: UINT,
    lpfnWndProc: WNDPROC,
    cbClsExtra: i32,
    cbWndExtra: i32,
    hInstance: HINSTANCE,
    hIcon: HICON,
    hCursor: HCURSOR,
    hbrBackground: HBRUSH,
    lpszMenuName: LPCWSTR,
    lpszClassName: LPCWSTR,
    hIconSm: HICON,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: UINT,
    wParam: WPARAM,
    lParam: LPARAM,
    time: DWORD,
    pt: POINT,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct POINT {
    x: LONG,
    y: LONG,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct RECT {
    left: LONG,
    top: LONG,
    right: LONG,
    bottom: LONG,
}

#[repr(C)]
struct INITCOMMONCONTROLSEX {
    dwSize: DWORD,
    dwICC: DWORD,
}

#[repr(C)]
struct CHARFORMAT2W {
    cbSize: u32,
    dwMask: u32,
    dwEffects: u32,
    yHeight: i32,
    yOffset: i32,
    crTextColor: u32,
    bCharSet: u8,
    bPitchAndFamily: u8,
    szFaceName: [u16; 32],
    wWeight: u16,
    sSpacing: i16,
    crBackColor: u32,
    lcid: u32,
    dwReserved: u32,
    sStyle: i16,
    wKerning: u16,
    bUnderlineType: u8,
    bAnimation: u8,
    bRevAuthor: u8,
    bReserved1: u8,
}

#[repr(C)]
struct SETTEXTEX {
    flags: DWORD,
    codepage: DWORD,
}

const ST_DEFAULT: DWORD = 0x0000;

#[repr(C)]
struct CHARRANGE {
    cpMin: i32,
    cpMax: i32,
}

#[repr(C)]
struct TOOLINFO {
    cbSize: u32,
    uFlags: u32,
    hwnd: HWND,
    uId: usize,
    rect: RECT,
    hinst: HINSTANCE,
    lpszText: LPCWSTR,
    lParam: LPARAM,
}

#[repr(C)]
struct OPENFILENAMEW {
    lStructSize: DWORD,
    hwndOwner: HWND,
    hInstance: HINSTANCE,
    lpstrFilter: LPCWSTR,
    lpstrCustomFilter: LPWSTR,
    nMaxCustFilter: DWORD,
    nFilterIndex: DWORD,
    lpstrFile: LPWSTR,
    nMaxFile: DWORD,
    lpstrFileTitle: LPWSTR,
    nMaxFileTitle: DWORD,
    lpstrInitialDir: LPCWSTR,
    lpstrTitle: LPCWSTR,
    Flags: DWORD,
    nFileOffset: u16,
    nFileExtension: u16,
    lpstrDefExt: LPCWSTR,
    lCustData: LPARAM,
    lpfnHook: usize,
    lpTemplateName: LPCWSTR,
    pvReserved: *mut c_void,
    dwReserved: DWORD,
    FlagsEx: DWORD,
}

#[repr(C)]
struct ACCEL {
    fVirt: u8,
    key: u16,
    cmd: u16,
}

#[repr(C)]
struct SCROLLINFO {
    cbSize: UINT,
    fMask: UINT,
    nMin: i32,
    nMax: i32,
    nPage: UINT,
    nPos: i32,
    nTrackPos: i32,
}

const ICC_STANDARD_CLASSES: DWORD = 0x00004000;
const MSFTEDIT_CLASS: &str = "RICHEDIT50W\0";
const RICHEDIT_DLL: &str = "Msftedit.dll\0";

// ============================================================================
// App State
// ============================================================================

struct AppState {
    hwnd_main: HWND,
    hwnd_editor: HWND,
    hwnd_terminal: HWND,
    hwnd_button: HWND,
    hwnd_status_text: HWND, // Static text for phase/percent
    hwnd_progress: HWND,    // Progress bar
    hFont: HFONT,
    hBrush: HBRUSH,
    hbrStatusBk: HBRUSH, // Background brush for status label
    hbrEditBk: HBRUSH,   // Background brush for edit controls
    hbrButtonBk: HBRUSH, // Background brush for button (if needed)
    file_path: Option<String>,
    is_modified: bool,
    terminal: Arc<Mutex<TerminalBuffer>>,
    last_refresh_time: Instant,
    pending_refresh: bool,    // if true, a refresh is already queued
    processing_refresh: bool, // reentrancy guard
    lsp_client: Option<LspClient>,
    diagnostics: Vec<Diagnostic>,
    last_change_time: Instant,
    pending_change: bool,
    tooltip_hwnd: HWND,
    old_editor_proc: Option<WNDPROC>,
    old_terminal_proc: Option<WNDPROC>,
    compilation_in_progress: bool, // track if a build is running
    was_at_bottom: bool,           // saved scroll state before text replacement
    is_test_run: bool,             // <-- NEW: true while a test run is active
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            hwnd_main: ptr::null_mut(),
            hwnd_editor: ptr::null_mut(),
            hwnd_terminal: ptr::null_mut(),
            hwnd_button: ptr::null_mut(),
            hwnd_status_text: ptr::null_mut(),
            hwnd_progress: ptr::null_mut(),
            hFont: ptr::null_mut(),
            hBrush: ptr::null_mut(),
            hbrStatusBk: ptr::null_mut(),
            hbrEditBk: ptr::null_mut(),
            hbrButtonBk: ptr::null_mut(),
            file_path: None,
            is_modified: false,
            terminal: Arc::new(Mutex::new(TerminalBuffer::new())),
            last_refresh_time: Instant::now(),
            pending_refresh: false,
            processing_refresh: false,
            lsp_client: None,
            diagnostics: Vec::new(),
            last_change_time: Instant::now(),
            pending_change: false,
            tooltip_hwnd: ptr::null_mut(),
            old_editor_proc: None,
            old_terminal_proc: None,
            compilation_in_progress: false,
            was_at_bottom: false,
            is_test_run: false, // <-- NEW
        }
    }
}

// ============================================================================
// Debug logging macro – now routes through diagnostic::debug_log
// ============================================================================

macro_rules! debug_log {
    ($($arg:tt)*) => {
        if crate::diagnostic::global_debug() {
            crate::diagnostic::debug_log(format!($($arg)*));
        }
    };
}

// ============================================================================
// Helper: Wide string conversion
// ============================================================================

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

// ============================================================================
// Progress bar helpers
// ============================================================================

/// Parse a phase message of the form "phase (percent%)" into (phase, percent).
/// If parsing fails, returns (msg, 0).
fn parse_phase_percent(msg: &str) -> (&str, usize) {
    if let Some(start) = msg.rfind('(') {
        if let Some(end) = msg.rfind(')') {
            if start < end {
                let phase = &msg[..start].trim();
                let percent_str = &msg[start + 1..end];
                if let Ok(percent) = percent_str.trim_end_matches('%').parse::<usize>() {
                    return (phase, percent);
                }
            }
        }
    }
    // Fallback: treat whole as phase, percent 0
    (msg.trim(), 0)
}

// ============================================================================
// Auto‑scroll helpers (Rich Edit specific) – now uses GetScrollInfo
// ============================================================================

/// Returns true if the vertical scroll thumb is at the bottom.
unsafe fn is_scroll_at_bottom(hwnd: HWND) -> bool {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_ALL,
        nMin: 0,
        nMax: 0,
        nPage: 0,
        nPos: 0,
        nTrackPos: 0,
    };
    if GetScrollInfo(hwnd, SB_VERT, &mut si) == 0 {
        // Fallback to old method if GetScrollInfo fails
        let total_lines = SendMessageW(hwnd, EM_GETLINECOUNT, 0, 0) as i32;
        if total_lines <= 1 {
            return true;
        }
        let first_visible = SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, 0, 0) as i32;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetClientRect(hwnd, &mut rect) == 0 {
            return first_visible >= total_lines - 3;
        }
        const LINE_HEIGHT: i32 = 20;
        let visible_lines = (rect.bottom - rect.top) / LINE_HEIGHT;
        if visible_lines <= 0 {
            return first_visible >= total_lines - 3;
        }
        first_visible + visible_lines >= total_lines - 3
    } else {
        // Use scroll info
        if si.nMax == 0 || si.nPage == 0 {
            return true;
        }
        let max_scroll = si.nMax - si.nPage as i32;
        let result = si.nPos >= max_scroll - 5;
        if AUTO_SCROLL_TRACE {
            println!(
                "[AUTO-SCROLL] is_scroll_at_bottom: si.nPos={}, max_scroll={}, result={}",
                si.nPos, max_scroll, result
            );
        }
        result
    }
}

// ============================================================================
// ANSI color parser and renderer
// ============================================================================

/// Parse ANSI escape sequences and apply color formatting to the terminal.
/// This is a simplified parser – it handles common SGR codes (30-37, 90-97, 0).
/// Returns a vector of (text, color) pairs.
fn parse_ansi_and_split(text: &str) -> Vec<(String, u32)> {
    let mut result = Vec::new();
    let mut current_color: u32 = 0x00FFFFFF; // default white
    let mut buf = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Flush buffer with current color
            if !buf.is_empty() {
                result.push((buf.clone(), current_color));
                buf.clear();
            }
            // Expect '['
            if let Some('[') = chars.peek() {
                chars.next(); // consume '['
                // Read parameters until a letter
                let mut params = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphabetic() {
                        let command = c;
                        chars.next(); // consume command
                        // Process command
                        if command == 'm' {
                            // SGR
                            for p in params.split(';') {
                                if let Ok(val) = p.parse::<u32>() {
                                    match val {
                                        0 => current_color = 0x00FFFFFF, // reset to white
                                        // Standard colors (30-37)
                                        30 => current_color = 0x00000000, // black
                                        31 => current_color = 0x000000FF, // red
                                        32 => current_color = 0x0000FF00, // green
                                        33 => current_color = 0x0000FFFF, // yellow
                                        34 => current_color = 0x00FF0000, // blue
                                        35 => current_color = 0x00FF00FF, // magenta
                                        36 => current_color = 0x00FFFF00, // cyan
                                        37 => current_color = 0x00FFFFFF, // white
                                        // Bright colors (90-97)
                                        90 => current_color = 0x00808080, // bright black (gray)
                                        91 => current_color = 0x000000FF, // bright red
                                        92 => current_color = 0x0000FF00, // bright green
                                        93 => current_color = 0x0000FFFF, // bright yellow
                                        94 => current_color = 0x00FF0000, // bright blue
                                        95 => current_color = 0x00FF00FF, // bright magenta
                                        96 => current_color = 0x00FFFF00, // bright cyan
                                        97 => current_color = 0x00FFFFFF, // bright white
                                        _ => {}
                                    }
                                }
                            }
                        }
                        break;
                    } else {
                        params.push(c);
                        chars.next();
                    }
                }
            } else {
                // Not a CSI, treat as literal
                buf.push(ch);
                // Continue; will be flushed later
            }
        } else {
            buf.push(ch);
        }
    }
    // Flush remaining
    if !buf.is_empty() {
        result.push((buf, current_color));
    }
    result
}

// ============================================================================
// New append‑only text renderer – optimized
// ============================================================================

/// Append ANSI‑colored text to the terminal using EM_REPLACESEL.
/// This preserves the scroll position and is much faster than full replacement.
unsafe fn apply_terminal_colored_text_append(hwnd_terminal: HWND, text: &str) {
    if AUTO_SCROLL_TRACE {
        println!(
            "[AUTO-SCROLL] apply_terminal_colored_text_append: text_len={}",
            text.len()
        );
    }

    // Stop redrawing to prevent flicker
    SendMessageW(hwnd_terminal, 0x000B /* WM_SETREDRAW */, 0, 0);

    // Ensure advanced typography is enabled
    SendMessageW(
        hwnd_terminal,
        EM_SETTYPOGRAPHYOPTIONS,
        TO_ADVANCEDTYPOGRAPHY as WPARAM,
        TO_ADVANCEDTYPOGRAPHY as LPARAM,
    );

    let segments = parse_ansi_and_split(text);

    for (segment, color) in segments {
        // Always snap selection to the exact end of the text
        SendMessageW(hwnd_terminal, EM_SETSEL, usize::MAX, -1 as isize);

        let mut cf = CHARFORMAT2W {
            cbSize: mem::size_of::<CHARFORMAT2W>() as u32,
            dwMask: CFM_COLOR,
            dwEffects: 0,
            crTextColor: color,
            ..mem::zeroed()
        };

        SendMessageW(
            hwnd_terminal,
            EM_SETCHARFORMAT,
            SCF_SELECTION,
            &mut cf as *mut _ as isize,
        );

        let wide_seg = to_wide(&segment);
        // Replace selection (insert) the text with the active color
        SendMessageW(hwnd_terminal, EM_REPLACESEL, 0, wide_seg.as_ptr() as LPARAM);
    }

    // Leave caret at the end
    SendMessageW(hwnd_terminal, EM_SETSEL, usize::MAX, -1 as isize);

    // Resume redrawing – let the control repaint naturally, do not force immediate paint
    SendMessageW(hwnd_terminal, 0x000B /* WM_SETREDRAW */, 1, 0);

    // Invalidate the client area to trigger a repaint, but do not force synchronous update.
    // This avoids blocking the UI thread.
    InvalidateRect(hwnd_terminal, ptr::null(), 0);
}

// ============================================================================
// Editor white color helper
// ============================================================================

unsafe fn set_editor_white_color(hwnd_editor: HWND) {
    debug_log!("[GUI] Setting editor white color...");
    let mut cf = CHARFORMAT2W {
        cbSize: mem::size_of::<CHARFORMAT2W>() as u32,
        dwMask: CFM_COLOR,
        dwEffects: 0,
        crTextColor: 0x00FFFFFF,
        ..mem::zeroed()
    };
    SendMessageW(
        hwnd_editor,
        EM_SETCHARFORMAT,
        SCF_ALL,
        &mut cf as *mut _ as isize,
    );
    SendMessageW(
        hwnd_editor,
        EM_SETCHARFORMAT,
        SCF_DEFAULT,
        &mut cf as *mut _ as isize,
    );
}

// ============================================================================
// Load / Save functions
// ============================================================================

unsafe fn load_file(state: &mut AppState, path: &str) -> Result<(), String> {
    if DRAG_DROP_TRACE {
        println!("[DRAG_DROP] load_file: path={}", path);
    }
    let content = fs::read_to_string(path).map_err(|e| {
        if DRAG_DROP_TRACE {
            println!("[DRAG_DROP] load_file: read failed: {}", e);
        }
        e.to_string()
    })?;
    let len = content.len();
    if DRAG_DROP_TRACE {
        println!("[DRAG_DROP] load_file: read {} bytes", len);
    }

    let wide_content = to_wide(&content);
    let result = SetWindowTextW(state.hwnd_editor, wide_content.as_ptr());
    if DRAG_DROP_TRACE {
        println!("[DRAG_DROP] load_file: SetWindowTextW result={}", result);
    }

    state.file_path = Some(path.to_string());
    state.is_modified = false;

    set_editor_white_color(state.hwnd_editor);

    let title = format!(
        "vox - {}",
        Path::new(path).file_name().unwrap().to_string_lossy()
    );
    let wide_title = to_wide(&title);
    SetWindowTextW(state.hwnd_main, wide_title.as_ptr());

    if state.lsp_client.is_none() {
        start_lsp(state);
    } else if let Some(client) = &mut state.lsp_client {
        let uri = path_to_uri(Path::new(path));
        debug_log!("[GUI] Sending didOpen for {}", uri);
        let _ = client.send_open(&uri, &content);
    }

    Ok(())
}

unsafe fn save_file(state: &AppState) -> Result<(), String> {
    let path = state.file_path.as_ref().ok_or("No file open".to_string())?;
    debug_log!("[GUI] save_file: {}", path);
    let len = GetWindowTextLengthW(state.hwnd_editor);
    debug_log!("[GUI] save_file: text length = {}", len);
    if len == 0 {
        fs::write(path, "").map_err(|e| {
            debug_log!("[GUI] save_file: write failed: {}", e);
            e.to_string()
        })?;
        return Ok(());
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    GetWindowTextW(state.hwnd_editor, buf.as_mut_ptr(), len + 1);
    let wide_slice = &buf[..len as usize];
    let content = String::from_utf16_lossy(wide_slice);
    fs::write(path, content).map_err(|e| {
        debug_log!("[GUI] save_file: write failed: {}", e);
        e.to_string()
    })?;
    debug_log!("[GUI] save_file: saved successfully");
    Ok(())
}

// ============================================================================
// LSP integration helpers
// ============================================================================

unsafe fn start_lsp(state: &mut AppState) {
    debug_log!("[GUI] Starting LSP...");
    match LspClient::start(state.hwnd_main) {
        Ok(mut client) => {
            debug_log!("[GUI] LSP client started");
            let _ = client.send_initialize("file://");
            if let Some(path) = &state.file_path {
                let uri = path_to_uri(Path::new(path));
                let content = get_editor_text(state.hwnd_editor);
                debug_log!("[GUI] Sending didOpen for {}", uri);
                let _ = client.send_open(&uri, &content);
            }
            state.lsp_client = Some(client);
        }
        Err(e) => {
            debug_log!("[GUI] Failed to start LSP: {}", e);
            let msg = format!("Failed to start LSP: {}", e);
            push_output_line(&state.terminal, state.hwnd_main, msg);
        }
    }
}

unsafe fn get_editor_text(hwnd_editor: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd_editor);
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    GetWindowTextW(hwnd_editor, buf.as_mut_ptr(), len + 1);
    String::from_utf16_lossy(&buf[..len as usize])
}

// ============================================================================
// Output batching helpers (updated with pending‑refresh coalescing)
// ============================================================================

/// Append a line to the terminal buffer and request a refresh.
unsafe fn push_output_line(terminal: &Arc<Mutex<TerminalBuffer>>, hwnd: HWND, line: String) {
    {
        let mut term = terminal.lock().unwrap();
        term.push(line);
        if AUTO_SCROLL_TRACE {
            println!(
                "[AUTO-SCROLL] push_output_line: buffer now has {} lines",
                term.len()
            );
        }
    }
    request_refresh(hwnd);
}

/// Clear the terminal content.
unsafe fn clear_output(_hwnd: HWND, terminal: &Arc<Mutex<TerminalBuffer>>, terminal_hwnd: HWND) {
    {
        let mut term = terminal.lock().unwrap();
        term.clear();
    }
    let empty = to_wide("");
    let settextex = SETTEXTEX {
        flags: ST_DEFAULT,
        codepage: 1200,
    };
    SendMessageW(
        terminal_hwnd,
        EM_SETTEXTEX,
        &settextex as *const _ as WPARAM,
        empty.as_ptr() as LPARAM,
    );
    if GUI_TRACE {
        eprintln!("[GUI_TRACE] clear_output: cleared terminal");
    }
    RedrawWindow(
        terminal_hwnd,
        ptr::null(),
        ptr::null_mut(),
        RDW_INVALIDATE | RDW_UPDATENOW,
    );
}

/// Request a terminal refresh. Only posts a message if one isn't already pending.
/// During a test run, we force immediate refresh using SendMessage.
unsafe fn request_refresh(hwnd: HWND) {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if state_ptr == 0 {
        return;
    }
    let state = &mut *(state_ptr as *mut AppState);

    // ---- TEST STREAMING FIX: if test run active, force immediate refresh via SendMessage ----
    if state.is_test_run {
        // SendMessage will process WM_USER_REFRESH synchronously on the main thread.
        SendMessageW(hwnd, WM_USER_REFRESH, 0, 0);
        return;
    }

    if state.pending_refresh {
        if AUTO_SCROLL_TRACE {
            println!("[AUTO-SCROLL] request_refresh: refresh already pending, skipping");
        }
        return;
    }
    state.pending_refresh = true;
    if AUTO_SCROLL_TRACE {
        println!("[AUTO-SCROLL] request_refresh: posting WM_USER_REFRESH");
    }
    PostMessageW(hwnd, WM_USER_REFRESH, 0, 0);
}

/// Process refresh: steal the buffer, truncate if needed, and append.
unsafe fn process_output_refresh(hwnd: HWND) {
    if AUTO_SCROLL_TRACE {
        println!("[AUTO-SCROLL] process_output_refresh: entered");
    }

    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if state_ptr == 0 {
        return;
    }
    let state = &mut *(state_ptr as *mut AppState);

    if state.processing_refresh {
        return;
    }

    // ---- TEST STREAMING FIX: bypass cooldown if test run is active ----
    if !state.is_test_run {
        let now = Instant::now();
        if now.duration_since(state.last_refresh_time) < Duration::from_millis(REFRESH_INTERVAL_MS)
        {
            // We are on cooldown! Don't drop the update.
            // Keep pending_refresh = true and set an alarm to flush the rest later.
            state.pending_refresh = true;
            SetTimer(hwnd, 1001, REFRESH_INTERVAL_MS as u32, ptr::null());
            return;
        }
        // Cooldown passed – proceed
        state.pending_refresh = false;
        state.last_refresh_time = now;
        KillTimer(hwnd, 1001);
    } else {
        // For test runs, we don't throttle – just proceed immediately.
        state.pending_refresh = false;
        // Update last_refresh_time to avoid a sudden burst after the test, but we don't enforce cooldown.
        state.last_refresh_time = Instant::now();
        KillTimer(hwnd, 1001);
    }

    // 1. STEAL the buffer
    let new_lines = {
        let mut term = state.terminal.lock().unwrap();
        term.take_all()
    };

    if new_lines.is_empty() {
        state.processing_refresh = false;
        return;
    }

    // 2. Check if user is at bottom (using GetScrollInfo)
    let was_at_bottom = is_scroll_at_bottom(state.hwnd_terminal);
    if AUTO_SCROLL_TRACE {
        println!(
            "[AUTO-SCROLL] was_at_bottom = {} before modifications",
            was_at_bottom
        );
    }

    // 3. Truncate the Rich Edit control if it has grown too large
    let current_lines = SendMessageW(state.hwnd_terminal, EM_GETLINECOUNT, 0, 0) as isize;
    if current_lines > UI_MAX_LINES {
        let lines_to_delete = current_lines - UI_MAX_LINES;
        let cutoff_char_index = SendMessageW(
            state.hwnd_terminal,
            EM_LINEINDEX,
            lines_to_delete as WPARAM,
            0,
        );

        if AUTO_SCROLL_TRACE {
            println!(
                "[AUTO-SCROLL] Truncating: deleting {} lines, cutoff index {}",
                lines_to_delete, cutoff_char_index
            );
        }

        SendMessageW(state.hwnd_terminal, 0x000B /* WM_SETREDRAW */, 0, 0);
        SendMessageW(
            state.hwnd_terminal,
            EM_SETSEL,
            0,
            cutoff_char_index as LPARAM,
        );
        SendMessageW(state.hwnd_terminal, EM_REPLACESEL, 0, w!("") as LPARAM);
        SendMessageW(state.hwnd_terminal, 0x000B /* WM_SETREDRAW */, 1, 0);
    }

    // 4. Append the new lines with ANSI parsing
    let mut combined = String::with_capacity(new_lines.len() * 80);
    for line in new_lines {
        combined.push_str(&line);
        combined.push_str("\r\n");
    }

    apply_terminal_colored_text_append(state.hwnd_terminal, &combined);

    // 5. Restore caret and snap to bottom if needed
    if was_at_bottom {
        if AUTO_SCROLL_TRACE {
            println!("[AUTO-SCROLL] Scrolling to bottom");
        }
        let ndx = SendMessageW(state.hwnd_terminal, WM_GETTEXTLENGTH, 0, 0);
        SendMessageW(state.hwnd_terminal, EM_SETSEL, ndx as WPARAM, ndx as LPARAM);
        SendMessageW(state.hwnd_terminal, EM_SCROLLCARET, 0, 0);

        let total_lines = SendMessageW(state.hwnd_terminal, EM_GETLINECOUNT, 0, 0);
        if total_lines > 0 {
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let visible_lines = if GetClientRect(state.hwnd_terminal, &mut rect) != 0 {
                (rect.bottom - rect.top) / 20
            } else {
                10
            };
            let current_first = SendMessageW(state.hwnd_terminal, EM_GETFIRSTVISIBLELINE, 0, 0);
            let target_first = total_lines - (visible_lines as isize);
            let delta = target_first - current_first;
            if AUTO_SCROLL_TRACE {
                println!(
                    "[AUTO-SCROLL] EM_LINESCROLL: total_lines={}, visible_lines={}, current_first={}, target_first={}, delta={}",
                    total_lines, visible_lines, current_first, target_first, delta
                );
            }
            if delta > 0 {
                SendMessageW(state.hwnd_terminal, EM_LINESCROLL, 0, delta as LPARAM);
            }
        }
    }

    state.processing_refresh = false;
}

// ============================================================================
// Subclass procedures for clipboard handling and context menu
// ============================================================================

/// Editor subclass procedure – handles:
/// - Drag‑and‑drop forwarding
/// - Custom context menu
/// - All keyboard shortcuts (no accelerator table used)
/// All other keys (including undo/redo/clipboard) are passed to Rich Edit.
unsafe extern "system" fn editor_subclass_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
    _uIdSubclass: usize,
    _dwRefData: usize,
) -> LRESULT {
    let main_hwnd = GetParent(hwnd);
    // Safety: if the parent window is invalid, fall back to default processing.
    if main_hwnd.is_null() {
        return DefSubclassProc(hwnd, msg, wparam, lparam);
    }

    match msg {
        WM_DROPFILES => {
            if DRAG_DROP_TRACE {
                println!("[DRAG_DROP] editor_subclass_proc: forwarding WM_DROPFILES to main");
            }
            let parent = GetParent(hwnd);
            SendMessageW(parent, msg, wparam, lparam);
            return 0;
        }

        WM_CONTEXTMENU => {
            // Create the context menu
            let hMenu = CreatePopupMenu();
            if hMenu.is_null() {
                return DefSubclassProc(hwnd, msg, wparam, lparam);
            }

            // Query editor state
            let can_undo = SendMessageW(hwnd, EM_CANUNDO, 0, 0) != 0;
            let can_redo = SendMessageW(hwnd, EM_CANREDO, 0, 0) != 0;
            let mut start = 0i32;
            let mut end = 0i32;
            SendMessageW(
                hwnd,
                EM_GETSEL,
                &mut start as *mut _ as WPARAM,
                &mut end as *mut _ as LPARAM,
            );
            let has_selection = start != end;

            // Helper to produce wide string pointer (must live long enough)
            let wstr = |s: &str| {
                let wide = to_wide(s);
                wide.as_ptr()
            };

            // Append menu items
            AppendMenuW(
                hMenu,
                if can_undo { MF_ENABLED } else { MF_GRAYED },
                ID_EDIT_UNDO as usize,
                wstr("&Undo\tCtrl+Z"),
            );
            AppendMenuW(
                hMenu,
                if can_redo { MF_ENABLED } else { MF_GRAYED },
                ID_EDIT_REDO as usize,
                wstr("&Redo\tCtrl+Y"),
            );
            AppendMenuW(hMenu, MF_SEPARATOR, 0, ptr::null());
            AppendMenuW(
                hMenu,
                if has_selection { MF_ENABLED } else { MF_GRAYED },
                ID_EDIT_CUT as usize,
                wstr("Cu&t\tCtrl+X"),
            );
            AppendMenuW(
                hMenu,
                if has_selection { MF_ENABLED } else { MF_GRAYED },
                ID_EDIT_COPY as usize,
                wstr("&Copy\tCtrl+C"),
            );
            AppendMenuW(
                hMenu,
                MF_ENABLED,
                ID_EDIT_PASTE as usize,
                wstr("&Paste\tCtrl+V"),
            );
            AppendMenuW(
                hMenu,
                if has_selection { MF_ENABLED } else { MF_GRAYED },
                ID_EDIT_DELETE as usize,
                wstr("&Delete"),
            );
            AppendMenuW(hMenu, MF_SEPARATOR, 0, ptr::null());
            AppendMenuW(
                hMenu,
                MF_ENABLED,
                ID_EDIT_SELECT_ALL as usize,
                wstr("Select &All\tCtrl+A"),
            );

            // Determine where to show the menu
            let mut pt = POINT { x: 0, y: 0 };
            let is_keyboard = lparam == -1;
            if is_keyboard {
                // For keyboard, position near the caret
                let mut char_range = CHARRANGE { cpMin: 0, cpMax: 0 };
                SendMessageW(hwnd, EM_GETSEL, &mut char_range as *mut _ as WPARAM, 0);
                let pos = SendMessageW(hwnd, EM_POSFROMCHAR, char_range.cpMax as WPARAM, 0);
                pt.x = (pos & 0xFFFF) as LONG;
                pt.y = ((pos >> 16) & 0xFFFF) as LONG;
                ClientToScreen(hwnd, &mut pt);
            } else {
                // Mouse: use the provided lParam (screen coordinates)
                pt.x = (lparam & 0xFFFF) as LONG;
                pt.y = ((lparam >> 16) & 0xFFFF) as LONG;
            }

            // Show the menu and get the selected command (TPM_RETURNCMD)
            let cmd = TrackPopupMenu(
                hMenu,
                TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_RETURNCMD,
                pt.x,
                pt.y,
                0,
                hwnd,
                ptr::null(),
            );

            // Destroy menu
            DestroyMenu(hMenu);

            // Handle the selected command by forwarding to main window
            if cmd != 0 {
                // cmd is the menu ID (u16) as a usize
                let wm_id = (cmd & 0xFFFF) as u16;
                // Check if it's one of our edit commands
                match wm_id {
                    ID_EDIT_UNDO | ID_EDIT_REDO | ID_EDIT_CUT | ID_EDIT_COPY | ID_EDIT_PASTE
                    | ID_EDIT_DELETE | ID_EDIT_SELECT_ALL => {
                        // Forward to main window as WM_COMMAND
                        PostMessageW(main_hwnd, WM_COMMAND, wm_id as WPARAM, 0);
                    }
                    _ => {}
                }
            }
            return 0; // Suppress default context menu
        }

        WM_KEYDOWN => {
            let vk = wparam as u16;
            let ctrl = (GetKeyState(VK_CONTROL) as i16) < 0;
            let shift = (GetKeyState(VK_SHIFT) as i16) < 0;

            if ctrl {
                match vk {
                    VK_O => {
                        debug_log!("[GUI] Subclass: Ctrl+O detected -> posting ID_FILE_OPEN");
                        PostMessageW(main_hwnd, WM_COMMAND, ID_FILE_OPEN as WPARAM, 0);
                        return 0;
                    }
                    VK_S => {
                        debug_log!("[GUI] Subclass: Ctrl+S detected -> posting ID_FILE_SAVE");
                        PostMessageW(main_hwnd, WM_COMMAND, ID_FILE_SAVE as WPARAM, 0);
                        return 0;
                    }
                    VK_Q => {
                        debug_log!("[GUI] Subclass: Ctrl+Q detected -> posting WM_CLOSE");
                        PostMessageW(main_hwnd, WM_CLOSE, 0, 0);
                        return 0;
                    }
                    VK_Z => {
                        if shift {
                            // Ctrl+Shift+Z = Redo
                            debug_log!("[GUI] Subclass: Ctrl+Shift+Z -> Redo");
                            SendMessageW(hwnd, EM_REDO, 0, 0);
                            return 0;
                        }
                        // Ctrl+Z = Undo (handled natively by Rich Edit)
                    }
                    VK_Y => {
                        // Ctrl+Y = Redo (handled natively by Rich Edit)
                    }
                    VK_B => {
                        // Ctrl+B = Build Debug, Ctrl+Shift+B = Build Release
                        let cmd = if shift {
                            ID_BUILD_RELEASE
                        } else {
                            ID_BUILD_DEBUG
                        };
                        debug_log!(
                            "[GUI] Subclass: Ctrl+{}B detected -> posting {:?}",
                            if shift { "Shift+" } else { "" },
                            cmd
                        );
                        PostMessageW(main_hwnd, WM_COMMAND, cmd as WPARAM, 0);
                        return 0;
                    }
                    VK_C => {
                        if shift {
                            // Ctrl+Shift+C = Check
                            debug_log!("[GUI] Subclass: Ctrl+Shift+C detected -> posting ID_CHECK");
                            PostMessageW(main_hwnd, WM_COMMAND, ID_CHECK as WPARAM, 0);
                            return 0;
                        }
                        // Ctrl+C = Copy (handled natively by Rich Edit)
                    }
                    VK_T => {
                        // Ctrl+T = Test
                        debug_log!("[GUI] Subclass: Ctrl+T detected -> posting ID_TEST");
                        PostMessageW(main_hwnd, WM_COMMAND, ID_TEST as WPARAM, 0);
                        return 0;
                    }
                    VK_L => {
                        // Ctrl+L = Clean
                        debug_log!("[GUI] Subclass: Ctrl+L detected -> posting ID_CLEAN");
                        PostMessageW(main_hwnd, WM_COMMAND, ID_CLEAN as WPARAM, 0);
                        return 0;
                    }
                    // Let Rich Edit handle Ctrl+C, Ctrl+V, Ctrl+X, Ctrl+A natively
                    _ => {}
                }
            } else if vk == VK_F5 {
                // F5 (no modifier) = Run
                debug_log!("[GUI] Subclass: F5 detected -> posting ID_RUN");
                PostMessageW(main_hwnd, WM_COMMAND, ID_RUN as WPARAM, 0);
                return 0;
            }
            // Let the control process all other keys (including native undo/redo/clipboard)
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }

        _WM_MOUSEMOVE => {
            // Tooltip implementation will be added later.
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }

        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

/// Terminal subclass procedure – handles only drag‑and‑drop forwarding.
unsafe extern "system" fn terminal_subclass_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
    _uIdSubclass: usize,
    _dwRefData: usize,
) -> LRESULT {
    match msg {
        WM_DROPFILES => {
            if DRAG_DROP_TRACE {
                println!("[DRAG_DROP] terminal_subclass_proc: forwarding WM_DROPFILES to main");
            }
            let parent = GetParent(hwnd);
            if !parent.is_null() {
                SendMessageW(parent, msg, wparam, lparam);
            }
            return 0;
        }
        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

// ============================================================================
// Diagnostic helpers
// ============================================================================

unsafe fn clear_diagnostics(state: &mut AppState) {
    let mut cf = CHARFORMAT2W {
        cbSize: mem::size_of::<CHARFORMAT2W>() as u32,
        dwMask: CFM_UNDERLINE | CFM_COLOR,
        dwEffects: 0,
        crTextColor: 0x00FFFFFF,
        ..mem::zeroed()
    };
    SendMessageW(
        state.hwnd_editor,
        EM_SETCHARFORMAT,
        SCF_ALL,
        &mut cf as *mut _ as isize,
    );
    SendMessageW(
        state.hwnd_editor,
        EM_SETCHARFORMAT,
        SCF_DEFAULT,
        &mut cf as *mut _ as isize,
    );
}

unsafe fn apply_diagnostic_underline(state: &AppState, diag: &Diagnostic, text: &str) {
    let (line, start_col, end_col) = if let Some(range) = diag.range {
        (
            range.start_line as usize,
            range.start_col as usize,
            range.end_col as usize,
        )
    } else if let Some(span) = diag.span {
        (span.line as usize, span.col as usize, span.col as usize + 1)
    } else {
        return;
    };

    let mut char_idx = 0;
    let mut current_line = 0;
    let mut current_col = 0;
    let mut start_pos = 0;
    let mut end_pos = 0;
    for ch in text.chars() {
        if current_line == line && current_col == start_col {
            start_pos = char_idx;
        }
        if current_line == line && current_col == end_col {
            end_pos = char_idx;
        }
        if ch == '\n' {
            current_line += 1;
            current_col = 0;
        } else {
            current_col += 1;
        }
        char_idx += ch.len_utf16();
    }
    if end_pos == 0 && start_pos > 0 {
        end_pos = start_pos + 1;
    }

    let color = match diag.level {
        Level::Error => 0x0000FF,
        Level::Warning => 0x00FFFF,
        _ => return,
    };
    let mut cf = CHARFORMAT2W {
        cbSize: mem::size_of::<CHARFORMAT2W>() as u32,
        dwMask: CFM_UNDERLINE | CFM_COLOR,
        dwEffects: CFE_UNDERLINE,
        crTextColor: color,
        ..mem::zeroed()
    };
    SendMessageW(
        state.hwnd_editor,
        EM_SETSEL,
        start_pos as WPARAM,
        end_pos as LPARAM,
    );
    SendMessageW(
        state.hwnd_editor,
        EM_SETCHARFORMAT,
        SCF_SELECTION,
        &mut cf as *mut _ as isize,
    );
}

unsafe fn apply_diagnostics(state: &mut AppState, diags: Vec<Diagnostic>) {
    clear_diagnostics(state);
    state.diagnostics = diags;

    let text = get_editor_text(state.hwnd_editor);
    for diag in &state.diagnostics {
        if diag.level == Level::Error || diag.level == Level::Warning {
            apply_diagnostic_underline(state, diag, &text);
        }
    }
}

// ============================================================================
// Phase callback (called from diagnostic.rs on compiler threads)
// ============================================================================

fn phase_callback(phase: &'static str, percent: usize) {
    // Called from diagnostic.rs – this is on a compiler thread, not the main GUI thread.
    // We use PostMessageW to send the update to the main window.
    let hwnd = get_gui_hwnd();
    debug_log!(
        "[GUI] phase_callback: phase='{}', percent={}, hwnd={:?}",
        phase,
        percent,
        hwnd
    );
    if hwnd.is_null() {
        debug_log!("[GUI] phase_callback: HWND is NULL! Cannot post phase update.");
        return;
    }
    let msg = format!("{} ({}%)", phase, percent);
    let boxed = Box::new(msg);
    unsafe {
        let result = PostMessageW(
            hwnd as HWND,
            WM_USER_PHASE_UPDATE,
            0,
            Box::into_raw(boxed) as isize,
        );
        if result == 0 {
            debug_log!("[GUI] phase_callback: PostMessageW failed!");
        } else {
            debug_log!("[GUI] phase_callback: PostMessageW succeeded");
        }
    }
}

// ============================================================================
// Window Procedure
// ============================================================================

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            debug_log!("[GUI] WM_CREATE");
            let dll_name = to_wide(RICHEDIT_DLL);
            LoadLibraryW(dll_name.as_ptr());

            // Initialize common controls for progress bar
            let mut icc = INITCOMMONCONTROLSEX {
                dwSize: mem::size_of::<INITCOMMONCONTROLSEX>() as DWORD,
                dwICC: ICC_STANDARD_CLASSES | ICC_PROGRESS_CLASS,
            };
            InitCommonControlsEx(&mut icc);

            let hinst = GetModuleHandleW(ptr::null());

            // Editor Rich Edit – flat, borderless (no WS_BORDER, no WS_EX_CLIENTEDGE)
            let class_wide = to_wide(MSFTEDIT_CLASS);
            let hwnd_editor = CreateWindowExW(
                0, // No extended border styles
                class_wide.as_ptr(),
                ptr::null(),
                WS_CHILD
                    | WS_VISIBLE
                    | ES_MULTILINE
                    | ES_AUTOVSCROLL
                    | ES_AUTOHSCROLL
                    | WS_VSCROLL
                    | WS_HSCROLL
                    | ES_NOHIDESEL
                    | ES_SAVESEL,
                0,
                0,
                0,
                0,
                hwnd,
                ptr::null_mut(),
                hinst,
                ptr::null_mut(),
            );
            debug_log!("[GUI] Editor HWND: {:?}", hwnd_editor);

            // Force dark scrollbars and overall dark theme on the editor
            let dark_theme = w!("DarkMode_Explorer");
            SetWindowTheme(hwnd_editor, dark_theme, ptr::null());

            RevokeDragDrop(hwnd_editor);

            // Set a large text limit for the editor (2GB characters)
            SendMessageW(hwnd_editor, EM_LIMITTEXT, 0x7FFFFFFF, 0);

            // ---- Terminal Rich Edit – flat, borderless ----
            // Set ES_READONLY to prevent user input, but allow selection and copy.
            let hwnd_terminal = CreateWindowExW(
                0, // No extended border styles
                class_wide.as_ptr(),
                ptr::null(),
                WS_CHILD
                    | WS_VISIBLE
                    | ES_MULTILINE
                    | ES_AUTOVSCROLL
                    | ES_AUTOHSCROLL
                    | WS_VSCROLL
                    | WS_HSCROLL
                    | ES_READONLY,
                0,
                0,
                0,
                0,
                hwnd,
                ptr::null_mut(),
                hinst,
                ptr::null_mut(),
            );
            debug_log!("[GUI] Terminal HWND: {:?}", hwnd_terminal);
            SetWindowPos(
                hwnd_terminal,
                ptr::null_mut(),
                0,
                0,
                100,
                100,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            // Force dark scrollbars and overall dark theme on the terminal
            SetWindowTheme(hwnd_terminal, dark_theme, ptr::null());

            // Revoke OLE drag‑drop on the terminal too
            RevokeDragDrop(hwnd_terminal);

            // Set effectively infinite text limit for terminal (2GB characters)
            SendMessageW(hwnd_terminal, EM_LIMITTEXT, 0x7FFFFFFF, 0);

            // ---- Button ----
            let hwnd_button = CreateWindowExW(
                0,
                w!("BUTTON"),
                w!("&Run"),
                WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON,
                0,
                0,
                0,
                0,
                hwnd,
                ptr::null_mut(),
                hinst,
                ptr::null_mut(),
            );
            debug_log!("[GUI] Button HWND: {:?}", hwnd_button);

            // ---- Status bar grip is removed ----

            // ---- Status text label ----
            let hwnd_status_text = CreateWindowExW(
                0,
                w!("STATIC"),
                w!("0% - Ready"), // hyphen, not en-dash
                WS_CHILD | WS_VISIBLE | SS_LEFT,
                0,
                0,
                200,
                22,
                hwnd,
                ptr::null_mut(),
                hinst,
                ptr::null_mut(),
            );
            debug_log!("[GUI] Status text HWND: {:?}", hwnd_status_text);

            // ---- Progress bar ----
            let hwnd_progress = CreateWindowExW(
                0,
                w!(PROGRESS_CLASS),
                ptr::null(),
                WS_CHILD | WS_VISIBLE | PBS_SMOOTH,
                0,
                0,
                100,
                22,
                hwnd,
                ptr::null_mut(),
                hinst,
                ptr::null_mut(),
            );
            debug_log!("[GUI] Progress bar HWND: {:?}", hwnd_progress);

            // ---- Create dark brushes for controls ----
            let hbrEditBk = CreateSolidBrush(0x001E1E1E);
            let hbrButtonBk = CreateSolidBrush(0x001E1E1E);
            let hbrStatusBk = CreateSolidBrush(0x001E1E1E);

            // ---- FORCE DARK THEME ON ALL CONTROLS (already done for editor/terminal) ----
            SetWindowTheme(hwnd_button, dark_theme, ptr::null());
            SetWindowTheme(hwnd_status_text, dark_theme, ptr::null());
            SetWindowTheme(hwnd_progress, dark_theme, ptr::null());

            let hr_progress = SetWindowTheme(hwnd_progress, dark_theme, ptr::null());
            debug_log!(
                "[GUI] SetWindowTheme(progress) HRESULT = 0x{:08X}",
                hr_progress
            );

            // ---- Create a modern, ClearType‑hinted monospace font ----
            debug_log!("[GUI] Creating font with Consolas, -15pt, ClearType");
            let hFont = CreateFontW(
                -15,                     // height (negative = point size)
                0,                       // width (0 = automatic)
                0,                       // escapement
                0,                       // orientation
                FW_NORMAL,               // weight
                0,                       // italic
                0,                       // underline
                0,                       // strikeout
                DEFAULT_CHARSET,         // character set
                OUT_DEFAULT_PRECIS,      // output precision
                CLIP_DEFAULT_PRECIS,     // clipping precision
                CLEARTYPE_QUALITY,       // quality (ClearType)
                FIXED_PITCH | FF_MODERN, // pitch and family
                w!("Consolas"),          // face name
            );
            debug_log!("[GUI] Font handle: {:?}", hFont);

            // ---- Apply font and background to editor ----
            if !hFont.is_null() {
                let res = SendMessageW(hwnd_editor, 0x0030, hFont as WPARAM, 1);
                debug_log!("[GUI] WM_SETFONT(editor) returned {}", res);
            } else {
                debug_log!("[GUI] ERROR: hFont is NULL!");
            }

            // Set dark background for editor via EM_SETBKGNDCOL
            let dark_bg: isize = 0x001E1E1E;
            let prev_bg = SendMessageW(hwnd_editor, EM_SETBKGNDCOL, 0, dark_bg);
            debug_log!(
                "[GUI] EM_SETBKGNDCOL(editor) returned {} (previous)",
                prev_bg
            );
            // Force redraw to apply new background
            RedrawWindow(
                hwnd_editor,
                ptr::null(),
                ptr::null_mut(),
                RDW_INVALIDATE | RDW_UPDATENOW,
            );

            // Set default text color to white for editor
            set_editor_white_color(hwnd_editor);

            // Apply font to terminal
            if !hFont.is_null() {
                let res = SendMessageW(hwnd_terminal, 0x0030, hFont as WPARAM, 1);
                debug_log!("[GUI] WM_SETFONT(terminal) returned {}", res);
            }

            // Set dark background for terminal via EM_SETBKGNDCOL
            let prev_bg = SendMessageW(hwnd_terminal, EM_SETBKGNDCOL, 0, dark_bg);
            debug_log!(
                "[GUI] EM_SETBKGNDCOL(terminal) returned {} (previous)",
                prev_bg
            );
            RedrawWindow(
                hwnd_terminal,
                ptr::null(),
                ptr::null_mut(),
                RDW_INVALIDATE | RDW_UPDATENOW,
            );

            // Also set default text color for terminal to white
            let mut cf = CHARFORMAT2W {
                cbSize: mem::size_of::<CHARFORMAT2W>() as u32,
                dwMask: CFM_COLOR,
                dwEffects: 0,
                crTextColor: 0x00FFFFFF,
                ..mem::zeroed()
            };
            SendMessageW(
                hwnd_terminal,
                EM_SETCHARFORMAT,
                SCF_ALL,
                &mut cf as *mut _ as isize,
            );
            SendMessageW(
                hwnd_terminal,
                EM_SETCHARFORMAT,
                SCF_DEFAULT,
                &mut cf as *mut _ as isize,
            );

            // ---- Apply font to status text and button ----
            if !hFont.is_null() {
                let res = SendMessageW(hwnd_status_text, 0x0030, hFont as WPARAM, 1);
                debug_log!("[GUI] WM_SETFONT(status_text) returned {}", res);
                let res = SendMessageW(hwnd_button, 0x0030, hFont as WPARAM, 1);
                debug_log!("[GUI] WM_SETFONT(button) returned {}", res);
            }

            // ---- Add margins to editor and terminal ----
            let margins = (12 << 16) | 12; // right and left = 12 pixels
            let margin_res = SendMessageW(
                hwnd_editor,
                EM_SETMARGINS,
                (EC_LEFTMARGIN | EC_RIGHTMARGIN) as WPARAM,
                margins as LPARAM,
            );
            debug_log!("[GUI] EM_SETMARGINS(editor) returned {}", margin_res);
            let margin_res = SendMessageW(
                hwnd_terminal,
                EM_SETMARGINS,
                (EC_LEFTMARGIN | EC_RIGHTMARGIN) as WPARAM,
                margins as LPARAM,
            );
            debug_log!("[GUI] EM_SETMARGINS(terminal) returned {}", margin_res);

            // ---- Theme the button (already done) ----
            let hr_button = SetWindowTheme(hwnd_button, dark_theme, ptr::null());
            debug_log!("[GUI] SetWindowTheme(button) HRESULT = 0x{:08X}", hr_button);

            let hBrush = CreateSolidBrush(0x1E1E1E);

            // ---- Menu ----
            let hMenu = CreateMenu();

            // File menu
            let hFileMenu = CreatePopupMenu();
            AppendMenuW(hFileMenu, MF_STRING, ID_FILE_OPEN as usize, w!("&Open..."));
            AppendMenuW(hFileMenu, MF_STRING, ID_FILE_SAVE as usize, w!("&Save"));
            AppendMenuW(hFileMenu, MF_SEPARATOR, 0, ptr::null());
            AppendMenuW(hFileMenu, MF_STRING, ID_RUN as usize, w!("&Run"));
            AppendMenuW(hFileMenu, MF_SEPARATOR, 0, ptr::null());
            AppendMenuW(hFileMenu, MF_STRING, ID_FILE_EXIT as usize, w!("E&xit"));
            AppendMenuW(hMenu, MF_POPUP, hFileMenu as usize, w!("&File"));

            // Edit menu
            let hEditMenu = CreatePopupMenu();
            AppendMenuW(
                hEditMenu,
                MF_STRING,
                ID_EDIT_UNDO as usize,
                w!("&Undo\tCtrl+Z"),
            );
            AppendMenuW(
                hEditMenu,
                MF_STRING,
                ID_EDIT_REDO as usize,
                w!("&Redo\tCtrl+Y"),
            );
            AppendMenuW(hEditMenu, MF_SEPARATOR, 0, ptr::null());
            AppendMenuW(
                hEditMenu,
                MF_STRING,
                ID_EDIT_CUT as usize,
                w!("Cu&t\tCtrl+X"),
            );
            AppendMenuW(
                hEditMenu,
                MF_STRING,
                ID_EDIT_COPY as usize,
                w!("&Copy\tCtrl+C"),
            );
            AppendMenuW(
                hEditMenu,
                MF_STRING,
                ID_EDIT_PASTE as usize,
                w!("&Paste\tCtrl+V"),
            );
            AppendMenuW(hEditMenu, MF_STRING, ID_EDIT_DELETE as usize, w!("&Delete"));
            AppendMenuW(
                hEditMenu,
                MF_STRING,
                ID_EDIT_SELECT_ALL as usize,
                w!("Select &All\tCtrl+A"),
            );
            AppendMenuW(hMenu, MF_POPUP, hEditMenu as usize, w!("&Edit"));

            // Build menu
            let hBuildMenu = CreatePopupMenu();
            AppendMenuW(
                hBuildMenu,
                MF_STRING,
                ID_BUILD_DEBUG as usize,
                w!("&Build (Debug)\tCtrl+B"),
            );
            AppendMenuW(
                hBuildMenu,
                MF_STRING,
                ID_BUILD_RELEASE as usize,
                w!("Build &Release\tCtrl+Shift+B"),
            );
            AppendMenuW(hBuildMenu, MF_SEPARATOR, 0, ptr::null());
            AppendMenuW(
                hBuildMenu,
                MF_STRING,
                ID_CHECK as usize,
                w!("&Check\tCtrl+Shift+C"),
            );
            AppendMenuW(hBuildMenu, MF_SEPARATOR, 0, ptr::null());
            AppendMenuW(hBuildMenu, MF_STRING, ID_TEST as usize, w!("&Test\tCtrl+T"));
            AppendMenuW(
                hBuildMenu,
                MF_STRING,
                ID_CLEAN as usize,
                w!("&Clean\tCtrl+L"),
            );
            AppendMenuW(hMenu, MF_POPUP, hBuildMenu as usize, w!("&Build"));

            SetMenu(hwnd, hMenu);
            DrawMenuBar(hwnd);

            // ---- Tooltip (placeholder) ----
            let tooltip_hwnd = CreateWindowExW(
                0,
                w!("tooltips_class32"),
                ptr::null(),
                WS_POPUP | TTS_ALWAYSTIP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                hwnd_editor,
                ptr::null_mut(),
                hinst,
                ptr::null_mut(),
            );
            debug_log!("[GUI] Tooltip HWND: {:?}", tooltip_hwnd);

            // ---- AppState ----
            let state = AppState {
                hwnd_main: hwnd,
                hwnd_editor,
                hwnd_terminal,
                hwnd_button,
                hwnd_status_text,
                hwnd_progress,
                hFont,
                hBrush,
                hbrStatusBk,
                hbrEditBk,
                hbrButtonBk,
                tooltip_hwnd,
                compilation_in_progress: false,
                was_at_bottom: false,
                pending_refresh: false,
                is_test_run: false, // <-- NEW
                ..Default::default()
            };

            let terminal = state.terminal.clone();
            let boxed = Box::new(state);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(boxed) as isize);

            // Set the terminal buffer and GUI HWND for the diagnostic module
            set_gui_terminal(terminal);
            debug_log!("[GUI] Setting GUI HWND to {:?}", hwnd);
            set_gui_hwnd(hwnd);

            // Set the phase callback (this is the function that will be called from diagnostic.rs)
            set_gui_phase_callback(phase_callback);

            // Subclass the editor
            let _ = SetWindowSubclass(
                hwnd_editor,
                editor_subclass_proc as *const () as usize,
                1,
                0,
            );

            // Subclass the terminal
            let _ = SetWindowSubclass(
                hwnd_terminal,
                terminal_subclass_proc as *const () as usize,
                2,
                0,
            );

            // Revoke OLE drag‑drop on the main window as well to be safe
            RevokeDragDrop(hwnd);

            // Enable classic drag‑and‑drop on main window AND child controls
            DragAcceptFiles(hwnd, 1);
            DragAcceptFiles(hwnd_editor, 1);
            DragAcceptFiles(hwnd_terminal, 1);

            // --- UIPI BYPASS: Allow Drag-and-Drop when running as Administrator ---
            let allow_admin_drop = |h: HWND| {
                ChangeWindowMessageFilterEx(h, WM_DROPFILES, MSGFLT_ALLOW, ptr::null_mut());
                ChangeWindowMessageFilterEx(h, WM_COPYDATA, MSGFLT_ALLOW, ptr::null_mut());
                ChangeWindowMessageFilterEx(h, WM_COPYGLOBALDATA, MSGFLT_ALLOW, ptr::null_mut());
            };

            allow_admin_drop(hwnd);
            allow_admin_drop(hwnd_editor);
            allow_admin_drop(hwnd_terminal);

            SetFocus(hwnd_editor);

            // Set initial progress bar value (0)
            SendMessageW(hwnd_progress, PBM_SETPOS, 0, 0);

            // ---- Force an initial WM_SIZE to set layout ----
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetClientRect(hwnd, &mut rect);
            SendMessageW(
                hwnd,
                WM_SIZE,
                0,
                ((rect.bottom as isize) << 16) | (rect.right as isize),
            );

            return 0;
        }

        // ---- Undocumented UAH messages for dark menu bar ----
        // WM_UAHDRAWMENU: paints the background of the menu bar
        WM_UAHDRAWMENU => {
            let p_uahmenu = lparam as *const UAHMENU;
            if !p_uahmenu.is_null() {
                let hdc = (*p_uahmenu).hdc as HDC;

                let mut rc_window = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                GetWindowRect(hwnd, &mut rc_window);

                let mut mbi: MENUBARINFO = mem::zeroed();
                mbi.cbSize = mem::size_of::<MENUBARINFO>() as u32;

                // OBJID_MENU = -3
                if GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut mbi) != 0 {
                    let mut rc_bar = mbi.rcBar;

                    // Map from screen coordinates to non-client window coordinates
                    rc_bar.left -= rc_window.left;
                    rc_bar.top -= rc_window.top;
                    rc_bar.right -= rc_window.left;
                    rc_bar.bottom -= rc_window.top;

                    // #1E1E1E is standard VS Code dark gray
                    let brush = CreateSolidBrush(0x001E1E1E);
                    FillRect(hdc, &rc_bar, brush);
                    DeleteObject(brush as *mut c_void);
                }
            }
            return 0;
        }

        // WM_UAHDRAWMENUITEM: paints individual menu items (File, Edit, etc.)
        WM_UAHDRAWMENUITEM => {
            let p_draw = lparam as *const UAHDRAWMENUITEM;
            if !p_draw.is_null() {
                let dis = &(*p_draw).dis;
                let hdc = dis.hDC;
                let mut rc = dis.rcItem;

                // Check if user is hovering (ODS_HOTLIGHT = 0x0040) or clicking (ODS_SELECTED = 0x0001)
                let is_active = (dis.itemState & 0x0041) != 0;

                // Use lighter gray for hover/selection, dark gray for idle
                let bg_color = if is_active { 0x00333333 } else { 0x001E1E1E };

                let brush = CreateSolidBrush(bg_color);
                FillRect(hdc, &rc, brush);
                DeleteObject(brush as *mut c_void);

                // Set text color and background mode
                SetBkMode(hdc, 1); // TRANSPARENT
                SetTextColor(hdc, 0x00FFFFFF); // White text

                // Get menu text
                let mut text = [0u16; 256];
                let hmenu = (*p_draw).um.hmenu as HMENU;

                let len = GetMenuStringW(
                    hmenu,
                    (*p_draw).umi.iPosition as u32,
                    text.as_mut_ptr(),
                    text.len() as i32,
                    0x0400, // MF_BYPOSITION
                );

                if len > 0 {
                    // DT_SINGLELINE | DT_CENTER | DT_VCENTER = 0x0025
                    let format = DT_SINGLELINE | DT_CENTER | DT_VCENTER;
                    DrawTextW(hdc, text.as_ptr(), len, &mut rc, format);
                }
            }
            return 0;
        }

        // --------------------------------------------------------------
        // WM_ERASEBKGND – fill with border color so the 1‑px gap becomes visible
        // --------------------------------------------------------------
        WM_ERASEBKGND => {
            let hdc = wparam as HDC;
            let border_brush = CreateSolidBrush(0x00333333);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetClientRect(hwnd, &mut rect);
            FillRect(hdc, &rect, border_brush);
            DeleteObject(border_brush);
            return 1; // we handled it
        }

        WM_CTLCOLOREDIT => {
            // For editor and terminal, return dark background brush and set white text
            let hdc = wparam as HDC;
            let hwndCtl = lparam as HWND;
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if state_ptr != 0 {
                let state = &*(state_ptr as *const AppState);
                if hwndCtl == state.hwnd_editor || hwndCtl == state.hwnd_terminal {
                    SetTextColor(hdc, 0x00FFFFFF); // white text
                    SetBkColor(hdc, 0x001E1E1E); // dark background
                    return state.hbrEditBk as LRESULT;
                }
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        WM_CTLCOLORBTN => {
            // For button, return dark background brush if needed
            let hdc = wparam as HDC;
            let hwndCtl = lparam as HWND;
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if state_ptr != 0 {
                let state = &*(state_ptr as *const AppState);
                if hwndCtl == state.hwnd_button {
                    SetTextColor(hdc, 0x00FFFFFF);
                    SetBkColor(hdc, 0x001E1E1E);
                    return state.hbrButtonBk as LRESULT;
                }
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        WM_CTLCOLORSTATIC => {
            // Handle dark background for status text
            let hdc = wparam as HDC;
            let hwndCtl = lparam as HWND;
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if state_ptr != 0 {
                let state = &*(state_ptr as *const AppState);
                if hwndCtl == state.hwnd_status_text {
                    SetTextColor(hdc, 0x00DCDCDC);
                    SetBkColor(hdc, 0x001E1E1E);
                    return state.hbrStatusBk as LRESULT;
                }
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        // ============================================================
        // WM_SIZE – layout with 2‑px inset (Gap Trick) for custom border
        // ============================================================
        WM_SIZE => {
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if state_ptr == 0 {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let state = &*(state_ptr as *const AppState);

            let client_width = (lparam & 0xFFFF) as i32;
            let client_height = ((lparam >> 16) & 0xFFFF) as i32;

            // Inset for the custom border (2px on all sides for visibility)
            const BORDER_INSET: i32 = 2;

            // Increased bottom bar height to 64 pixels
            let bottom_bar_height = 64;
            let gap = 8; // gap between editor and terminal

            // Compute heights without insets
            let editor_height = ((client_height - bottom_bar_height) * 6) / 10;
            let terminal_height = client_height - bottom_bar_height - editor_height - gap;

            let hdwp = BeginDeferWindowPos(5); // 5 controls: editor, terminal, status_text, progress, button

            // Editor – inset by BORDER_INSET from left, top, right, bottom
            let _ = DeferWindowPos(
                hdwp,
                state.hwnd_editor,
                ptr::null_mut(),
                BORDER_INSET,
                BORDER_INSET,
                client_width - 2 * BORDER_INSET,
                editor_height - 2 * BORDER_INSET,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            // Terminal – inset from left, right, and bottom, and from top with gap
            let _ = DeferWindowPos(
                hdwp,
                state.hwnd_terminal,
                ptr::null_mut(),
                BORDER_INSET,
                editor_height + gap + BORDER_INSET, // start after gap and inset
                client_width - 2 * BORDER_INSET,
                terminal_height - 2 * BORDER_INSET,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            // Status text – we leave it as before (no inset needed, but we keep it aligned)
            let status_text_width = 280;
            let button_width = 100;
            let button_height = 34;

            // Status text – vertically centered in the taller bar
            let status_y = client_height - bottom_bar_height + (bottom_bar_height - 22) / 2;
            let _ = DeferWindowPos(
                hdwp,
                state.hwnd_status_text,
                ptr::null_mut(),
                gap,
                status_y,
                status_text_width,
                22,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            // Progress bar – vertically centered
            let progress_x = gap + status_text_width + gap;
            let progress_width =
                (client_width - status_text_width - button_width - gap * 6).max(120);
            let progress_y = client_height - bottom_bar_height + (bottom_bar_height - 18) / 2;
            let _ = DeferWindowPos(
                hdwp,
                state.hwnd_progress,
                ptr::null_mut(),
                progress_x,
                progress_y,
                progress_width,
                18,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            // Run Button – vertically centered
            let button_y =
                client_height - bottom_bar_height + (bottom_bar_height - button_height) / 2;
            let _ = DeferWindowPos(
                hdwp,
                state.hwnd_button,
                ptr::null_mut(),
                client_width - button_width - 12,
                button_y,
                button_width,
                button_height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            EndDeferWindowPos(hdwp);

            // Force redraw of button to ensure it appears correctly
            RedrawWindow(
                state.hwnd_button,
                ptr::null(),
                ptr::null_mut(),
                RDW_INVALIDATE | RDW_UPDATENOW,
            );

            return 0;
        }

        WM_DROPFILES => {
            if DRAG_DROP_TRACE {
                println!("[DRAG_DROP] WM_DROPFILES received in main window");
            }
            let hdrop = wparam as *mut c_void;
            let file_count = DragQueryFileW(hdrop, DRAGQUERYFILE, ptr::null_mut(), 0);
            if DRAG_DROP_TRACE {
                println!("[DRAG_DROP] file_count = {}", file_count);
            }
            if file_count > 0 {
                let len = DragQueryFileW(hdrop, 0, ptr::null_mut(), 0) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, 0, buf.as_mut_ptr(), buf.len() as u32);
                if let Ok(path) = String::from_utf16(&buf[..len]) {
                    if DRAG_DROP_TRACE {
                        println!("[DRAG_DROP] path = {}", path);
                    }
                    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                    if state_ptr != 0 {
                        let state = &mut *(state_ptr as *mut AppState);
                        if let Err(e) = load_file(state, &path) {
                            if DRAG_DROP_TRACE {
                                println!("[DRAG_DROP] load_file failed: {}", e);
                            }
                            let msg = format!("Failed to load: {}", e);
                            push_output_line(&state.terminal, hwnd, msg);
                        } else {
                            if DRAG_DROP_TRACE {
                                println!("[DRAG_DROP] load_file succeeded");
                            }
                        }
                    } else {
                        if DRAG_DROP_TRACE {
                            println!("[DRAG_DROP] state_ptr is null, cannot load file");
                        }
                    }
                } else {
                    if DRAG_DROP_TRACE {
                        println!("[DRAG_DROP] Failed to parse dropped file path");
                    }
                }
            } else {
                if DRAG_DROP_TRACE {
                    println!("[DRAG_DROP] No files dropped");
                }
            }
            DragFinish(hdrop);
            return 0;
        }

        WM_COMMAND => {
            let wm_id = (wparam & 0xFFFF) as u16;
            let wm_code = (wparam >> 16) as u16;
            let hwnd_from = lparam as HWND;

            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if state_ptr == 0 {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let state = &mut *(state_ptr as *mut AppState);

            // Ignore messages from the terminal control to avoid reentrancy issues.
            if hwnd_from == state.hwnd_terminal {
                return 0;
            }

            debug_log!(
                "[GUI] WM_COMMAND: id={}, code={}, from={:?}",
                wm_id,
                wm_code,
                hwnd_from
            );

            match wm_id {
                ID_FILE_OPEN => {
                    debug_log!("[GUI] ID_FILE_OPEN");
                    let mut file_buf = [0u16; 260];
                    let mut ofn = OPENFILENAMEW {
                        lStructSize: mem::size_of::<OPENFILENAMEW>() as DWORD,
                        hwndOwner: hwnd,
                        hInstance: ptr::null_mut(),
                        lpstrFilter: w!("Vox Source Files\0*.vx\0All Files\0*.*\0"),
                        lpstrCustomFilter: ptr::null_mut(),
                        nMaxCustFilter: 0,
                        nFilterIndex: 1,
                        lpstrFile: file_buf.as_mut_ptr(),
                        nMaxFile: file_buf.len() as DWORD,
                        lpstrFileTitle: ptr::null_mut(),
                        nMaxFileTitle: 0,
                        lpstrInitialDir: ptr::null(),
                        lpstrTitle: w!("Open Vox Source File"),
                        Flags: OFN_FILEMUSTEXIST | OFN_HIDEREADONLY | OFN_PATHMUSTEXIST,
                        nFileOffset: 0,
                        nFileExtension: 0,
                        lpstrDefExt: w!("vx"),
                        lCustData: 0,
                        lpfnHook: 0,
                        lpTemplateName: ptr::null(),
                        pvReserved: ptr::null_mut(),
                        dwReserved: 0,
                        FlagsEx: 0,
                    };
                    if GetOpenFileNameW(&mut ofn as *mut _) != 0 {
                        if let Ok(path) = String::from_utf16(&file_buf[..]) {
                            let path = path.trim_end_matches('\0');
                            debug_log!("[GUI] User selected: {}", path);
                            if let Err(e) = load_file(state, path) {
                                debug_log!("[GUI] load_file failed: {}", e);
                                let msg = format!("Failed to load: {}", e);
                                push_output_line(&state.terminal, hwnd, msg);
                            }
                        }
                    }
                    return 0;
                }
                ID_FILE_SAVE => {
                    debug_log!("[GUI] ID_FILE_SAVE");
                    if let Err(e) = save_file(state) {
                        debug_log!("[GUI] save_file failed: {}", e);
                        push_output_line(&state.terminal, hwnd, format!("Save failed: {}", e));
                    } else {
                        state.is_modified = false;
                        push_output_line(&state.terminal, hwnd, "File saved.".to_string());
                    }
                    return 0;
                }
                ID_RUN => {
                    debug_log!("[GUI] ID_RUN");
                    // Prevent multiple concurrent runs
                    if state.compilation_in_progress {
                        push_output_line(
                            &state.terminal,
                            hwnd,
                            "Compilation already in progress.".to_string(),
                        );
                        return 0;
                    }

                    if let Some(_path) = &state.file_path {
                        if let Err(e) = save_file(state) {
                            debug_log!("[GUI] save_file before run failed: {}", e);
                            push_output_line(&state.terminal, hwnd, format!("Save failed: {}", e));
                            return 0;
                        }
                    }

                    clear_output(hwnd, &state.terminal, state.hwnd_terminal);

                    let path = match state.file_path.clone() {
                        Some(p) => Path::new(&p).to_path_buf(),
                        None => {
                            debug_log!("[GUI] ID_RUN: no file open");
                            push_output_line(
                                &state.terminal,
                                hwnd,
                                "No file open. Use Open or drag-and-drop.".to_string(),
                            );
                            return 0;
                        }
                    };
                    debug_log!("[GUI] Running: {}", path.display());

                    // Mark compilation as in progress and update status bar
                    state.compilation_in_progress = true;
                    // Send initial phase update (0%) via the diagnostic system
                    emit_phase_update("Compiling", 0);

                    let terminal = state.terminal.clone();
                    let hwnd_main = hwnd as usize;
                    let target = host_triple();
                    let config = CacheConfig {
                        no_cache: true,
                        reuse_proofs: false,
                        reuse_bitcode: false,
                        offline: true,
                        trust_modules: false,
                    };

                    thread::spawn(move || {
                        let hwnd = hwnd_main as HWND;
                        unsafe {
                            push_output_line(
                                &terminal,
                                hwnd,
                                format!("Compiling {}...", path.display()),
                            );
                        }
                        let result = compile_and_run_file(&path, &target, &config, None, None);
                        unsafe {
                            match result {
                                Ok(output) => {
                                    for line in output.lines {
                                        push_output_line(&terminal, hwnd, line);
                                    }
                                    push_output_line(
                                        &terminal,
                                        hwnd,
                                        "Execution finished.".to_string(),
                                    );
                                }
                                Err(e) => {
                                    debug_log!("[GUI] compile_and_run_file error: {}", e);
                                    push_output_line(&terminal, hwnd, format!("Error: {}", e));
                                }
                            }
                        }
                        // Force a final refresh to ensure all lines are rendered
                        unsafe {
                            if AUTO_SCROLL_TRACE {
                                println!(
                                    "[AUTO-SCROLL] Compilation thread: posting final WM_USER_REFRESH"
                                );
                            }
                            PostMessageW(hwnd, WM_USER_REFRESH, 0, 0);
                            thread::sleep(Duration::from_millis(20));
                            if AUTO_SCROLL_TRACE {
                                println!(
                                    "[AUTO-SCROLL] Compilation thread: sending 'Compilation complete'"
                                );
                            }
                            emit_phase_update("Compilation complete", 100);
                        }
                    });
                    return 0;
                }

                // ---- New build actions ----
                ID_BUILD_DEBUG => {
                    debug_log!("[GUI] ID_BUILD_DEBUG");
                    if state.compilation_in_progress {
                        push_output_line(
                            &state.terminal,
                            hwnd,
                            "Compilation already in progress.".to_string(),
                        );
                        return 0;
                    }

                    if let Some(_path) = &state.file_path {
                        if let Err(e) = save_file(state) {
                            debug_log!("[GUI] save_file before build failed: {}", e);
                            push_output_line(&state.terminal, hwnd, format!("Save failed: {}", e));
                            return 0;
                        }
                        let path = Path::new(&_path).to_path_buf();
                        clear_output(hwnd, &state.terminal, state.hwnd_terminal);
                        state.compilation_in_progress = true;
                        emit_phase_update("Building (debug)", 0);

                        let terminal = state.terminal.clone();
                        let hwnd_main = hwnd as usize;
                        let target = host_triple();
                        let config = CacheConfig {
                            no_cache: true,
                            reuse_proofs: false,
                            reuse_bitcode: false,
                            offline: true,
                            trust_modules: false,
                        };

                        thread::spawn(move || {
                            let hwnd = hwnd_main as HWND;
                            unsafe {
                                push_output_line(
                                    &terminal,
                                    hwnd,
                                    format!("Building (debug) {}...", path.display()),
                                );
                            }
                            let result = build_file(&path, false, &target, &config, None, None);
                            unsafe {
                                match result {
                                    Ok(exe) => {
                                        push_output_line(
                                            &terminal,
                                            hwnd,
                                            format!("Build succeeded: {}", exe.display()),
                                        );
                                    }
                                    Err(e) => {
                                        debug_log!("[GUI] build_file error: {}", e);
                                        push_output_line(
                                            &terminal,
                                            hwnd,
                                            format!("Build failed: {}", e),
                                        );
                                    }
                                }
                                // Final refresh and status update
                                PostMessageW(hwnd, WM_USER_REFRESH, 0, 0);
                                thread::sleep(Duration::from_millis(20));
                                emit_phase_update("Build complete", 100);
                            }
                        });
                    } else {
                        push_output_line(&state.terminal, hwnd, "No file open.".to_string());
                    }
                    return 0;
                }

                ID_BUILD_RELEASE => {
                    debug_log!("[GUI] ID_BUILD_RELEASE");
                    if state.compilation_in_progress {
                        push_output_line(
                            &state.terminal,
                            hwnd,
                            "Compilation already in progress.".to_string(),
                        );
                        return 0;
                    }

                    if let Some(_path) = &state.file_path {
                        if let Err(e) = save_file(state) {
                            debug_log!("[GUI] save_file before build failed: {}", e);
                            push_output_line(&state.terminal, hwnd, format!("Save failed: {}", e));
                            return 0;
                        }
                        let path = Path::new(&_path).to_path_buf();
                        clear_output(hwnd, &state.terminal, state.hwnd_terminal);
                        state.compilation_in_progress = true;
                        emit_phase_update("Building (release)", 0);

                        let terminal = state.terminal.clone();
                        let hwnd_main = hwnd as usize;
                        let target = host_triple();
                        let config = CacheConfig {
                            no_cache: true,
                            reuse_proofs: false,
                            reuse_bitcode: false,
                            offline: true,
                            trust_modules: false,
                        };

                        thread::spawn(move || {
                            let hwnd = hwnd_main as HWND;
                            unsafe {
                                push_output_line(
                                    &terminal,
                                    hwnd,
                                    format!("Building (release) {}...", path.display()),
                                );
                            }
                            let result = build_file(&path, true, &target, &config, None, None);
                            unsafe {
                                match result {
                                    Ok(exe) => {
                                        push_output_line(
                                            &terminal,
                                            hwnd,
                                            format!("Build succeeded: {}", exe.display()),
                                        );
                                    }
                                    Err(e) => {
                                        debug_log!("[GUI] build_file error: {}", e);
                                        push_output_line(
                                            &terminal,
                                            hwnd,
                                            format!("Build failed: {}", e),
                                        );
                                    }
                                }
                                PostMessageW(hwnd, WM_USER_REFRESH, 0, 0);
                                thread::sleep(Duration::from_millis(20));
                                emit_phase_update("Build complete", 100);
                            }
                        });
                    } else {
                        push_output_line(&state.terminal, hwnd, "No file open.".to_string());
                    }
                    return 0;
                }

                ID_CHECK => {
                    debug_log!("[GUI] ID_CHECK");
                    if state.compilation_in_progress {
                        push_output_line(
                            &state.terminal,
                            hwnd,
                            "Compilation already in progress.".to_string(),
                        );
                        return 0;
                    }

                    if let Some(_path) = &state.file_path {
                        if let Err(e) = save_file(state) {
                            debug_log!("[GUI] save_file before check failed: {}", e);
                            push_output_line(&state.terminal, hwnd, format!("Save failed: {}", e));
                            return 0;
                        }
                        let path = Path::new(&_path).to_path_buf();
                        clear_output(hwnd, &state.terminal, state.hwnd_terminal);
                        state.compilation_in_progress = true;
                        emit_phase_update("Checking", 0);

                        let terminal = state.terminal.clone();
                        let hwnd_main = hwnd as usize;
                        let target = host_triple();
                        let config = CacheConfig {
                            no_cache: true,
                            reuse_proofs: false,
                            reuse_bitcode: false,
                            offline: true,
                            trust_modules: false,
                        };

                        thread::spawn(move || {
                            let hwnd = hwnd_main as HWND;
                            unsafe {
                                push_output_line(
                                    &terminal,
                                    hwnd,
                                    format!("Checking {}...", path.display()),
                                );
                            }
                            let result = check_file(&path, &target, &config, None, None);
                            unsafe {
                                match result {
                                    Ok(true) => {
                                        push_output_line(
                                            &terminal,
                                            hwnd,
                                            "Check passed.".to_string(),
                                        );
                                    }
                                    Ok(false) => {
                                        push_output_line(
                                            &terminal,
                                            hwnd,
                                            "Check failed: semantic errors.".to_string(),
                                        );
                                    }
                                    Err(e) => {
                                        debug_log!("[GUI] check_file error: {}", e);
                                        push_output_line(
                                            &terminal,
                                            hwnd,
                                            format!("Check error: {}", e),
                                        );
                                    }
                                }
                                PostMessageW(hwnd, WM_USER_REFRESH, 0, 0);
                                thread::sleep(Duration::from_millis(20));
                                emit_phase_update("Check complete", 100);
                            }
                        });
                    } else {
                        push_output_line(&state.terminal, hwnd, "No file open.".to_string());
                    }
                    return 0;
                }

                ID_TEST => {
                    debug_log!("[GUI] ID_TEST");
                    if state.compilation_in_progress {
                        push_output_line(
                            &state.terminal,
                            hwnd,
                            "Compilation already in progress.".to_string(),
                        );
                        return 0;
                    }

                    clear_output(hwnd, &state.terminal, state.hwnd_terminal);
                    state.compilation_in_progress = true;

                    let terminal = state.terminal.clone();
                    let hwnd_main = hwnd as usize;
                    let target = host_triple();
                    let config = CacheConfig {
                        no_cache: true,
                        reuse_proofs: false,
                        reuse_bitcode: false,
                        offline: true,
                        trust_modules: false,
                    };

                    // Use find_vox_root() to locate the examples directory
                    let root = find_vox_root();
                    let test_dir = root.join("src/Examples");
                    if !test_dir.exists() {
                        let msg = format!("Examples directory not found at {}", test_dir.display());
                        push_output_line(&state.terminal, hwnd, msg);
                        state.compilation_in_progress = false;
                        return 0;
                    }

                    // ---- Set test run flag so refresh bypasses throttle ----
                    state.is_test_run = true;

                    thread::spawn(move || {
                        let hwnd = hwnd_main as HWND;
                        unsafe {
                            push_output_line(
                                &terminal,
                                hwnd,
                                format!("Running tests in {}...", test_dir.display()),
                            );
                        }
                        let result = run_tests(&test_dir, &target, &config, None, None);
                        unsafe {
                            match result {
                                Ok((passed, total)) => {
                                    let msg = format!("Tests: {}/{} passed.", passed, total);
                                    push_output_line(&terminal, hwnd, msg);
                                }
                                Err(e) => {
                                    debug_log!("[GUI] run_tests error: {}", e);
                                    push_output_line(&terminal, hwnd, format!("Test error: {}", e));
                                }
                            }
                            // ---- Test run finished: clear flag ----
                            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                            if state_ptr != 0 {
                                let state = &mut *(state_ptr as *mut AppState);
                                state.is_test_run = false;
                            }
                            PostMessageW(hwnd, WM_USER_REFRESH, 0, 0);
                            thread::sleep(Duration::from_millis(20));
                            // No phase update here – run_tests handles it.
                        }
                    });
                    return 0;
                }

                ID_CLEAN => {
                    debug_log!("[GUI] ID_CLEAN");
                    if state.compilation_in_progress {
                        push_output_line(
                            &state.terminal,
                            hwnd,
                            "Compilation in progress, cannot clean.".to_string(),
                        );
                        return 0;
                    }

                    clear_output(hwnd, &state.terminal, state.hwnd_terminal);
                    state.compilation_in_progress = true;
                    emit_phase_update("Cleaning", 0);

                    let terminal = state.terminal.clone();
                    let hwnd_main = hwnd as usize;

                    thread::spawn(move || {
                        let hwnd = hwnd_main as HWND;
                        unsafe {
                            push_output_line(&terminal, hwnd, "Cleaning target/...".to_string());
                        }
                        let result = clean_project();
                        unsafe {
                            match result {
                                Ok(()) => {
                                    push_output_line(
                                        &terminal,
                                        hwnd,
                                        "Clean completed.".to_string(),
                                    );
                                }
                                Err(e) => {
                                    debug_log!("[GUI] clean_project error: {}", e);
                                    push_output_line(
                                        &terminal,
                                        hwnd,
                                        format!("Clean error: {}", e),
                                    );
                                }
                            }
                            PostMessageW(hwnd, WM_USER_REFRESH, 0, 0);
                            thread::sleep(Duration::from_millis(20));
                            emit_phase_update("Clean complete", 100);
                        }
                    });
                    return 0;
                }

                // ---- Edit menu commands (also used by context menu) ----
                ID_EDIT_UNDO => {
                    debug_log!("[GUI] ID_EDIT_UNDO");
                    SendMessageW(state.hwnd_editor, EM_UNDO, 0, 0);
                    return 0;
                }
                ID_EDIT_REDO => {
                    debug_log!("[GUI] ID_EDIT_REDO");
                    SendMessageW(state.hwnd_editor, EM_REDO, 0, 0);
                    return 0;
                }
                ID_EDIT_CUT => {
                    debug_log!("[GUI] ID_EDIT_CUT");
                    SendMessageW(state.hwnd_editor, WM_CUT, 0, 0);
                    return 0;
                }
                ID_EDIT_COPY => {
                    debug_log!("[GUI] ID_EDIT_COPY");
                    SendMessageW(state.hwnd_editor, WM_COPY, 0, 0);
                    return 0;
                }
                ID_EDIT_PASTE => {
                    debug_log!("[GUI] ID_EDIT_PASTE");
                    SendMessageW(state.hwnd_editor, WM_PASTE, 0, 0);
                    return 0;
                }
                ID_EDIT_DELETE => {
                    debug_log!("[GUI] ID_EDIT_DELETE");
                    SendMessageW(state.hwnd_editor, WM_CLEAR, 0, 0);
                    return 0;
                }
                ID_EDIT_SELECT_ALL => {
                    debug_log!("[GUI] ID_EDIT_SELECT_ALL");
                    SendMessageW(state.hwnd_editor, EM_SETSEL, 0, -1 as LPARAM);
                    return 0;
                }

                ID_FILE_EXIT => {
                    debug_log!("[GUI] ID_FILE_EXIT received, closing window...");
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                    return 0;
                }
                _ => {}
            }

            if hwnd_from == state.hwnd_button && (wm_code as u32) == BN_CLICKED {
                debug_log!("[GUI] Run button clicked -> posting ID_RUN");
                SendMessageW(hwnd, WM_COMMAND, ID_RUN as WPARAM, 0);
                return 0;
            }

            if hwnd_from == state.hwnd_editor
                && ((wm_code as u32) == 0x0300 || (wm_code as u32) == 0x0400)
            {
                state.is_modified = true;
                debug_log!("[GUI] Text modified, is_modified = true");
                let now = Instant::now();
                if now.duration_since(state.last_change_time) > Duration::from_millis(300) {
                    state.last_change_time = now;
                    if let Some(client) = &mut state.lsp_client {
                        if let Some(path) = &state.file_path {
                            let uri = path_to_uri(Path::new(path));
                            let text = get_editor_text(state.hwnd_editor);
                            debug_log!("[GUI] EN_CHANGE: sending didChange for {}", uri);
                            let _ = client.send_change(&uri, &text);
                        }
                    }
                } else {
                    state.pending_change = true;
                }
                return 0;
            }

            return 0;
        }

        WM_INITMENUPOPUP => {
            // Update Undo/Redo menu item states based on the Rich Edit control's state.
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if state_ptr != 0 {
                let state = &*(state_ptr as *mut AppState);
                let hMenu = wparam as HMENU;
                let can_undo = SendMessageW(state.hwnd_editor, EM_CANUNDO, 0, 0) != 0;
                let can_redo = SendMessageW(state.hwnd_editor, EM_CANREDO, 0, 0) != 0;
                EnableMenuItem(
                    hMenu,
                    ID_EDIT_UNDO as u32,
                    if can_undo { MF_ENABLED } else { MF_GRAYED },
                );
                EnableMenuItem(
                    hMenu,
                    ID_EDIT_REDO as u32,
                    if can_redo { MF_ENABLED } else { MF_GRAYED },
                );
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        WM_CLOSE => {
            debug_log!("[GUI] WM_CLOSE");
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if state_ptr == 0 {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let state = &mut *(state_ptr as *mut AppState);

            // If the file is modified, ask the user what to do
            if state.is_modified {
                let filename = state
                    .file_path
                    .as_ref()
                    .and_then(|p| Path::new(p).file_name())
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_else(|| std::borrow::Cow::Borrowed("untitled"));

                let msg = format!("Do you want to save changes to '{}'?", filename);
                let wide_msg = to_wide(&msg);
                let caption = to_wide("vox - Save changes?");

                let result = MessageBoxW(
                    hwnd,
                    wide_msg.as_ptr(),
                    caption.as_ptr(),
                    MB_YESNOCANCEL | MB_ICONQUESTION,
                );

                match result {
                    IDYES => {
                        // Try to save; if it fails, show error and keep window open
                        if let Err(e) = save_file(state) {
                            let err_msg = format!("Failed to save: {}", e);
                            let wide_err = to_wide(&err_msg);
                            MessageBoxW(
                                hwnd,
                                wide_err.as_ptr(),
                                w!("Error") as LPCWSTR,
                                MB_ICONERROR,
                            );
                            return 0; // Stay open
                        }
                        // Save succeeded – allow close
                    }
                    IDNO => {
                        // Discard changes – allow close
                    }
                    IDCANCEL => {
                        // User cancelled – do not close
                        return 0;
                    }
                    _ => {}
                }
            }

            // If we reach here, we are allowed to close
            // Let the default handler destroy the window (which will send WM_DESTROY)
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        WM_USER_DIAGNOSTICS => {
            debug_log!("[GUI] WM_USER_DIAGNOSTICS");
            if lparam != 0 {
                let diags = Box::from_raw(lparam as *mut Vec<Diagnostic>);
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if state_ptr != 0 {
                    let state = &mut *(state_ptr as *mut AppState);
                    apply_diagnostics(state, *diags);
                }
            }
            return 0;
        }

        WM_USER_PHASE_UPDATE => {
            if lparam != 0 {
                let msg = Box::from_raw(lparam as *mut String);
                debug_log!("[GUI] WM_USER_PHASE_UPDATE: received message: '{}'", msg);
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if state_ptr != 0 {
                    let state = &mut *(state_ptr as *mut AppState);
                    let (phase, percent) = parse_phase_percent(&msg);
                    debug_log!(
                        "[GUI] Phase update: phase='{}', percent={}, raw='{}'",
                        phase,
                        percent,
                        msg
                    );

                    // ---- TEST STREAMING FIX: set flag based on phase ----
                    if phase == "Testing" {
                        state.is_test_run = true;
                    } else if phase == "Test complete" {
                        state.is_test_run = false;
                    }

                    if phase == "Compilation complete"
                        || phase == "Build complete"
                        || phase == "Check complete"
                        || phase == "Test complete"
                        || phase == "Clean complete"
                    {
                        state.compilation_in_progress = false;
                        debug_log!("[GUI] compilation_in_progress set to false");
                    } else {
                        state.compilation_in_progress = true;
                    }
                    // Update status text with hyphen, not en-dash
                    let status_text = format!("{}% - {}", percent, phase);
                    SetWindowTextW(state.hwnd_status_text, to_wide(&status_text).as_ptr());
                    // Update progress bar
                    SendMessageW(state.hwnd_progress, PBM_SETPOS, percent as WPARAM, 0);
                    // Force immediate redraw of the status bar area
                    RedrawWindow(
                        state.hwnd_status_text,
                        ptr::null(),
                        ptr::null_mut(),
                        RDW_INVALIDATE,
                    );
                    RedrawWindow(
                        state.hwnd_progress,
                        ptr::null(),
                        ptr::null_mut(),
                        RDW_INVALIDATE,
                    );
                } else {
                    debug_log!("[GUI] WM_USER_PHASE_UPDATE: state_ptr is null!");
                }
            } else {
                debug_log!("[GUI] WM_USER_PHASE_UPDATE: lparam is 0, ignoring.");
            }
            return 0;
        }

        // New: Timer handler for throttled refresh
        WM_TIMER => {
            if wparam == 1001 {
                // The rate-limit cooldown has expired! Flush the orphaned logs.
                KillTimer(hwnd, 1001);
                process_output_refresh(hwnd);
            }
            return 0;
        }

        WM_USER_REFRESH => {
            process_output_refresh(hwnd);
            return 0;
        }

        WM_DESTROY => {
            debug_log!("[GUI] WM_DESTROY");
            let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if state_ptr != 0 {
                let state = Box::from_raw(state_ptr as *mut AppState);
                if let Some(client) = state.lsp_client {
                    debug_log!("[GUI] Shutting down LSP");
                    let _ = client.shutdown();
                }
                DeleteObject(state.hFont as *mut c_void);
                DeleteObject(state.hBrush as *mut c_void);
                DeleteObject(state.hbrStatusBk as *mut c_void);
                DeleteObject(state.hbrEditBk as *mut c_void);
                DeleteObject(state.hbrButtonBk as *mut c_void);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }

            PostQuitMessage(0);
            return 0;
        }

        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ============================================================================
// run() entry point
// ============================================================================

pub fn run(hide_console: bool) -> Result<(), String> {
    unsafe {
        // Detach from the console if requested (e.g., when launched with no args)
        if hide_console {
            FreeConsole(); // Completely hides the console window
        }

        // Initialize OLE so that RevokeDragDrop actually works
        OleInitialize(ptr::null_mut());

        // Enable dark mode for native menus (dropdowns) – does not affect horizontal bar
        enable_dark_mode_menus();

        debug_log!("[GUI] run() started");
        let hinst = GetModuleHandleW(ptr::null());
        if hinst.is_null() {
            debug_log!("[GUI] GetModuleHandleW failed");
            return Err("Failed to get module handle".to_string());
        }

        let cursor = LoadCursorW(ptr::null_mut(), (IDC_ARROW as isize) as LPCWSTR);
        let icon = LoadIconW(ptr::null_mut(), (IDI_APPLICATION as isize) as LPCWSTR);

        let class_name = w!("voxWindowClass");
        let wc = WNDCLASSEXW {
            cbSize: mem::size_of::<WNDCLASSEXW>() as UINT,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: icon,
            hCursor: cursor,
            // Use subtle dark gray for the border (Gap Trick)
            hbrBackground: CreateSolidBrush(0x00333333),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name,
            hIconSm: ptr::null_mut(),
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            let err = GetLastError();
            debug_log!("[GUI] RegisterClassExW failed: {}", err);
            return Err(format!("Failed to register window class (error {})", err));
        }
        debug_log!("[GUI] Window class registered: {}", atom);

        let window_title = w!("vox");
        let hwnd = CreateWindowExW(
            WS_EX_ACCEPTFILES,
            class_name,
            window_title,
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            960,
            720,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null_mut(),
        );
        if hwnd.is_null() {
            let err = GetLastError();
            debug_log!("[GUI] CreateWindowExW failed: {}", err);
            return Err(format!("Failed to create main window (error {})", err));
        }
        debug_log!("[GUI] Main window HWND: {:?}", hwnd);

        // ---- Stronger dark mode enforcement ----
        let dark = 1i32;

        // Primary dark mode attribute (Windows 11)
        let hr = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const c_void,
            4,
        );
        debug_log!(
            "[GUI] DwmSetWindowAttribute dark mode (20): HRESULT = 0x{:08X}",
            hr
        );

        // Older attribute (for Windows 10 builds that need it)
        let hr = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE_OLD,
            &dark as *const _ as *const c_void,
            4,
        );
        debug_log!(
            "[GUI] DwmSetWindowAttribute dark mode (19): HRESULT = 0x{:08X}",
            hr
        );

        // Rounded corners
        let corner = DWMWCP_ROUND;
        let hr = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const c_void,
            4,
        );
        debug_log!(
            "[GUI] DwmSetWindowAttribute rounded corners: HRESULT = 0x{:08X}",
            hr
        );

        // Mica backdrop
        let backdrop = DWMSBT_MAINWINDOW;
        let hr = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const c_void,
            4,
        );
        debug_log!("[GUI] DwmSetWindowAttribute Mica: HRESULT = 0x{:08X}", hr);

        // ---- NEW: Custom title bar color (slightly lighter) ----
        const DWMWA_CAPTION_COLOR: u32 = 35;
        let caption_color = 0x00333333u32; // <-- tweak this hex value
        let hr_caption = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            &caption_color as *const _ as *const c_void,
            4,
        );
        debug_log!(
            "[GUI] DwmSetWindowAttribute caption color (35): HRESULT = 0x{:08X}",
            hr_caption
        );

        // ---- Force dark menu bar theme ----
        let hr_theme = SetWindowTheme(hwnd, w!("DarkMode_Explorer"), ptr::null());
        debug_log!("[GUI] SetWindowTheme(main) HRESULT = 0x{:08X}", hr_theme);

        // Re-apply immersive dark mode (sometimes it needs a second push)
        let hr = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const c_void,
            4,
        );
        debug_log!(
            "[GUI] DwmSetWindowAttribute dark mode (20) re-apply: HRESULT = 0x{:08X}",
            hr
        );

        // Show the window
        ShowWindow(hwnd, 1);
        debug_log!("[GUI] ShowWindow called");

        // Redraw the menu bar (will be painted via UAH messages)
        DrawMenuBar(hwnd);

        // ---- Message pump: no accelerator table, no TranslateAccelerator ----
        let mut msg = MSG {
            hwnd: ptr::null_mut(),
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: POINT { x: 0, y: 0 },
        };

        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        debug_log!("[GUI] run() exiting");
        OleUninitialize();
        Ok(())
    }
}

// ============================================================================
// Constants for CreateWindowEx
// ============================================================================

const CW_USEDEFAULT: i32 = -2147483648; // 0x80000000
