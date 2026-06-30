// src/shell/macos_gui.rs – Native macOS GUI
// FINAL: Polished layout, throttle debounce, fixed null_mut type inference.

#![allow(
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    non_camel_case_types,
    unsafe_op_in_unsafe_fn,
    static_mut_refs
)]

use std::ffi::{CStr, CString, c_void};
use std::fs;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::lsp::{LspClient, path_to_uri};
use super::runner::{build_file, check_file, clean_project, compile_and_run_file, run_tests};
use super::terminal::TerminalBuffer;
use crate::diagnostic::{
    Diagnostic, Level, emit_phase_update, set_gui_phase_callback, set_gui_refresh_callback,
    set_gui_terminal,
};
use crate::{CacheConfig, find_vox_root, host_triple};

// ============================================================================
// debug_log macro
// ============================================================================

macro_rules! debug_log {
    ($($arg:tt)*) => {
        crate::diagnostic::debug_log(format!($($arg)*));
    };
}

// ============================================================================
// Objective‑C Runtime FFI
// ============================================================================

#[link(name = "objc")]
#[link(name = "Foundation", kind = "framework")]
#[link(name = "AppKit", kind = "framework")]
#[allow(clashing_extern_declarations)]
unsafe extern "C" {
    fn objc_msgSend(obj: id, sel: SEL, ...) -> id;
    fn objc_msgSend_stret(ret: *mut c_void, obj: id, sel: SEL, ...) -> id;
    fn objc_msgSendSuper(obj: *mut objc_super, sel: SEL, ...) -> id;

    // Explicit f64 binding for setMinValue:, setMaxValue:, setDoubleValue:
    #[link_name = "objc_msgSend"]
    fn objc_msgSend_f64(obj: id, sel: SEL, val: f64) -> id;

    // Explicit binding for performSelector:withObject:afterDelay: (takes f64 delay)
    #[link_name = "objc_msgSend"]
    fn objc_msgSend_perform_delay(obj: id, sel: SEL, method_sel: SEL, arg: id, delay: f64) -> id;

    fn sel_registerName(name: *const c_char) -> SEL;
    fn objc_getClass(name: *const c_char) -> id;
    fn objc_allocateClassPair(superclass: id, name: *const c_char, extraBytes: usize) -> id;
    fn objc_registerClassPair(cls: id);
    fn class_addMethod(cls: id, sel: SEL, imp: *const c_void, types: *const c_char) -> BOOL;
    fn class_addIvar(
        cls: id,
        name: *const c_char,
        size: usize,
        alignment: u8,
        types: *const c_char,
    ) -> BOOL;
    fn objc_setAssociatedObject(obj: id, key: *const c_void, value: id, policy: usize);
    fn objc_getAssociatedObject(obj: id, key: *const c_void) -> id;
}

// ============================================================================
// Types
// ============================================================================

type id = *mut c_void;
type SEL = *const c_void;
type BOOL = i8;
type NSInteger = i64;
type NSUInteger = usize;
type CGFloat = f64;
type NSRect = objc_sys::NSRect;
type NSSize = objc_sys::NSSize;
type NSPoint = objc_sys::NSPoint;
type NSModalResponse = NSInteger;

mod objc_sys {
    use super::*;
    #[repr(C)]
    #[derive(Debug)]
    pub struct NSPoint {
        pub x: CGFloat,
        pub y: CGFloat,
    }
    #[repr(C)]
    #[derive(Debug)]
    pub struct NSSize {
        pub width: CGFloat,
        pub height: CGFloat,
    }
    #[repr(C)]
    pub struct NSRect {
        pub origin: NSPoint,
        pub size: NSSize,
    }
}

// For objc_msgSendSuper
#[repr(C)]
struct objc_super {
    receiver: id,
    super_class: id,
}

// ============================================================================
// Helper: call objc_msgSend_stret for selectors returning NSRect
// ============================================================================

unsafe fn rect_for_selector_stret(obj: id, sel: SEL) -> NSRect {
    if obj == ptr::null_mut() {
        panic!("rect_for_selector_stret: obj is null");
    }
    let mut rect: NSRect = std::mem::zeroed();
    objc_msgSend_stret(&mut rect as *mut NSRect as *mut c_void, obj, sel);
    rect
}

// ============================================================================
// Helper Macros
// ============================================================================

macro_rules! cls {
    ($name:expr) => {{
        static mut C: id = 0 as id;
        if C == 0 as id {
            let cstr = std::ffi::CString::new($name).unwrap();
            C = objc_getClass(cstr.as_ptr());
        }
        C
    }};
}
macro_rules! sel {
    ($name:expr) => {{
        static mut S: SEL = 0 as SEL;
        if S == 0 as SEL {
            let cstr = std::ffi::CString::new($name).unwrap();
            S = sel_registerName(cstr.as_ptr());
        }
        S
    }};
}
macro_rules! nsstring {
    ($s:expr) => {{
        let cls = cls!("NSString");
        let s: &str = $s.as_ref();
        // Remove null bytes to avoid panic in CString (they can appear in terminal output)
        let clean: String = s.chars().filter(|&c| c != '\0').collect();
        let cstr = std::ffi::CString::new(clean).unwrap();
        objc_msgSend(cls, sel!("stringWithUTF8String:"), cstr.as_ptr())
    }};
}
macro_rules! nsarray {
    ($($obj:expr),*) => {{
        let cls = cls!("NSArray");
        let array = objc_msgSend(cls, sel!("array"));
        $(
            let _: id = objc_msgSend(array, sel!("arrayByAddingObject:"), $obj);
        )*
        array
    }};
}
macro_rules! msg_send_void {
    ($obj:expr, $sel:expr $(, $arg:expr)*) => {{
        if $obj == ptr::null_mut() {
            panic!("msg_send_void: object is null");
        }
        let _: id = objc_msgSend($obj, $sel $(, $arg)*);
    }};
}
macro_rules! msg_send_bool {
    ($obj:expr, $sel:expr $(, $arg:expr)*) => {{
        if $obj == ptr::null_mut() {
            panic!("msg_send_bool: object is null");
        }
        objc_msgSend($obj, $sel $(, $arg)*) as BOOL
    }};
}
macro_rules! msg_send_int {
    ($obj:expr, $sel:expr $(, $arg:expr)*) => {{
        if $obj == ptr::null_mut() {
            panic!("msg_send_int: object is null");
        }
        objc_msgSend($obj, $sel $(, $arg)*) as NSInteger
    }};
}
macro_rules! msg_send_rect {
    ($obj:expr, $sel:expr) => {{
        if $obj == ptr::null_mut() {
            panic!("msg_send_rect: object is null");
        }
        rect_for_selector_stret($obj, $sel)
    }};
}
macro_rules! msg_send_id {
    ($obj:expr, $sel:expr $(, $arg:expr)*) => {{
        if $obj == ptr::null_mut() {
            panic!("msg_send_id: object is null");
        }
        objc_msgSend($obj, $sel $(, $arg)*)
    }};
}

// ============================================================================
// Constants
// ============================================================================

const NSWindowStyleMaskTitled: NSInteger = 1 << 0;
const NSWindowStyleMaskClosable: NSInteger = 1 << 1;
const NSWindowStyleMaskMiniaturizable: NSInteger = 1 << 2;
const NSWindowStyleMaskResizable: NSInteger = 1 << 3;
const NSBackingStoreBuffered: NSInteger = 2;
const NSApplicationActivationPolicyRegular: NSInteger = 0;
const NSModalResponseOK: NSInteger = 1;
const NSRoundedBezelStyle: NSInteger = 1;
const NSMomentaryPushInButton: NSInteger = 0;
const NSControlStateValueOn: NSInteger = 1;
const NSViewWidthSizable: NSUInteger = 1 << 1;
const NSViewHeightSizable: NSUInteger = 1 << 4;
const NSViewMinXMargin: NSUInteger = 1 << 0;
const NSViewMaxXMargin: NSUInteger = 1 << 2;
const NSViewMinYMargin: NSUInteger = 1 << 3;
const NSViewMaxYMargin: NSUInteger = 1 << 5;
const NSLeftTextAlignment: NSInteger = 0;
const NSDragOperationCopy: NSInteger = 1;
const NSAppearanceNameDarkAqua: &str = "NSAppearanceNameDarkAqua";
const NSBezelBorder: NSInteger = 1;

// NSProgressIndicator styles
const NSProgressIndicatorStyleBar: NSInteger = 0;
const NSProgressIndicatorStyleSpinning: NSInteger = 1;

const ID_FILE_OPEN: u16 = 1001;
const ID_FILE_SAVE: u16 = 1002;
const ID_FILE_EXIT: u16 = 1003;
const ID_RUN: u16 = 1004;
const ID_BUILD_DEBUG: u16 = 1005;
const ID_BUILD_RELEASE: u16 = 1006;
const ID_CLEAN: u16 = 1007;
const ID_TEST: u16 = 1008;
const ID_CHECK: u16 = 1009;
const ID_EDIT_UNDO: u16 = 2001;
const ID_EDIT_REDO: u16 = 2002;
const ID_EDIT_CUT: u16 = 2003;
const ID_EDIT_COPY: u16 = 2004;
const ID_EDIT_PASTE: u16 = 2005;
const ID_EDIT_DELETE: u16 = 2006;
const ID_EDIT_SELECT_ALL: u16 = 2007;

const NOTIFICATION_REFRESH: &str = "VoxRefreshNotification";
const NOTIFICATION_DIAGNOSTICS: &str = "VoxDiagnosticsNotification";
const NOTIFICATION_PHASE_UPDATE: &str = "VoxPhaseUpdateNotification";
const APP_STATE_KEY: &str = "VoxAppStateKey";

// ============================================================================
// AppState
// ============================================================================

struct AppState {
    app: id,
    window: id,
    window_delegate: id,
    editor_scroll: id,
    editor: id,
    terminal_scroll: id,
    terminal: id,
    button: id,
    status: id,
    progress_bar: id,
    menu_bar: id,
    file_path: Option<PathBuf>,
    is_modified: bool,
    terminal_buf: Arc<Mutex<TerminalBuffer>>,
    last_refresh: Instant,
    pending_refresh: bool,
    processing_refresh: bool,
    lsp: Option<LspClient>,
    diagnostics: Vec<Diagnostic>,
    compilation_in_progress: bool,
    was_at_bottom: bool,
    controller: id,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            app: ptr::null_mut(),
            window: ptr::null_mut(),
            window_delegate: ptr::null_mut(),
            editor_scroll: ptr::null_mut(),
            editor: ptr::null_mut(),
            terminal_scroll: ptr::null_mut(),
            terminal: ptr::null_mut(),
            button: ptr::null_mut(),
            status: ptr::null_mut(),
            progress_bar: ptr::null_mut(),
            menu_bar: ptr::null_mut(),
            file_path: None,
            is_modified: false,
            terminal_buf: Arc::new(Mutex::new(TerminalBuffer::new())),
            last_refresh: Instant::now(),
            pending_refresh: false,
            processing_refresh: false,
            lsp: None,
            diagnostics: Vec::new(),
            compilation_in_progress: false,
            was_at_bottom: false,
            controller: ptr::null_mut(),
        }
    }
}

static mut APP_STATE_PTR: *mut AppState = ptr::null_mut();

unsafe fn get_state() -> &'static mut AppState {
    let state_ptr = APP_STATE_PTR;
    if state_ptr.is_null() {
        panic!("AppState not initialized");
    }
    &mut *state_ptr
}

// ============================================================================
// Helpers
// ============================================================================

fn parse_phase_percent(msg: &str) -> (&str, usize) {
    if let Some(start) = msg.rfind('(') {
        if let Some(end) = msg.rfind(')')
            && start < end
        {
            let phase = msg[..start].trim();
            let percent_str = &msg[start + 1..end];
            if let Ok(percent) = percent_str.trim_end_matches('%').parse::<usize>() {
                return (phase, percent);
            }
        }
    }
    (msg.trim(), 0)
}

unsafe fn get_editor_text(editor: id) -> String {
    let string = msg_send_id!(editor, sel!("string"));
    if string == ptr::null_mut() {
        return String::new();
    }
    let utf8 = msg_send_id!(string, sel!("UTF8String")) as *const c_char;
    if utf8.is_null() {
        return String::new();
    }
    CStr::from_ptr(utf8).to_string_lossy().into_owned()
}

unsafe fn set_editor_text(editor: id, text: &str) {
    let nsstr = nsstring!(text);
    msg_send_void!(editor, sel!("setString:"), nsstr);
}

unsafe fn update_status(state: &mut AppState, phase: &str, percent: usize) {
    let text = format!("{}% – {}", percent, phase);
    let ns_text = nsstring!(&text);
    msg_send_void!(state.status, sel!("setStringValue:"), ns_text);
    debug_log!(
        "[PROGRESS] update_status: percent = {}, phase = {}",
        percent,
        phase
    );
    // Use explicit f64 binding
    objc_msgSend_f64(state.progress_bar, sel!("setDoubleValue:"), percent as f64);
    msg_send_void!(state.progress_bar, sel!("setNeedsDisplay:"), 1);
    msg_send_void!(state.status, sel!("displayIfNeeded"));
    msg_send_void!(state.progress_bar, sel!("displayIfNeeded"));
    msg_send_void!(state.window, sel!("flushWindow"));
}

unsafe fn load_file(state: &mut AppState, path: &Path) -> Result<(), String> {
    debug_log!("[GUI] load_file: {:?}", path);
    let content = fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;
    set_editor_text(state.editor, &content);
    state.file_path = Some(path.to_path_buf());
    state.is_modified = false;

    let title = format!("vox - {}", path.file_name().unwrap().to_string_lossy());
    let ns_title = nsstring!(&title);
    msg_send_void!(state.window, sel!("setTitle:"), ns_title);

    if state.lsp.is_none() {
        start_lsp(state);
    } else if let Some(client) = &mut state.lsp {
        let uri = path_to_uri(path);
        debug_log!("[GUI] Sending didOpen for {}", uri);
        let _ = client.send_open(&uri, &content);
    }
    Ok(())
}

unsafe fn save_file(state: &mut AppState) -> Result<(), String> {
    let path = state.file_path.as_ref().ok_or("No file open")?;
    debug_log!("[GUI] save_file: {:?}", path);
    let content = get_editor_text(state.editor);
    fs::write(path, content).map_err(|e| format!("Failed to save: {}", e))?;
    state.is_modified = false;
    Ok(())
}

unsafe fn start_lsp(state: &mut AppState) {
    debug_log!("[GUI] Starting LSP...");
    match LspClient::start() {
        Ok(mut client) => {
            debug_log!("[GUI] LSP client started");
            let _ = client.send_initialize("file://");
            if let Some(path) = &state.file_path {
                let uri = path_to_uri(path);
                let content = get_editor_text(state.editor);
                let _ = client.send_open(&uri, &content);
            }
            state.lsp = Some(client);
        }
        Err(e) => {
            debug_log!("[GUI] Failed to start LSP: {}", e);
            let msg = format!("Failed to start LSP: {}", e);
            push_output_line(&state.terminal_buf, &msg);
        }
    }
}

unsafe fn notify_diagnostics(diags: Vec<Diagnostic>) {
    use std::sync::Mutex;
    static DIAGNOSTICS: Mutex<Option<Vec<Diagnostic>>> = Mutex::new(None);
    {
        let mut lock = DIAGNOSTICS.lock().unwrap();
        *lock = Some(diags);
    }
    let center = msg_send_id!(cls!("NSNotificationCenter"), sel!("defaultCenter"));
    let name = nsstring!(NOTIFICATION_DIAGNOSTICS);
    let notif = msg_send_id!(
        cls!("NSNotification"),
        sel!("notificationWithName:object:"),
        name,
        std::ptr::null_mut::<c_void>()
    );
    msg_send_void!(center, sel!("postNotification:"), notif);
}

unsafe fn apply_diagnostics(state: &mut AppState) {
    for diag in &state.diagnostics {
        let level = match diag.level {
            Level::Error => "error",
            Level::Warning => "warning",
            _ => "info",
        };
        let msg = format!(
            "[{}] {}: {}",
            level,
            diag.message,
            diag.span
                .map_or("".to_string(), |s| format!("at {}:{}", s.line, s.col))
        );
        push_output_line(&state.terminal_buf, &msg);
    }
}

unsafe fn push_output_line(terminal_buf: &Arc<Mutex<TerminalBuffer>>, line: &str) {
    {
        let mut term = terminal_buf.lock().unwrap();
        term.push(line.to_string());
    }
    request_refresh();
}

// Clear output – uses setString: directly, avoids NSRange struct
unsafe fn clear_output() {
    let state = get_state();
    let mut term = state.terminal_buf.lock().unwrap();
    term.clear();
    // Directly set the string on the terminal – safe over variadic objc_msgSend
    msg_send_void!(state.terminal, sel!("setString:"), nsstring!(""));
}

unsafe fn request_refresh() {
    let state = get_state();
    if state.pending_refresh {
        return;
    }
    state.pending_refresh = true;
    let controller = state.controller;
    if controller != ptr::null_mut() {
        msg_send_void!(
            controller,
            sel!("performSelectorOnMainThread:withObject:waitUntilDone:"),
            sel!("processRefresh"),
            std::ptr::null_mut::<c_void>(),
            0
        );
    }
}

// ============================================================================
// ANSI color parsing and attributed string appending
// ============================================================================

unsafe fn append_colored_chunk(text_storage: id, text: &str, color: id) {
    let ns_text = nsstring!(text);
    let attr_dict = msg_send_id!(cls!("NSMutableDictionary"), sel!("dictionary"));
    msg_send_void!(
        attr_dict,
        sel!("setObject:forKey:"),
        color,
        nsstring!("NSColor")
    );
    let font = msg_send_id!(
        cls!("NSFont"),
        sel!("fontWithName:size:"),
        nsstring!("Menlo"),
        14.0
    );
    msg_send_void!(
        attr_dict,
        sel!("setObject:forKey:"),
        font,
        nsstring!("NSFont")
    );

    let attr_string_cls = cls!("NSAttributedString");
    let attr_string = msg_send_id!(attr_string_cls, sel!("alloc"));
    let attr_string = msg_send_id!(
        attr_string,
        sel!("initWithString:attributes:"),
        ns_text,
        attr_dict
    );
    msg_send_void!(text_storage, sel!("appendAttributedString:"), attr_string);
}

unsafe fn append_ansi_text(text_storage: id, ansi_string: &str) {
    let mut current_color = msg_send_id!(cls!("NSColor"), sel!("whiteColor"));
    let mut parts = ansi_string.split('\x1b');

    if let Some(first) = parts.next() {
        if !first.is_empty() {
            append_colored_chunk(text_storage, first, current_color);
        }
    }

    for part in parts {
        if part.starts_with('[') {
            if let Some(m_idx) = part.find('m') {
                let code_str = &part[1..m_idx];
                let text_str = &part[m_idx + 1..];
                current_color = match code_str {
                    "30" => msg_send_id!(cls!("NSColor"), sel!("blackColor")),
                    "31" | "91" => msg_send_id!(cls!("NSColor"), sel!("systemRedColor")),
                    "32" | "92" => msg_send_id!(cls!("NSColor"), sel!("systemGreenColor")),
                    "33" | "93" => msg_send_id!(cls!("NSColor"), sel!("systemYellowColor")),
                    "34" | "94" => msg_send_id!(cls!("NSColor"), sel!("systemBlueColor")),
                    "35" | "95" => msg_send_id!(cls!("NSColor"), sel!("systemPurpleColor")),
                    "36" | "96" => msg_send_id!(cls!("NSColor"), sel!("systemCyanColor")),
                    "37" | "97" | "0" => msg_send_id!(cls!("NSColor"), sel!("whiteColor")),
                    "90" => msg_send_id!(cls!("NSColor"), sel!("systemGrayColor")),
                    _ => current_color,
                };
                if !text_str.is_empty() {
                    append_colored_chunk(text_storage, text_str, current_color);
                }
            } else {
                append_colored_chunk(text_storage, part, current_color);
            }
        } else {
            append_colored_chunk(text_storage, part, current_color);
        }
    }
}

// ============================================================================
// Process refresh – with cancellation to avoid spam
// ============================================================================

unsafe fn process_output_refresh() {
    let state = get_state();
    if state.processing_refresh {
        return;
    }

    let now = Instant::now();
    if now.duration_since(state.last_refresh) < Duration::from_millis(200) {
        state.pending_refresh = true;
        let controller = state.controller;
        if controller != ptr::null_mut() {
            // Cancel any previously scheduled delayed refreshes to prevent pile‑up
            // Use explicit type for null_mut to avoid inference errors
            let null_id: id = std::ptr::null_mut();
            msg_send_void!(
                cls!("NSObject"),
                sel!("cancelPreviousPerformRequestsWithTarget:selector:object:"),
                controller,
                sel!("processRefresh"),
                null_id
            );

            // Schedule exactly ONE new delayed refresh
            objc_msgSend_perform_delay(
                controller,
                sel!("performSelector:withObject:afterDelay:"),
                sel!("processRefresh"),
                ptr::null_mut::<c_void>(),
                0.2,
            );
        }
        return;
    }

    state.pending_refresh = false;
    state.processing_refresh = true;
    state.last_refresh = now;

    let lines = {
        let mut term = state.terminal_buf.lock().unwrap();
        term.take_all()
    };
    if lines.is_empty() {
        state.processing_refresh = false;
        return;
    }

    let was_at_bottom = is_scroll_at_bottom(state.terminal_scroll);
    let combined = lines.join("\r\n") + "\r\n";
    let text_storage = msg_send_id!(state.terminal, sel!("textStorage"));

    msg_send_void!(text_storage, sel!("beginEditing"));
    append_ansi_text(text_storage, &combined);
    let length = msg_send_int!(text_storage, sel!("length")) as usize;
    const MAX_LENGTH: usize = 10_000_000; // 10 million chars – almost infinite
    const KEEP_LENGTH: usize = 250_000; // keep this many chars when trimming

    if length > MAX_LENGTH {
        // Trim from the beginning, keeping the last KEEP_LENGTH characters.
        let full_string = msg_send_id!(state.terminal, sel!("string"));
        let full = msg_send_id!(full_string, sel!("UTF8String")) as *const c_char;
        if !full.is_null() {
            let raw = CStr::from_ptr(full).to_string_lossy();
            let total_len = raw.len();
            if total_len > KEEP_LENGTH {
                let start = total_len - KEEP_LENGTH;
                let tail = &raw[start..];
                msg_send_void!(state.terminal, sel!("setString:"), nsstring!(tail));
            }
        }
        msg_send_void!(text_storage, sel!("endEditing"));
        state.processing_refresh = false;
        return;
    }
    msg_send_void!(text_storage, sel!("endEditing"));

    if was_at_bottom {
        msg_send_void!(
            state.terminal,
            sel!("scrollToEndOfDocument:"),
            std::ptr::null_mut::<c_void>()
        );
    }

    state.processing_refresh = false;
}

unsafe fn is_scroll_at_bottom(scroll_view: id) -> bool {
    let clip_view = msg_send_id!(scroll_view, sel!("contentView"));
    let doc_view = msg_send_id!(scroll_view, sel!("documentView"));
    if doc_view == ptr::null_mut() {
        return true;
    }
    let visible = msg_send_rect!(clip_view, sel!("visibleRect"));
    let doc_bounds = msg_send_rect!(doc_view, sel!("bounds"));
    let doc_height = doc_bounds.size.height;
    let visible_height = visible.size.height;
    let visible_y = visible.origin.y;
    let bottom_threshold = doc_height - visible_height - 5.0;
    visible_y >= bottom_threshold
}

// ============================================================================
// Callbacks for diagnostic module
// ============================================================================

fn refresh_callback() {
    unsafe {
        let state = get_state();
        let controller = state.controller;
        if controller != ptr::null_mut() {
            msg_send_void!(
                controller,
                sel!("performSelectorOnMainThread:withObject:waitUntilDone:"),
                sel!("processRefresh"),
                std::ptr::null_mut::<c_void>(),
                0
            );
        }
    }
}

fn phase_callback(phase: &'static str, percent: usize) {
    unsafe {
        let center = msg_send_id!(cls!("NSNotificationCenter"), sel!("defaultCenter"));
        let name = nsstring!(NOTIFICATION_PHASE_UPDATE);
        let user_info = msg_send_id!(cls!("NSMutableDictionary"), sel!("dictionary"));
        let ns_phase = nsstring!(phase);
        let ns_percent = msg_send_id!(
            cls!("NSNumber"),
            sel!("numberWithInteger:"),
            percent as NSInteger
        );
        msg_send_void!(
            user_info,
            sel!("setObject:forKey:"),
            ns_phase,
            nsstring!("phase")
        );
        msg_send_void!(
            user_info,
            sel!("setObject:forKey:"),
            ns_percent,
            nsstring!("percent")
        );
        let notif = msg_send_id!(
            cls!("NSNotification"),
            sel!("notificationWithName:object:userInfo:"),
            name,
            std::ptr::null_mut::<c_void>(),
            user_info
        );
        msg_send_void!(center, sel!("postNotification:"), notif);
    }
}

// ============================================================================
// Controller Class
// ============================================================================

unsafe fn create_controller_class() -> id {
    let superclass = cls!("NSObject");
    let class_name = CString::new("VoxController").unwrap();
    let cls = objc_allocateClassPair(superclass, class_name.as_ptr(), 0);
    if cls == ptr::null_mut() {
        return ptr::null_mut();
    }

    extern "C" fn process_refresh(_self: id, _cmd: SEL) {
        unsafe {
            process_output_refresh();
        }
    }

    extern "C" fn process_diagnostics(_self: id, _cmd: SEL, _notification: id) {
        unsafe {
            let state = get_state();
            use std::sync::Mutex;
            static DIAGNOSTICS: Mutex<Option<Vec<Diagnostic>>> = Mutex::new(None);
            let mut lock = DIAGNOSTICS.lock().unwrap();
            if let Some(diags) = lock.take() {
                state.diagnostics = diags;
                apply_diagnostics(state);
            }
        }
    }

    extern "C" fn update_status_ui(_self: id, _cmd: SEL, user_info: id) {
        unsafe {
            let state = get_state();
            if user_info != ptr::null_mut() {
                let phase_obj = msg_send_id!(user_info, sel!("objectForKey:"), nsstring!("phase"));
                let percent_obj =
                    msg_send_id!(user_info, sel!("objectForKey:"), nsstring!("percent"));

                if phase_obj != ptr::null_mut() {
                    let utf8 = msg_send_id!(phase_obj, sel!("UTF8String")) as *const c_char;
                    if !utf8.is_null() {
                        let phase = CStr::from_ptr(utf8).to_string_lossy().into_owned();
                        // Get the numeric percent from the notification (important!)
                        let percent = if percent_obj != ptr::null_mut() {
                            msg_send_int!(percent_obj, sel!("integerValue")) as usize
                        } else {
                            0
                        };

                        // Parse the phase string for a possible legacy phase name
                        let (phase_str, _pct) = parse_phase_percent(&phase);
                        // _pct is ignored; we use the real 'percent' from the notification.

                        if phase_str == "Compilation complete"
                            || phase_str == "Build complete"
                            || phase_str == "Check complete"
                            || phase_str == "Test complete"
                            || phase_str == "Clean complete"
                        {
                            state.compilation_in_progress = false;
                        } else {
                            state.compilation_in_progress = true;
                        }

                        // Use the correct 'percent' from the notification
                        update_status(state, phase_str, percent);
                    }
                }
            }
        }
    }

    extern "C" fn process_phase_update(_self: id, _cmd: SEL, notification: id) {
        unsafe {
            let user_info = msg_send_id!(notification, sel!("userInfo"));
            msg_send_void!(
                _self,
                sel!("performSelectorOnMainThread:withObject:waitUntilDone:"),
                sel!("updateStatusUI:"),
                user_info,
                0
            );
        }
    }

    extern "C" fn handle_menu_command(_self: id, _cmd: SEL, sender: id) {
        unsafe {
            let tag = msg_send_int!(sender, sel!("tag")) as u16;
            let state = get_state();
            match tag {
                999 => {
                    debug_log!("[GUI_DEBUG] Debug menu clicked!");
                    push_output_line(&state.terminal_buf, "Debug menu clicked.");
                }
                ID_FILE_OPEN => {
                    debug_log!("[GUI_DEBUG] File->Open menu clicked");
                    let panel = msg_send_id!(cls!("NSOpenPanel"), sel!("openPanel"));
                    msg_send_void!(panel, sel!("setAllowsMultipleSelection:"), 0);
                    msg_send_void!(panel, sel!("setCanChooseDirectories:"), 0);
                    msg_send_void!(panel, sel!("setCanChooseFiles:"), 1);
                    msg_send_void!(
                        panel,
                        sel!("setAllowedFileTypes:"),
                        nsarray!(nsstring!("vx"))
                    );
                    let response = msg_send_int!(panel, sel!("runModal"));
                    if response == NSModalResponseOK {
                        let urls = msg_send_id!(panel, sel!("URLs"));
                        if urls != ptr::null_mut() {
                            let url = msg_send_id!(urls, sel!("firstObject"));
                            if url != ptr::null_mut() {
                                let path_str = msg_send_id!(url, sel!("path"));
                                if path_str != ptr::null_mut() {
                                    let utf8 =
                                        msg_send_id!(path_str, sel!("UTF8String")) as *const c_char;
                                    if !utf8.is_null() {
                                        let path =
                                            CStr::from_ptr(utf8).to_string_lossy().into_owned();
                                        let p = Path::new(&path);
                                        if let Err(e) = load_file(state, p) {
                                            push_output_line(
                                                &state.terminal_buf,
                                                &format!("Failed to load: {}", e),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                ID_FILE_SAVE => {
                    debug_log!("[GUI_DEBUG] File->Save menu clicked");
                    if let Err(e) = save_file(state) {
                        push_output_line(&state.terminal_buf, &format!("Save failed: {}", e));
                    } else {
                        state.is_modified = false;
                        push_output_line(&state.terminal_buf, "File saved.");
                    }
                }
                ID_RUN => {
                    debug_log!("[GUI_DEBUG] Run menu clicked");
                    if state.compilation_in_progress {
                        push_output_line(&state.terminal_buf, "Compilation already in progress.");
                        return;
                    }
                    if let Some(_path) = &state.file_path {
                        if let Err(e) = save_file(state) {
                            push_output_line(&state.terminal_buf, &format!("Save failed: {}", e));
                            return;
                        }
                    }
                    trigger_run();
                }
                ID_BUILD_DEBUG => trigger_build(false),
                ID_BUILD_RELEASE => trigger_build(true),
                ID_CHECK => trigger_check(),
                ID_TEST => trigger_test(),
                ID_CLEAN => trigger_clean(),
                ID_FILE_EXIT => {
                    debug_log!("[GUI_DEBUG] Quit menu clicked");
                    msg_send_void!(state.window, sel!("close"));
                }
                ID_EDIT_UNDO => {
                    msg_send_void!(state.editor, sel!("undo:"), std::ptr::null_mut::<c_void>())
                }
                ID_EDIT_REDO => {
                    msg_send_void!(state.editor, sel!("redo:"), std::ptr::null_mut::<c_void>())
                }
                ID_EDIT_CUT => {
                    msg_send_void!(state.editor, sel!("cut:"), std::ptr::null_mut::<c_void>())
                }
                ID_EDIT_COPY => {
                    let first_responder = msg_send_id!(state.window, sel!("firstResponder"));
                    if first_responder == state.terminal {
                        msg_send_void!(
                            state.terminal,
                            sel!("copy:"),
                            std::ptr::null_mut::<c_void>()
                        );
                    } else {
                        msg_send_void!(state.editor, sel!("copy:"), std::ptr::null_mut::<c_void>());
                    }
                }
                ID_EDIT_PASTE => {
                    msg_send_void!(state.editor, sel!("paste:"), std::ptr::null_mut::<c_void>())
                }
                ID_EDIT_DELETE => msg_send_void!(
                    state.editor,
                    sel!("delete:"),
                    std::ptr::null_mut::<c_void>()
                ),
                ID_EDIT_SELECT_ALL => msg_send_void!(
                    state.editor,
                    sel!("selectAll:"),
                    std::ptr::null_mut::<c_void>()
                ),
                _ => {}
            }
        }
    }

    extern "C" fn run_button_clicked(_self: id, _cmd: SEL, _sender: id) {
        unsafe {
            debug_log!("[GUI_DEBUG] Run button clicked");
            let state = get_state();
            if state.compilation_in_progress {
                push_output_line(&state.terminal_buf, "Compilation already in progress.");
                return;
            }
            if let Some(_path) = &state.file_path {
                if let Err(e) = save_file(state) {
                    push_output_line(&state.terminal_buf, &format!("Save failed: {}", e));
                    return;
                }
            }
            trigger_run();
        }
    }

    let imp_refresh = process_refresh as *const c_void;
    let imp_diag = process_diagnostics as *const c_void;
    let imp_phase = process_phase_update as *const c_void;
    let imp_menu = handle_menu_command as *const c_void;
    let imp_status_ui = update_status_ui as *const c_void;
    let imp_run = run_button_clicked as *const c_void;

    class_addMethod(
        cls,
        sel!("processRefresh"),
        imp_refresh,
        "v@:\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("processDiagnostics:"),
        imp_diag,
        "v@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("processPhaseUpdate:"),
        imp_phase,
        "v@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("handleMenuCommand:"),
        imp_menu,
        "v@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("updateStatusUI:"),
        imp_status_ui,
        "v@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("runButtonClicked:"),
        imp_run,
        "v@:@\0".as_ptr() as *const c_char,
    );

    objc_registerClassPair(cls);
    cls
}

// ============================================================================
// Window Delegate Class
// ============================================================================

unsafe fn create_window_delegate_class() -> id {
    let superclass = cls!("NSObject");
    let class_name = CString::new("VoxWindowDelegate").unwrap();
    let cls = objc_allocateClassPair(superclass, class_name.as_ptr(), 0);
    if cls == ptr::null_mut() {
        return ptr::null_mut();
    }

    extern "C" fn window_will_resize(
        _self: id,
        _cmd: SEL,
        _sender: id,
        frame_size: NSSize,
    ) -> NSSize {
        unsafe {
            let state = get_state();
            let content = msg_send_id!(state.window, sel!("contentView"));
            let bounds = msg_send_rect!(content, sel!("bounds"));
            let width = bounds.size.width;
            let height = bounds.size.height;

            let button_height: CGFloat = 28.0;
            let status_height: CGFloat = 22.0;
            let gap: CGFloat = 4.0;
            let editor_height = (height - status_height - button_height - gap * 3.0) * 0.6;
            let terminal_height =
                height - status_height - button_height - gap * 3.0 - editor_height;

            let editor_rect = NSRect {
                origin: NSPoint {
                    x: gap,
                    y: terminal_height + button_height + gap * 2.0 + gap,
                },
                size: NSSize {
                    width: width - gap * 2.0,
                    height: editor_height,
                },
            };
            let terminal_rect = NSRect {
                origin: NSPoint {
                    x: gap,
                    y: button_height + gap * 2.0 + gap,
                },
                size: NSSize {
                    width: width - gap * 2.0,
                    height: terminal_height,
                },
            };

            // ---- NEW FIXED LAYOUT ----
            let button_width: CGFloat = 80.0;
            let status_width: CGFloat = 250.0; // Fixed width for status text

            // Progress bar takes up all the remaining middle space
            let progress_width = (width - gap * 4.0 - button_width - status_width).max(50.0);

            let status_rect = NSRect {
                origin: NSPoint { x: gap, y: gap },
                size: NSSize {
                    width: status_width,
                    height: status_height,
                },
            };
            let progress_rect = NSRect {
                origin: NSPoint {
                    x: gap + status_width + gap,
                    y: gap,
                },
                size: NSSize {
                    width: progress_width,
                    height: status_height,
                },
            };
            let button_rect = NSRect {
                origin: NSPoint {
                    x: width - button_width - gap,
                    y: gap,
                },
                size: NSSize {
                    width: button_width,
                    height: button_height,
                },
            };
            // ---- END NEW LAYOUT ----

            msg_send_void!(state.editor_scroll, sel!("setFrame:"), editor_rect);
            msg_send_void!(state.terminal_scroll, sel!("setFrame:"), terminal_rect);
            msg_send_void!(state.button, sel!("setFrame:"), button_rect);
            msg_send_void!(state.status, sel!("setFrame:"), status_rect);
            msg_send_void!(state.progress_bar, sel!("setFrame:"), progress_rect);

            return frame_size;
        }
    }

    extern "C" fn window_should_close(_self: id, _cmd: SEL, _sender: id) -> BOOL {
        unsafe {
            let state = get_state();
            if state.is_modified {
                let alert = msg_send_id!(cls!("NSAlert"), sel!("new"));
                let msg = format!(
                    "Do you want to save changes to '{}'?",
                    state
                        .file_path
                        .as_ref()
                        .map(|p| p.file_name().unwrap().to_string_lossy())
                        .unwrap_or("untitled".into())
                );
                let nsmsg = nsstring!(&msg);
                msg_send_void!(alert, sel!("setMessageText:"), nsmsg);
                msg_send_void!(alert, sel!("setAlertStyle:"), 1);
                let save_btn = nsstring!("Save");
                let dont_save_btn = nsstring!("Don't Save");
                let cancel_btn = nsstring!("Cancel");
                msg_send_void!(alert, sel!("addButtonWithTitle:"), save_btn);
                msg_send_void!(alert, sel!("addButtonWithTitle:"), dont_save_btn);
                msg_send_void!(alert, sel!("addButtonWithTitle:"), cancel_btn);
                let response = msg_send_int!(alert, sel!("runModal"));
                if response == 1 {
                    if let Err(e) = save_file(state) {
                        let err_alert = msg_send_id!(cls!("NSAlert"), sel!("new"));
                        let err_text = format!("Failed to save: {}", e);
                        let err_msg = nsstring!(&err_text);
                        msg_send_void!(err_alert, sel!("setMessageText:"), err_msg);
                        msg_send_void!(err_alert, sel!("runModal"));
                        return 0;
                    }
                    return 1;
                } else if response == 0 {
                    return 0;
                } else {
                    return 1;
                }
            }
            1
        }
    }

    extern "C" fn window_will_close(_self: id, _cmd: SEL, _notification: id) {
        unsafe {
            let state = get_state();
            if let Some(lsp) = state.lsp.take() {
                let _ = lsp.shutdown();
            }
            msg_send_void!(
                state.app,
                sel!("terminate:"),
                std::ptr::null_mut::<c_void>()
            );
        }
    }

    extern "C" fn validate_menu_item(_self: id, _cmd: SEL, _item: id) -> BOOL {
        1
    }

    let imp_resize = window_will_resize as *const c_void;
    let imp_should_close = window_should_close as *const c_void;
    let imp_will_close = window_will_close as *const c_void;
    let imp_validate = validate_menu_item as *const c_void;

    class_addMethod(
        cls,
        sel!("windowWillResize:toSize:"),
        imp_resize,
        "{NSSize=dd}@:@:{NSSize=dd}\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("windowShouldClose:"),
        imp_should_close,
        "B@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("windowWillClose:"),
        imp_will_close,
        "v@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("validateMenuItem:"),
        imp_validate,
        "B@:@\0".as_ptr() as *const c_char,
    );

    objc_registerClassPair(cls);
    cls
}

// ============================================================================
// NSTextView Subclass – with drag‑and‑drop
// ============================================================================

unsafe fn create_text_view_subclass(editable: bool) -> id {
    let superclass = cls!("NSTextView");
    let class_name = if editable {
        CString::new("VoxEditorTextView").unwrap()
    } else {
        CString::new("VoxTerminalTextView").unwrap()
    };
    let cls = objc_allocateClassPair(superclass, class_name.as_ptr(), 0);
    if cls == ptr::null_mut() {
        return ptr::null_mut();
    }

    extern "C" fn become_first_responder(_self: id, _cmd: SEL) -> BOOL {
        unsafe {
            debug_log!("[GUI_DEBUG] becomeFirstResponder called on text view");
            let super_class = cls!("NSTextView");
            let mut super_struct = objc_super {
                receiver: _self,
                super_class,
            };
            let result = objc_msgSendSuper(&mut super_struct, sel!("becomeFirstResponder")) as BOOL;
            debug_log!("[GUI_DEBUG] becomeFirstResponder returned: {}", result);
            result
        }
    }

    extern "C" fn resign_first_responder(_self: id, _cmd: SEL) -> BOOL {
        unsafe {
            debug_log!("[GUI_DEBUG] resignFirstResponder called on text view");
            let super_class = cls!("NSTextView");
            let mut super_struct = objc_super {
                receiver: _self,
                super_class,
            };
            let result = objc_msgSendSuper(&mut super_struct, sel!("resignFirstResponder")) as BOOL;
            debug_log!("[GUI_DEBUG] resignFirstResponder returned: {}", result);
            result
        }
    }

    extern "C" fn key_down(_self: id, _cmd: SEL, event: id) {
        unsafe {
            let characters = msg_send_id!(event, sel!("charactersIgnoringModifiers"));
            if characters != ptr::null_mut() {
                let utf8 = msg_send_id!(characters, sel!("UTF8String")) as *const c_char;
                if !utf8.is_null() {
                    let ch = CStr::from_ptr(utf8).to_string_lossy().into_owned();
                    if ch.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                        debug_log!("[GUI_DEBUG] keyDown: character '{}'", ch);
                    }
                }
            }

            let modifier_flags = msg_send_int!(event, sel!("modifierFlags"));
            let key_code = msg_send_int!(event, sel!("keyCode"));
            let characters = msg_send_id!(event, sel!("charactersIgnoringModifiers"));
            if characters != ptr::null_mut() {
                let utf8 = msg_send_id!(characters, sel!("UTF8String")) as *const c_char;
                if !utf8.is_null() {
                    let ch = CStr::from_ptr(utf8).to_string_lossy().into_owned();
                    let is_cmd = (modifier_flags & (1 << 20)) != 0;
                    if is_cmd {
                        match ch.as_str() {
                            "o" => {
                                debug_log!("[GUI_DEBUG] Cmd+O triggered");
                                let state = get_state();
                                let panel = msg_send_id!(cls!("NSOpenPanel"), sel!("openPanel"));
                                msg_send_void!(panel, sel!("setAllowsMultipleSelection:"), 0);
                                msg_send_void!(panel, sel!("setCanChooseDirectories:"), 0);
                                msg_send_void!(panel, sel!("setCanChooseFiles:"), 1);
                                msg_send_void!(
                                    panel,
                                    sel!("setAllowedFileTypes:"),
                                    nsarray!(nsstring!("vx"))
                                );
                                let response = msg_send_int!(panel, sel!("runModal"));
                                if response == NSModalResponseOK {
                                    let urls = msg_send_id!(panel, sel!("URLs"));
                                    if urls != ptr::null_mut() {
                                        let url = msg_send_id!(urls, sel!("firstObject"));
                                        if url != ptr::null_mut() {
                                            let path_str = msg_send_id!(url, sel!("path"));
                                            if path_str != ptr::null_mut() {
                                                let utf8 =
                                                    msg_send_id!(path_str, sel!("UTF8String"))
                                                        as *const c_char;
                                                if !utf8.is_null() {
                                                    let path = CStr::from_ptr(utf8)
                                                        .to_string_lossy()
                                                        .into_owned();
                                                    let p = Path::new(&path);
                                                    if let Err(e) = load_file(state, p) {
                                                        push_output_line(
                                                            &state.terminal_buf,
                                                            &format!("Failed to load: {}", e),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                return;
                            }
                            "s" => {
                                debug_log!("[GUI_DEBUG] Cmd+S triggered");
                                let state = get_state();
                                if let Err(e) = save_file(state) {
                                    push_output_line(
                                        &state.terminal_buf,
                                        &format!("Save failed: {}", e),
                                    );
                                } else {
                                    state.is_modified = false;
                                    push_output_line(&state.terminal_buf, "File saved.");
                                }
                                return;
                            }
                            "q" => {
                                debug_log!("[GUI_DEBUG] Cmd+Q triggered");
                                msg_send_void!(get_state().window, sel!("close"));
                                return;
                            }
                            _ => {}
                        }
                    } else {
                        if key_code == 96 {
                            debug_log!("[GUI_DEBUG] Backtick key pressed (run)");
                            let state = get_state();
                            if state.compilation_in_progress {
                                push_output_line(
                                    &state.terminal_buf,
                                    "Compilation already in progress.",
                                );
                                return;
                            }
                            if let Some(_path) = &state.file_path {
                                if let Err(e) = save_file(state) {
                                    push_output_line(
                                        &state.terminal_buf,
                                        &format!("Save failed: {}", e),
                                    );
                                    return;
                                }
                            }
                            trigger_run();
                            return;
                        }
                    }
                }
            }

            let super_class = cls!("NSTextView");
            let mut super_struct = objc_super {
                receiver: _self,
                super_class,
            };
            objc_msgSendSuper(&mut super_struct, sel!("keyDown:"), event);
        }
    }

    extern "C" fn dragging_entered(_self: id, _cmd: SEL, sender: id) -> NSInteger {
        unsafe {
            let pb = msg_send_id!(sender, sel!("draggingPasteboard"));

            // 1. Try NSFilenamesPboardType (classic, most reliable)
            let ns_filenames_type = nsstring!("NSFilenamesPboardType");
            let plist = msg_send_id!(pb, sel!("propertyListForType:"), ns_filenames_type);
            if plist != ptr::null_mut() && msg_send_int!(plist, sel!("count")) > 0 {
                let first = msg_send_id!(plist, sel!("objectAtIndex:"), 0);
                if first != ptr::null_mut() {
                    let utf8 = msg_send_id!(first, sel!("UTF8String")) as *const c_char;
                    if !utf8.is_null() {
                        let path = CStr::from_ptr(utf8).to_string_lossy();
                        if Path::new(path.as_ref()).exists() {
                            return NSDragOperationCopy;
                        }
                    }
                }
            }

            // 2. Try NSURL objects
            let classes = nsarray!(cls!("NSURL"));
            let options: *mut c_void = std::ptr::null_mut();
            let urls = msg_send_id!(pb, sel!("readObjectsForClasses:options:"), classes, options);
            if urls != ptr::null_mut() && msg_send_int!(urls, sel!("count")) > 0 {
                return NSDragOperationCopy;
            }

            // 3. Fallback to NSString
            let string_classes = nsarray!(cls!("NSString"));
            let strings = msg_send_id!(
                pb,
                sel!("readObjectsForClasses:options:"),
                string_classes,
                options
            );
            if strings != ptr::null_mut() && msg_send_int!(strings, sel!("count")) > 0 {
                let first = msg_send_id!(strings, sel!("objectAtIndex:"), 0);
                if first != ptr::null_mut() {
                    let utf8 = msg_send_id!(first, sel!("UTF8String")) as *const c_char;
                    if !utf8.is_null() {
                        let path = CStr::from_ptr(utf8).to_string_lossy();
                        if Path::new(path.as_ref()).exists() {
                            return NSDragOperationCopy;
                        }
                    }
                }
            }

            0
        }
    }

    extern "C" fn perform_drag_operation(_self: id, _cmd: SEL, sender: id) -> BOOL {
        unsafe {
            let pb = msg_send_id!(sender, sel!("draggingPasteboard"));

            // 1. Try NSFilenamesPboardType
            let ns_filenames_type = nsstring!("NSFilenamesPboardType");
            let plist = msg_send_id!(pb, sel!("propertyListForType:"), ns_filenames_type);
            if plist != ptr::null_mut() {
                let count = msg_send_int!(plist, sel!("count"));
                if count > 0 {
                    let first = msg_send_id!(plist, sel!("objectAtIndex:"), 0);
                    if first != ptr::null_mut() {
                        let utf8 = msg_send_id!(first, sel!("UTF8String")) as *const c_char;
                        if !utf8.is_null() {
                            let path = CStr::from_ptr(utf8).to_string_lossy().into_owned();
                            let p = Path::new(&path);
                            if p.exists() {
                                let state = get_state();
                                if let Err(e) = load_file(state, p) {
                                    push_output_line(
                                        &state.terminal_buf,
                                        &format!("Failed to load: {}", e),
                                    );
                                }
                                return 1;
                            }
                        }
                    }
                }
            }

            // 2. Try NSURL
            let classes = nsarray!(cls!("NSURL"));
            let options: *mut c_void = std::ptr::null_mut();
            let urls = msg_send_id!(pb, sel!("readObjectsForClasses:options:"), classes, options);
            if urls != ptr::null_mut() {
                let count = msg_send_int!(urls, sel!("count"));
                if count > 0 {
                    let url = msg_send_id!(urls, sel!("objectAtIndex:"), 0);
                    if url != ptr::null_mut() {
                        let path_str = msg_send_id!(url, sel!("path"));
                        if path_str != ptr::null_mut() {
                            let utf8 = msg_send_id!(path_str, sel!("UTF8String")) as *const c_char;
                            if !utf8.is_null() {
                                let path = CStr::from_ptr(utf8).to_string_lossy().into_owned();
                                let p = Path::new(&path);
                                let state = get_state();
                                if let Err(e) = load_file(state, p) {
                                    push_output_line(
                                        &state.terminal_buf,
                                        &format!("Failed to load: {}", e),
                                    );
                                }
                                return 1;
                            }
                        }
                    }
                }
            }

            // 3. Fallback to NSString
            let string_classes = nsarray!(cls!("NSString"));
            let strings = msg_send_id!(
                pb,
                sel!("readObjectsForClasses:options:"),
                string_classes,
                options
            );
            if strings != ptr::null_mut() && msg_send_int!(strings, sel!("count")) > 0 {
                let first = msg_send_id!(strings, sel!("objectAtIndex:"), 0);
                if first != ptr::null_mut() {
                    let utf8 = msg_send_id!(first, sel!("UTF8String")) as *const c_char;
                    if !utf8.is_null() {
                        let path = CStr::from_ptr(utf8).to_string_lossy().into_owned();
                        let p = Path::new(&path);
                        if p.exists() {
                            let state = get_state();
                            if let Err(e) = load_file(state, p) {
                                push_output_line(
                                    &state.terminal_buf,
                                    &format!("Failed to load: {}", e),
                                );
                            }
                            return 1;
                        }
                    }
                }
            }

            0
        }
    }

    let imp_become = become_first_responder as *const c_void;
    let imp_resign = resign_first_responder as *const c_void;
    let imp_key = key_down as *const c_void;
    let imp_drag_enter = dragging_entered as *const c_void;
    let imp_perform = perform_drag_operation as *const c_void;

    class_addMethod(
        cls,
        sel!("becomeFirstResponder"),
        imp_become,
        "B@:\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("resignFirstResponder"),
        imp_resign,
        "B@:\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("keyDown:"),
        imp_key,
        "v@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("draggingEntered:"),
        imp_drag_enter,
        "l@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("performDragOperation:"),
        imp_perform,
        "B@:@\0".as_ptr() as *const c_char,
    );

    objc_registerClassPair(cls);
    cls
}

// ============================================================================
// Menu creation
// ============================================================================

unsafe fn create_menu_bar(_state: &mut AppState) -> id {
    debug_log!("[GUI_DEBUG] Creating menu bar...");
    let menubar = msg_send_id!(cls!("NSMenu"), sel!("new"));
    msg_send_void!(menubar, sel!("setAutoenablesItems:"), 0);
    debug_log!("[GUI_DEBUG] Menubar pointer: {:?}", menubar);

    // --- App menu ---
    let app_menu = msg_send_id!(cls!("NSMenu"), sel!("alloc"));
    let app_menu = msg_send_id!(app_menu, sel!("initWithTitle:"), nsstring!("vox"));
    let app_item = msg_send_id!(cls!("NSMenuItem"), sel!("new"));
    msg_send_void!(app_item, sel!("setTitle:"), nsstring!("vox"));
    msg_send_void!(app_item, sel!("setSubmenu:"), app_menu);
    msg_send_void!(menubar, sel!("addItem:"), app_item);

    add_menu_item(app_menu, "About vox", 0, "", false, false);
    add_menu_item(app_menu, "", 0, "", false, false);
    add_menu_item(app_menu, "Preferences...", 0, ",", true, false);
    add_menu_item(app_menu, "", 0, "", false, false);
    add_menu_item(app_menu, "Hide vox", 0, "h", true, false);
    add_menu_item(app_menu, "Hide Others", 0, "h", true, true);
    add_menu_item(app_menu, "Show All", 0, "", false, false);
    add_menu_item(app_menu, "", 0, "", false, false);
    add_menu_item(app_menu, "Quit vox", ID_FILE_EXIT, "q", true, false);

    // --- File menu ---
    let file_menu = msg_send_id!(cls!("NSMenu"), sel!("alloc"));
    let file_menu = msg_send_id!(file_menu, sel!("initWithTitle:"), nsstring!("File"));
    let file_item = msg_send_id!(cls!("NSMenuItem"), sel!("new"));
    msg_send_void!(file_item, sel!("setTitle:"), nsstring!("File"));
    msg_send_void!(file_item, sel!("setSubmenu:"), file_menu);
    msg_send_void!(menubar, sel!("addItem:"), file_item);

    add_menu_item(file_menu, "Open...", ID_FILE_OPEN, "o", true, false);
    add_menu_item(file_menu, "Save", ID_FILE_SAVE, "s", true, false);
    add_menu_item(file_menu, "", 0, "", false, false);
    add_menu_item(file_menu, "Run", ID_RUN, "r", true, false);
    add_menu_item(file_menu, "", 0, "", false, false);
    add_menu_item(file_menu, "Exit", ID_FILE_EXIT, "q", true, false);

    // --- Edit menu ---
    let edit_menu = msg_send_id!(cls!("NSMenu"), sel!("alloc"));
    let edit_menu = msg_send_id!(edit_menu, sel!("initWithTitle:"), nsstring!("Edit"));
    let edit_item = msg_send_id!(cls!("NSMenuItem"), sel!("new"));
    msg_send_void!(edit_item, sel!("setTitle:"), nsstring!("Edit"));
    msg_send_void!(edit_item, sel!("setSubmenu:"), edit_menu);
    msg_send_void!(menubar, sel!("addItem:"), edit_item);

    add_menu_item(edit_menu, "Undo", ID_EDIT_UNDO, "z", true, false);
    add_menu_item(edit_menu, "Redo", ID_EDIT_REDO, "y", true, false);
    add_menu_item(edit_menu, "", 0, "", false, false);
    add_menu_item(edit_menu, "Cut", ID_EDIT_CUT, "x", true, false);
    add_menu_item(edit_menu, "Copy", ID_EDIT_COPY, "c", true, false);
    add_menu_item(edit_menu, "Paste", ID_EDIT_PASTE, "v", true, false);
    add_menu_item(edit_menu, "Delete", ID_EDIT_DELETE, "", false, false);
    add_menu_item(edit_menu, "", 0, "", false, false);
    add_menu_item(
        edit_menu,
        "Select All",
        ID_EDIT_SELECT_ALL,
        "a",
        true,
        false,
    );

    // --- Build menu ---
    let build_menu = msg_send_id!(cls!("NSMenu"), sel!("alloc"));
    let build_menu = msg_send_id!(build_menu, sel!("initWithTitle:"), nsstring!("Build"));
    let build_item = msg_send_id!(cls!("NSMenuItem"), sel!("new"));
    msg_send_void!(build_item, sel!("setTitle:"), nsstring!("Build"));
    msg_send_void!(build_item, sel!("setSubmenu:"), build_menu);
    msg_send_void!(menubar, sel!("addItem:"), build_item);

    add_menu_item(
        build_menu,
        "Build (Debug)",
        ID_BUILD_DEBUG,
        "b",
        true,
        false,
    );
    add_menu_item(
        build_menu,
        "Build Release",
        ID_BUILD_RELEASE,
        "r",
        true,
        false,
    );
    add_menu_item(build_menu, "", 0, "", false, false);
    add_menu_item(build_menu, "Check", ID_CHECK, "c", true, false);
    add_menu_item(build_menu, "Test", ID_TEST, "t", true, false);
    add_menu_item(build_menu, "Clean", ID_CLEAN, "l", true, false);

    // --- Debug menu (optional) ---
    let debug_menu = msg_send_id!(cls!("NSMenu"), sel!("alloc"));
    let debug_menu = msg_send_id!(debug_menu, sel!("initWithTitle:"), nsstring!("Debug"));
    let debug_item = msg_send_id!(cls!("NSMenuItem"), sel!("new"));
    msg_send_void!(debug_item, sel!("setTitle:"), nsstring!("Debug"));
    msg_send_void!(debug_item, sel!("setSubmenu:"), debug_menu);
    msg_send_void!(menubar, sel!("addItem:"), debug_item);

    add_menu_item(debug_menu, "Test Menu", 999, "", true, false);

    debug_log!("[GUI_DEBUG] Menu bar created: {:?}", menubar);
    menubar
}

unsafe fn add_menu_item(menu: id, title: &str, tag: u16, key: &str, enabled: bool, checked: bool) {
    let item = msg_send_id!(cls!("NSMenuItem"), sel!("new"));
    msg_send_void!(item, sel!("setTitle:"), nsstring!(title));
    msg_send_void!(item, sel!("setTag:"), tag as NSInteger);
    if enabled {
        msg_send_void!(item, sel!("setEnabled:"), 1);
    }
    if checked {
        msg_send_void!(item, sel!("setState:"), NSControlStateValueOn);
    }
    if !key.is_empty() {
        let key_str = key.to_lowercase();
        msg_send_void!(item, sel!("setKeyEquivalent:"), nsstring!(&key_str));
        msg_send_void!(item, sel!("setKeyEquivalentModifierMask:"), 1 << 20);
    }
    let controller = get_state().controller;
    msg_send_void!(item, sel!("setTarget:"), controller);
    msg_send_void!(item, sel!("setAction:"), sel!("handleMenuCommand:"));
    msg_send_void!(item, sel!("setEnabled:"), 1);
    msg_send_void!(menu, sel!("addItem:"), item);
}

// ============================================================================
// Build triggers
// ============================================================================

unsafe fn trigger_run() {
    let state = get_state();
    if state.compilation_in_progress {
        push_output_line(&state.terminal_buf, "Compilation already in progress.");
        return;
    }
    state.compilation_in_progress = true;
    emit_phase_update("Compiling", 0);

    let path = state.file_path.clone().unwrap_or_default();
    if !path.exists() {
        push_output_line(
            &state.terminal_buf,
            &format!("File not found: {}", path.display()),
        );
        state.compilation_in_progress = false;
        return;
    }

    clear_output();
    push_output_line(
        &state.terminal_buf,
        &format!("Compiling {}...", path.display()),
    );

    let terminal_buf = state.terminal_buf.clone();
    let target = host_triple();
    let config = CacheConfig {
        no_cache: true,
        reuse_proofs: false,
        reuse_bitcode: false,
        offline: true,
        trust_modules: false,
    };

    thread::spawn(move || unsafe {
        let result = compile_and_run_file(&path, &target, &config, None, None);
        match result {
            Ok(output) => {
                for line in output.lines {
                    push_output_line(&terminal_buf, &line);
                }
                push_output_line(&terminal_buf, "Execution finished.");
            }
            Err(e) => {
                debug_log!("[GUI] compile_and_run_file error: {}", e);
                push_output_line(&terminal_buf, &format!("Error: {}", e));
            }
        }
        request_refresh();
        emit_phase_update("Compilation complete", 100);
    });
}

unsafe fn trigger_build(release: bool) {
    let state = get_state();
    if state.compilation_in_progress {
        push_output_line(&state.terminal_buf, "Compilation already in progress.");
        return;
    }
    let path = match state.file_path.clone() {
        Some(p) => p,
        None => {
            push_output_line(&state.terminal_buf, "No file open.");
            return;
        }
    };
    if let Err(e) = save_file(state) {
        push_output_line(&state.terminal_buf, &format!("Save failed: {}", e));
        return;
    }
    state.compilation_in_progress = true;
    let phase = if release {
        "Building (release)"
    } else {
        "Building (debug)"
    };
    emit_phase_update(phase, 0);

    clear_output();
    push_output_line(
        &state.terminal_buf,
        &format!("{} {}...", phase, path.display()),
    );

    let terminal_buf = state.terminal_buf.clone();
    let target = host_triple();
    let config = CacheConfig {
        no_cache: true,
        reuse_proofs: false,
        reuse_bitcode: false,
        offline: true,
        trust_modules: false,
    };

    thread::spawn(move || unsafe {
        let result = build_file(&path, release, &target, &config, None, None);
        match result {
            Ok(exe) => push_output_line(
                &terminal_buf,
                &format!("Build succeeded: {}", exe.display()),
            ),
            Err(e) => push_output_line(&terminal_buf, &format!("Build failed: {}", e)),
        }
        request_refresh();
        emit_phase_update("Build complete", 100);
    });
}

unsafe fn trigger_check() {
    let state = get_state();
    if state.compilation_in_progress {
        push_output_line(&state.terminal_buf, "Compilation already in progress.");
        return;
    }
    let path = match state.file_path.clone() {
        Some(p) => p,
        None => {
            push_output_line(&state.terminal_buf, "No file open.");
            return;
        }
    };
    if let Err(e) = save_file(state) {
        push_output_line(&state.terminal_buf, &format!("Save failed: {}", e));
        return;
    }
    state.compilation_in_progress = true;
    emit_phase_update("Checking", 0);

    clear_output();
    push_output_line(
        &state.terminal_buf,
        &format!("Checking {}...", path.display()),
    );

    let terminal_buf = state.terminal_buf.clone();
    let target = host_triple();
    let config = CacheConfig {
        no_cache: true,
        reuse_proofs: false,
        reuse_bitcode: false,
        offline: true,
        trust_modules: false,
    };

    thread::spawn(move || unsafe {
        let result = check_file(&path, &target, &config, None, None);
        match result {
            Ok(true) => push_output_line(&terminal_buf, "Check passed."),
            Ok(false) => push_output_line(&terminal_buf, "Check failed: semantic errors."),
            Err(e) => push_output_line(&terminal_buf, &format!("Check error: {}", e)),
        }
        request_refresh();
        emit_phase_update("Check complete", 100);
    });
}

unsafe fn trigger_test() {
    let state = get_state();
    if state.compilation_in_progress {
        push_output_line(&state.terminal_buf, "Compilation already in progress.");
        return;
    }
    state.compilation_in_progress = true;
    emit_phase_update("Testing", 0);

    let root = find_vox_root();
    let test_dir = root.join("src/Examples");
    if !test_dir.exists() {
        push_output_line(
            &state.terminal_buf,
            &format!("Examples directory not found at {}", test_dir.display()),
        );
        state.compilation_in_progress = false;
        return;
    }

    clear_output();
    push_output_line(
        &state.terminal_buf,
        &format!("Running tests in {}...", test_dir.display()),
    );

    let terminal_buf = state.terminal_buf.clone();
    let target = host_triple();
    let config = CacheConfig {
        no_cache: true,
        reuse_proofs: false,
        reuse_bitcode: false,
        offline: true,
        trust_modules: false,
    };

    thread::spawn(move || unsafe {
        let result = run_tests(&test_dir, &target, &config, None, None);
        match result {
            Ok((passed, total)) => push_output_line(
                &terminal_buf,
                &format!("Tests: {}/{} passed.", passed, total),
            ),
            Err(e) => push_output_line(&terminal_buf, &format!("Test error: {}", e)),
        }
        request_refresh();
        emit_phase_update("Test complete", 100);
    });
}

unsafe fn trigger_clean() {
    let state = get_state();
    if state.compilation_in_progress {
        push_output_line(
            &state.terminal_buf,
            "Compilation in progress, cannot clean.",
        );
        return;
    }
    state.compilation_in_progress = true;
    emit_phase_update("Cleaning", 0);

    clear_output();
    push_output_line(&state.terminal_buf, "Cleaning target/...");

    let terminal_buf = state.terminal_buf.clone();

    thread::spawn(move || unsafe {
        match clean_project() {
            Ok(()) => push_output_line(&terminal_buf, "Clean completed."),
            Err(e) => push_output_line(&terminal_buf, &format!("Clean error: {}", e)),
        }
        request_refresh();
        emit_phase_update("Clean complete", 100);
    });
}

// ============================================================================
// Main window creation
// ============================================================================

unsafe fn create_main_window(state: &mut AppState) {
    debug_log!("[GUI_DEBUG] Creating main window...");
    let window = msg_send_id!(cls!("NSWindow"), sel!("alloc"));
    let rect = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize {
            width: 960.0,
            height: 720.0,
        },
    };
    let style_mask = NSWindowStyleMaskTitled
        | NSWindowStyleMaskClosable
        | NSWindowStyleMaskMiniaturizable
        | NSWindowStyleMaskResizable;
    let window = msg_send_id!(
        window,
        sel!("initWithContentRect:styleMask:backing:defer:"),
        rect,
        style_mask,
        NSBackingStoreBuffered,
        0
    );
    debug_log!("[GUI_DEBUG] Window pointer: {:?}", window);
    msg_send_void!(window, sel!("setTitle:"), nsstring!("vox"));

    let appearance = msg_send_id!(
        cls!("NSAppearance"),
        sel!("appearanceNamed:"),
        nsstring!(NSAppearanceNameDarkAqua)
    );
    msg_send_void!(window, sel!("setAppearance:"), appearance);

    let content = msg_send_id!(window, sel!("contentView"));
    if content == ptr::null_mut() {
        panic!("contentView is null");
    }
    msg_send_void!(content, sel!("setWantsLayer:"), 1);
    let layer = msg_send_id!(content, sel!("layer"));
    if layer == ptr::null_mut() {
        panic!("layer is null");
    }
    msg_send_void!(layer, sel!("setCornerRadius:"), 16.0);
    msg_send_void!(layer, sel!("setMasksToBounds:"), 1);

    let editor_scroll = create_text_view(state, true);
    let terminal_scroll = create_text_view(state, false);
    let button = create_button(state);
    let status = create_status_bar(state);
    let progress_bar = create_progress_bar(state);

    msg_send_void!(content, sel!("addSubview:"), editor_scroll);
    msg_send_void!(content, sel!("addSubview:"), terminal_scroll);
    msg_send_void!(content, sel!("addSubview:"), button);
    msg_send_void!(content, sel!("addSubview:"), status);
    msg_send_void!(content, sel!("addSubview:"), progress_bar);

    state.window = window;
    state.editor_scroll = editor_scroll;
    state.terminal_scroll = terminal_scroll;
    state.button = button;
    state.status = status;
    state.progress_bar = progress_bar;

    layout_subviews(state);

    let delegate_class = create_window_delegate_class();
    let delegate = msg_send_id!(delegate_class, sel!("new"));
    objc_setAssociatedObject(window, APP_STATE_KEY.as_ptr() as *const c_void, delegate, 1);
    msg_send_void!(window, sel!("setDelegate:"), delegate);
    state.window_delegate = delegate;

    register_for_drag_drop(window);
    register_for_drag_drop(state.editor);
    register_for_drag_drop(state.terminal);

    update_status(state, "Ready", 0);
    debug_log!("[GUI_DEBUG] Main window creation complete.");
}

// ============================================================================
// create_text_view – enables rich text for terminal
// ============================================================================

unsafe fn create_text_view(state: &mut AppState, editable: bool) -> id {
    debug_log!("[GUI_DEBUG] Creating text view, editable={}", editable);
    let scroll_view = msg_send_id!(cls!("NSScrollView"), sel!("new"));
    let text_view_class = create_text_view_subclass(editable);

    let alloc = msg_send_id!(text_view_class, sel!("alloc"));
    let initial_rect = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize {
            width: 1000.0,
            height: 1000.0,
        },
    };
    let text_view = msg_send_id!(alloc, sel!("initWithFrame:"), initial_rect);
    debug_log!("[GUI_DEBUG] TextView pointer: {:?}", text_view);

    msg_send_void!(
        text_view,
        sel!("setEditable:"),
        if editable { 1 } else { 0 }
    );
    msg_send_void!(text_view, sel!("setSelectable:"), 1);
    if !editable {
        msg_send_void!(text_view, sel!("setRichText:"), 1);
        msg_send_void!(text_view, sel!("setUsesFontPanel:"), 0);
        msg_send_void!(text_view, sel!("setAutomaticLinkDetectionEnabled:"), 0);
    } else {
        msg_send_void!(text_view, sel!("setRichText:"), 0);
    }

    let font = msg_send_id!(
        cls!("NSFont"),
        sel!("fontWithName:size:"),
        nsstring!("Menlo"),
        14.0
    );
    msg_send_void!(text_view, sel!("setFont:"), font);
    msg_send_void!(
        text_view,
        sel!("setTextColor:"),
        msg_send_id!(cls!("NSColor"), sel!("whiteColor"))
    );
    let bg = msg_send_id!(
        cls!("NSColor"),
        sel!("colorWithRed:green:blue:alpha:"),
        0.12,
        0.12,
        0.12,
        1.0
    );
    msg_send_void!(text_view, sel!("setBackgroundColor:"), bg);
    msg_send_void!(text_view, sel!("setHorizontallyResizable:"), 0);
    msg_send_void!(text_view, sel!("setVerticallyResizable:"), 1);
    msg_send_void!(
        text_view,
        sel!("setAutoresizingMask:"),
        NSViewWidthSizable | NSViewHeightSizable
    );
    if !editable {
        let inset = NSSize {
            width: 4.0,
            height: 4.0,
        };
        msg_send_void!(text_view, sel!("setTextContainerInset:"), inset);
    }

    let container = msg_send_id!(text_view, sel!("textContainer"));
    if container != ptr::null_mut() {
        msg_send_void!(container, sel!("setWidthTracksTextView:"), 1);
        debug_log!("[GUI_DEBUG] textContainer width tracking enabled");
    }

    msg_send_void!(scroll_view, sel!("setDocumentView:"), text_view);
    msg_send_void!(scroll_view, sel!("setHasHorizontalScroller:"), 1);
    msg_send_void!(scroll_view, sel!("setHasVerticalScroller:"), 1);
    msg_send_void!(scroll_view, sel!("setBorderType:"), NSBezelBorder);
    msg_send_void!(
        scroll_view,
        sel!("setAutoresizingMask:"),
        NSViewWidthSizable | NSViewHeightSizable
    );

    objc_setAssociatedObject(
        scroll_view,
        "VoxTextViewKey".as_ptr() as *const c_void,
        text_view,
        1,
    );

    if editable {
        state.editor = text_view;
    } else {
        state.terminal = text_view;
    }
    debug_log!("[GUI_DEBUG] Text view creation complete.");
    scroll_view
}

unsafe fn register_for_drag_drop(view: id) {
    let types = nsarray!(
        nsstring!("public.file-url"),
        nsstring!("NSFilenamesPboardType"),
        nsstring!("public.plain-text")
    );
    msg_send_void!(view, sel!("registerForDraggedTypes:"), types);
}

unsafe fn create_button(_state: &mut AppState) -> id {
    let button = msg_send_id!(cls!("NSButton"), sel!("new"));
    msg_send_void!(button, sel!("setTitle:"), nsstring!("Run"));
    msg_send_void!(button, sel!("setBezelStyle:"), NSRoundedBezelStyle);
    msg_send_void!(button, sel!("setButtonType:"), NSMomentaryPushInButton);
    msg_send_void!(
        button,
        sel!("setAutoresizingMask:"),
        NSViewMinXMargin | NSViewMaxYMargin
    );
    button
}

unsafe fn setup_button_target(state: &mut AppState) {
    msg_send_void!(state.button, sel!("setTarget:"), state.controller);
    msg_send_void!(state.button, sel!("setAction:"), sel!("runButtonClicked:"));
}

unsafe fn create_status_bar(_state: &mut AppState) -> id {
    let text_field = msg_send_id!(cls!("NSTextField"), sel!("new"));
    msg_send_void!(text_field, sel!("setEditable:"), 0);
    msg_send_void!(text_field, sel!("setSelectable:"), 0);
    msg_send_void!(text_field, sel!("setBezeled:"), 0);
    msg_send_void!(text_field, sel!("setDrawsBackground:"), 1);
    let bg = msg_send_id!(
        cls!("NSColor"),
        sel!("colorWithRed:green:blue:alpha:"),
        0.2,
        0.2,
        0.2,
        1.0
    );
    msg_send_void!(text_field, sel!("setBackgroundColor:"), bg);
    msg_send_void!(
        text_field,
        sel!("setTextColor:"),
        msg_send_id!(cls!("NSColor"), sel!("whiteColor"))
    );
    let font = msg_send_id!(
        cls!("NSFont"),
        sel!("fontWithName:size:"),
        nsstring!("Menlo"),
        12.0
    );
    msg_send_void!(text_field, sel!("setFont:"), font);
    msg_send_void!(text_field, sel!("setAlignment:"), NSLeftTextAlignment);
    msg_send_void!(
        text_field,
        sel!("setAutoresizingMask:"),
        NSViewWidthSizable | NSViewMinYMargin
    );
    text_field
}

unsafe fn create_progress_bar(_state: &mut AppState) -> id {
    let progress_bar = msg_send_id!(cls!("NSProgressIndicator"), sel!("alloc"));
    let initial_rect = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize {
            width: 100.0,
            height: 22.0,
        },
    };
    let progress_bar = msg_send_id!(progress_bar, sel!("initWithFrame:"), initial_rect);
    msg_send_void!(progress_bar, sel!("setStyle:"), NSProgressIndicatorStyleBar);
    msg_send_void!(progress_bar, sel!("setIndeterminate:"), 0);
    // Use explicit f64 binding to avoid ABI issues
    objc_msgSend_f64(progress_bar, sel!("setMinValue:"), 0.0);
    objc_msgSend_f64(progress_bar, sel!("setMaxValue:"), 100.0);
    objc_msgSend_f64(progress_bar, sel!("setDoubleValue:"), 0.0);
    msg_send_void!(progress_bar, sel!("setUsesThreadedAnimation:"), 0);
    msg_send_void!(progress_bar, sel!("setNeedsDisplay:"), 1);
    progress_bar
}

// ============================================================================
// layout_subviews – with fixed layout
// ============================================================================

unsafe fn layout_subviews(state: &mut AppState) {
    let window_frame = msg_send_rect!(state.window, sel!("frame"));
    let width = window_frame.size.width;
    let height = window_frame.size.height;

    let title_bar_height: CGFloat = 28.0;
    let content_height = height - title_bar_height;

    let button_height: CGFloat = 28.0;
    let status_height: CGFloat = 22.0;
    let gap: CGFloat = 4.0;
    let editor_height = (content_height - status_height - button_height - gap * 3.0) * 0.6;
    let terminal_height =
        content_height - status_height - button_height - gap * 3.0 - editor_height;

    let editor_rect = NSRect {
        origin: NSPoint {
            x: gap,
            y: terminal_height + button_height + gap * 2.0 + gap,
        },
        size: NSSize {
            width: width - gap * 2.0,
            height: editor_height,
        },
    };
    let terminal_rect = NSRect {
        origin: NSPoint {
            x: gap,
            y: button_height + gap * 2.0 + gap,
        },
        size: NSSize {
            width: width - gap * 2.0,
            height: terminal_height,
        },
    };

    // ---- NEW FIXED LAYOUT ----
    let button_width: CGFloat = 80.0;
    let status_width: CGFloat = 250.0; // Fixed width for status text

    // Progress bar takes up all the remaining middle space
    let progress_width = (width - gap * 4.0 - button_width - status_width).max(50.0);

    let status_rect = NSRect {
        origin: NSPoint { x: gap, y: gap },
        size: NSSize {
            width: status_width,
            height: status_height,
        },
    };
    let progress_rect = NSRect {
        origin: NSPoint {
            x: gap + status_width + gap,
            y: gap,
        },
        size: NSSize {
            width: progress_width,
            height: status_height,
        },
    };
    let button_rect = NSRect {
        origin: NSPoint {
            x: width - button_width - gap,
            y: gap,
        },
        size: NSSize {
            width: button_width,
            height: button_height,
        },
    };
    // ---- END NEW LAYOUT ----

    msg_send_void!(state.editor_scroll, sel!("setFrame:"), editor_rect);
    msg_send_void!(state.terminal_scroll, sel!("setFrame:"), terminal_rect);
    msg_send_void!(state.button, sel!("setFrame:"), button_rect);
    msg_send_void!(state.status, sel!("setFrame:"), status_rect);
    msg_send_void!(state.progress_bar, sel!("setFrame:"), progress_rect);
}

// ============================================================================
// App Delegate
// ============================================================================

unsafe fn create_app_delegate_class() -> id {
    let superclass = cls!("NSObject");
    let class_name = CString::new("VoxAppDelegate").unwrap();
    let cls = objc_allocateClassPair(superclass, class_name.as_ptr(), 0);
    if cls == ptr::null_mut() {
        return ptr::null_mut();
    }

    extern "C" fn application_did_finish_launching(_self: id, _cmd: SEL, _notification: id) {
        unsafe {
            debug_log!("[GUI_DEBUG] === applicationDidFinishLaunching started ===");
            let state = get_state();
            let shared_app = msg_send_id!(cls!("NSApplication"), sel!("sharedApplication"));

            let appearance = msg_send_id!(
                cls!("NSAppearance"),
                sel!("appearanceNamed:"),
                nsstring!(NSAppearanceNameDarkAqua)
            );
            msg_send_void!(shared_app, sel!("setAppearance:"), appearance);

            create_main_window(state);
            let controller_class = create_controller_class();
            let controller = msg_send_id!(controller_class, sel!("new"));
            state.controller = controller;
            setup_button_target(state);

            state.menu_bar = create_menu_bar(state);
            msg_send_void!(shared_app, sel!("setMainMenu:"), state.menu_bar);
            msg_send_void!(shared_app, sel!("updateWindows"));
            let main_menu = msg_send_id!(shared_app, sel!("mainMenu"));
            debug_log!("[GUI_DEBUG] mainMenu after set: {:?}", main_menu);

            msg_send_void!(
                state.window,
                sel!("setInitialFirstResponder:"),
                state.editor
            );

            let running_app =
                msg_send_id!(cls!("NSRunningApplication"), sel!("currentApplication"));
            msg_send_void!(running_app, sel!("activateWithOptions:"), 3_usize);
            msg_send_void!(shared_app, sel!("activateIgnoringOtherApps:"), 1);

            msg_send_void!(
                state.window,
                sel!("makeKeyAndOrderFront:"),
                std::ptr::null_mut::<c_void>()
            );
            msg_send_void!(state.window, sel!("makeKeyWindow"));
            msg_send_void!(state.window, sel!("makeMainWindow"));
            msg_send_void!(state.window, sel!("makeFirstResponder:"), state.editor);

            let center = msg_send_id!(cls!("NSNotificationCenter"), sel!("defaultCenter"));
            let refresh_name = nsstring!(NOTIFICATION_REFRESH);
            let diag_name = nsstring!(NOTIFICATION_DIAGNOSTICS);
            let phase_name = nsstring!(NOTIFICATION_PHASE_UPDATE);
            msg_send_void!(
                center,
                sel!("addObserver:selector:name:object:"),
                controller,
                sel!("processRefresh"),
                refresh_name,
                std::ptr::null_mut::<c_void>()
            );
            msg_send_void!(
                center,
                sel!("addObserver:selector:name:object:"),
                controller,
                sel!("processDiagnostics:"),
                diag_name,
                std::ptr::null_mut::<c_void>()
            );
            msg_send_void!(
                center,
                sel!("addObserver:selector:name:object:"),
                controller,
                sel!("processPhaseUpdate:"),
                phase_name,
                std::ptr::null_mut::<c_void>()
            );

            let is_key = msg_send_bool!(state.window, sel!("isKeyWindow"));
            let is_main = msg_send_bool!(state.window, sel!("isMainWindow"));
            debug_log!(
                "[GUI_DEBUG] After activation: isKeyWindow={}, isMainWindow={}",
                is_key,
                is_main
            );

            update_status(state, "Ready", 0);
            debug_log!("[GUI_DEBUG] === applicationDidFinishLaunching finished ===");
        }
    }

    extern "C" fn application_should_terminate_after_last_window_closed(
        _self: id,
        _cmd: SEL,
    ) -> BOOL {
        1
    }

    extern "C" fn application_will_terminate(_self: id, _cmd: SEL, _notification: id) {
        unsafe {
            let state = get_state();
            if let Some(lsp) = state.lsp.take() {
                let _ = lsp.shutdown();
            }
        }
    }

    let imp_did_launch = application_did_finish_launching as *const c_void;
    let imp_should = application_should_terminate_after_last_window_closed as *const c_void;
    let imp_will_term = application_will_terminate as *const c_void;

    class_addMethod(
        cls,
        sel!("applicationDidFinishLaunching:"),
        imp_did_launch,
        "v@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("applicationShouldTerminateAfterLastWindowClosed:"),
        imp_should,
        "B@:@\0".as_ptr() as *const c_char,
    );
    class_addMethod(
        cls,
        sel!("applicationWillTerminate:"),
        imp_will_term,
        "v@:@\0".as_ptr() as *const c_char,
    );

    objc_registerClassPair(cls);
    cls
}

// ============================================================================
// run()
// ============================================================================

pub fn run(_hide_console: bool) -> Result<(), String> {
    unsafe {
        debug_log!("[GUI_DEBUG] === run() started ===");
        let state = Box::new(AppState::default());
        APP_STATE_PTR = Box::into_raw(state);

        let state_ref = get_state();
        set_gui_terminal(state_ref.terminal_buf.clone());
        set_gui_refresh_callback(refresh_callback);
        set_gui_phase_callback(phase_callback);

        let delegate_class = create_app_delegate_class();
        let delegate = msg_send_id!(delegate_class, sel!("new"));
        let shared_app = msg_send_id!(cls!("NSApplication"), sel!("sharedApplication"));
        state_ref.app = shared_app;
        msg_send_void!(shared_app, sel!("setDelegate:"), delegate);
        msg_send_void!(
            shared_app,
            sel!("setActivationPolicy:"),
            NSApplicationActivationPolicyRegular
        );

        debug_log!("[GUI_DEBUG] Entering NSApplication runloop...");
        msg_send_void!(shared_app, sel!("run"));
        debug_log!("[GUI_DEBUG] runloop exited.");

        let _ = Box::from_raw(APP_STATE_PTR);
        APP_STATE_PTR = ptr::null_mut();
        Ok(())
    }
}
