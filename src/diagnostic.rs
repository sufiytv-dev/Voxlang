// diagnostic.rs - Structured diagnostics with event buffering, UI thread support,
// and hybrid output (pretty/JSON). Tracks exit codes and global debug flag.
// Updated: Added VT enablement for Windows using raw FFI (no winapi dependency).
// Now includes debugging traces for GUI forwarding and a placeholder for source-context
// diagnostic formatting.
// Added test_run_active flag to suppress per-file phase updates during test runs.
// GUI communication is now callback-based, removing direct PostMessageW usage for cross-platform support.

use crate::frontend::span::Span;
use crate::shell::terminal::TerminalBuffer;
use std::fs;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

// -----------------------------------------------------------------------------
// Windows‑specific constants and HWND management
// -----------------------------------------------------------------------------
#[cfg(windows)]
use std::os::raw::c_void;

#[cfg(windows)]
pub const WM_USER_REFRESH: u32 = 0x0400 + 100;
#[cfg(windows)]
pub const WM_USER_PHASE_UPDATE: u32 = 0x0400 + 101;

#[cfg(windows)]
static GUI_HWND: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[cfg(windows)]
pub fn set_gui_hwnd(hwnd: *mut c_void) {
    GUI_HWND.store(hwnd, Ordering::Relaxed);
}

#[cfg(windows)]
pub fn get_gui_hwnd() -> *mut c_void {
    GUI_HWND.load(Ordering::Relaxed)
}

// -----------------------------------------------------------------------------
// Diagnostic trace flag – set to true to enable internal trace prints (to stdout).
// This is independent of global_debug() – it controls only the [TRACE] prints.
// -----------------------------------------------------------------------------
const DIAG_TRACE: bool = false; // <-- Set to true for debugging, then false when done.

// -----------------------------------------------------------------------------
// Windows virtual terminal enablement (zero‑dependency, using raw FFI)
// -----------------------------------------------------------------------------
#[cfg(windows)]
mod vt {

    type DWORD = u32;
    type HANDLE = *mut std::ffi::c_void;
    type BOOL = i32;

    const STD_OUTPUT_HANDLE: DWORD = 0xFFFFFFF5;
    const STD_ERROR_HANDLE: DWORD = 0xFFFFFFF4;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: DWORD = 0x0004;

    unsafe extern "system" {
        fn GetStdHandle(nStdHandle: DWORD) -> HANDLE;
        fn GetConsoleMode(hConsoleHandle: HANDLE, lpMode: *mut DWORD) -> BOOL;
        fn SetConsoleMode(hConsoleHandle: HANDLE, dwMode: DWORD) -> BOOL;
    }

    pub fn enable() {
        unsafe {
            for &handle_id in &[STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                let handle = GetStdHandle(handle_id);
                if handle.is_null() {
                    continue;
                }
                let mut mode: DWORD = 0;
                if GetConsoleMode(handle, &mut mode) != 0 {
                    SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod vt {
    pub fn enable() {
        // No‑op on non‑Windows.
    }
}

// -----------------------------------------------------------------------------
// GUI callbacks (set by the platform‑specific GUI implementation)
// -----------------------------------------------------------------------------

/// Callback type for requesting a refresh of the GUI terminal.
type RefreshCallback = fn();

/// Callback type for updating the GUI status bar with a phase and percentage.
type PhaseCallback = fn(&'static str, usize);

static GUI_REFRESH_CALLBACK: OnceLock<RefreshCallback> = OnceLock::new();
static GUI_PHASE_CALLBACK: OnceLock<PhaseCallback> = OnceLock::new();

/// Set the callback to request a GUI refresh.
pub fn set_gui_refresh_callback(callback: RefreshCallback) {
    let _ = GUI_REFRESH_CALLBACK.set(callback);
}

/// Set the callback to update the GUI status bar.
pub fn set_gui_phase_callback(callback: PhaseCallback) {
    let _ = GUI_PHASE_CALLBACK.set(callback);
}

// -----------------------------------------------------------------------------
// ANSI color constants
// -----------------------------------------------------------------------------

const COLOR_RESET: &str = "\x1b[0m";
const COLOR_LEX: &str = "\x1b[94m"; // Bright Blue
const COLOR_PARSE: &str = "\x1b[95m"; // Bright Magenta
const COLOR_SEM: &str = "\x1b[96m"; // Bright Cyan
const COLOR_CODEGEN: &str = "\x1b[90m"; // Gray
const COLOR_DISCOVERY: &str = "\x1b[92m"; // Bright Green
const COLOR_IMPORT: &str = "\x1b[93m"; // Bright Yellow
const COLOR_GUI: &str = "\x1b[97m"; // Bright White
const COLOR_IR: &str = "\x1b[90m"; // Gray
const COLOR_PHASE: &str = "\x1b[93m"; // Bright Yellow
const COLOR_DEFAULT: &str = "\x1b[37m"; // White
const COLOR_DESUGAR: &str = "\x1b[93m";

/// Colorize every `[TAG]` occurrence in the message using character indexing (UTF‑8 safe).
fn colorize_prefix(msg: &str) -> String {
    let raw = msg;
    let chars: Vec<char> = raw.chars().collect();
    let mut result = String::with_capacity(raw.len() + 32);
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '[' {
            let start = i;
            let mut end = i + 1;
            while end < chars.len() && chars[end] != ']' {
                end += 1;
            }
            if end < chars.len() {
                // Found a complete tag
                let tag: String = chars[start..=end].iter().collect();
                let color = match tag.as_str() {
                    "[LEX]" => COLOR_LEX,
                    "[PARSE]" => COLOR_PARSE,
                    "[SEM]" => COLOR_SEM,
                    "[CODEGEN]" => COLOR_CODEGEN,
                    "[CODEGEN:STMT]" => COLOR_CODEGEN,
                    "[CODEGEN:device]" => COLOR_CODEGEN,
                    "[CODEGEN:TYPE_MAP]" => COLOR_CODEGEN,
                    "[DISCOVERY]" => COLOR_DISCOVERY,
                    "[IMPORT]" => COLOR_IMPORT,
                    "[GUI]" => COLOR_GUI,
                    p if p.starts_with("[IR:") => COLOR_IR,
                    "[PHASE]" => COLOR_PHASE,
                    "[LINK]" => COLOR_PARSE,
                    "[RUSTC]" => COLOR_LEX,
                    "[DESUGAR]" => COLOR_DESUGAR,
                    "[DIAG]" => COLOR_DEFAULT,
                    _ => COLOR_DEFAULT,
                };
                let colored = format!("{}{}{}", color, tag, COLOR_RESET);
                result.push_str(&colored);
                i = end + 1;
            } else {
                // No closing bracket – push the rest as plain text
                result.push_str(&chars[i..].iter().collect::<String>());
                break;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    if DIAG_TRACE {
        println!("[TRACE] colorize_prefix raw: {:?}", raw);
        println!("[TRACE] colorize_prefix result: {:?}", result);
    }

    result
}

// -----------------------------------------------------------------------------
// Exit code definitions
// -----------------------------------------------------------------------------

/// Exit codes returned by the compiler.
#[allow(dead_code)]
pub mod exit_code {
    pub const SUCCESS: i32 = 0;
    pub const GENERIC_ERROR: i32 = 1;
    pub const LEXICAL_ERROR: i32 = 2;
    pub const SYNTAX_ERROR: i32 = 3;
    pub const SEMANTIC_ERROR: i32 = 4;
    pub const IMPORT_ERROR: i32 = 5;
    pub const IR_GENERATION_ERROR: i32 = 6;
    pub const LINKER_ERROR: i32 = 7;
    pub const IO_ERROR: i32 = 8;
    pub const INTERNAL_ERROR: i32 = 127;
}

// -----------------------------------------------------------------------------
// Data types
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl Range {
    pub fn new(start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> Self {
        Self {
            start_line,
            start_col,
            end_line,
            end_col,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub message: String,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: Level,
    pub message: String,
    pub code: Option<String>,
    pub span: Option<Span>,   // kept for backward compatibility
    pub range: Option<Range>, // for LSP diagnostics
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            message: message.into(),
            code: None,
            span: None,
            range: None,
            suggestions: Vec::new(),
        }
    }
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            message: message.into(),
            code: None,
            span: None,
            range: None,
            suggestions: Vec::new(),
        }
    }
    pub fn note(message: impl Into<String>) -> Self {
        Self {
            level: Level::Note,
            message: message.into(),
            code: None,
            span: None,
            range: None,
            suggestions: Vec::new(),
        }
    }
    pub fn help(message: impl Into<String>) -> Self {
        Self {
            level: Level::Help,
            message: message.into(),
            code: None,
            span: None,
            range: None,
            suggestions: Vec::new(),
        }
    }
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }
    pub fn with_range(mut self, range: Range) -> Self {
        self.range = Some(range);
        self
    }
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }
}

// -----------------------------------------------------------------------------
// CompilerEvent
// -----------------------------------------------------------------------------

pub enum CompilerEvent {
    Diagnostic(Diagnostic),
    PhaseUpdate { phase: &'static str, percent: usize },
    Log(String),
}

// -----------------------------------------------------------------------------
// Output format control
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Auto,
    Pretty,
    Json,
}

static GLOBAL_OUTPUT_FORMAT: AtomicUsize = AtomicUsize::new(0); // 0=Auto, 1=Pretty, 2=Json

pub fn set_output_format(format: OutputFormat) {
    let val = match format {
        OutputFormat::Auto => 0,
        OutputFormat::Pretty => 1,
        OutputFormat::Json => 2,
    };
    GLOBAL_OUTPUT_FORMAT.store(val, Ordering::Relaxed);
}

fn determine_current_format() -> OutputFormat {
    match GLOBAL_OUTPUT_FORMAT.load(Ordering::Relaxed) {
        1 => OutputFormat::Pretty,
        2 => OutputFormat::Json,
        _ => {
            if std::io::stderr().is_terminal() {
                OutputFormat::Pretty
            } else {
                OutputFormat::Json
            }
        }
    }
}

// -----------------------------------------------------------------------------
// DiagnosticManager
// -----------------------------------------------------------------------------

pub struct DiagnosticManager {
    buffer: Mutex<Vec<u8>>,
    pub current_percent: AtomicUsize,
    current_phase: Mutex<&'static str>,
    collecting: AtomicUsize,
    exit_code: AtomicUsize,
}

impl DiagnosticManager {
    fn new() -> Self {
        Self {
            buffer: Mutex::new(Vec::new()),
            current_percent: AtomicUsize::new(0),
            current_phase: Mutex::new(""),
            collecting: AtomicUsize::new(0),
            exit_code: AtomicUsize::new(0),
        }
    }

    pub fn emit(&self, event: CompilerEvent) {
        match event {
            CompilerEvent::Diagnostic(diag) => {
                if diag.level == Level::Error {
                    let new_code = diagnostic_to_exit_code(&diag) as usize;
                    let current = self.exit_code.load(Ordering::Relaxed);
                    if new_code > current {
                        self.exit_code.store(new_code, Ordering::Relaxed);
                    }
                }
                let json = diagnostic_to_json(&diag);
                let mut buf = self.buffer.lock().unwrap();
                buf.extend_from_slice(json.as_bytes());
                buf.push(b'\n');
            }
            CompilerEvent::PhaseUpdate { phase, percent } => {
                {
                    let mut cur_phase = self.current_phase.lock().unwrap();
                    *cur_phase = phase;
                }
                self.current_percent.store(percent, Ordering::Relaxed);
                let log_line = format!("[PHASE] {} at {}%\n", phase, percent);
                let mut buf = self.buffer.lock().unwrap();
                buf.extend_from_slice(log_line.as_bytes());
            }
            CompilerEvent::Log(msg) => {
                let mut buf = self.buffer.lock().unwrap();
                buf.extend_from_slice(msg.as_bytes());
                if !msg.ends_with('\n') {
                    buf.push(b'\n');
                }
            }
        }
    }

    pub fn set_collecting(&self, enabled: bool) {
        self.collecting.store(enabled as usize, Ordering::Relaxed);
    }

    pub fn is_collecting(&self) -> bool {
        self.collecting.load(Ordering::Relaxed) != 0
    }

    pub fn take_buffer(&self) -> Vec<u8> {
        let mut buf = self.buffer.lock().unwrap();
        std::mem::take(&mut *buf)
    }

    pub fn flush_to_file(&self, path: Option<PathBuf>) -> std::io::Result<()> {
        let data = self.take_buffer();
        if data.is_empty() {
            return Ok(());
        }
        if let Some(file_path) = path {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file_path)?;
            file.write_all(&data)?;
        } else {
            let _ = std::io::stderr().write_all(&data);
        }
        Ok(())
    }

    pub fn get_current_phase(&self) -> &'static str {
        *self.current_phase.lock().unwrap()
    }

    pub fn get_exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::Relaxed) as i32
    }

    pub fn set_exit_code(&self, code: i32) {
        let code = code as usize;
        let current = self.exit_code.load(Ordering::Relaxed);
        if code > current {
            self.exit_code.store(code, Ordering::Relaxed);
        }
    }

    pub fn reset_exit_code(&self) {
        self.exit_code.store(0, Ordering::Relaxed);
    }
}

static DIAGNOSTIC_MANAGER: OnceLock<DiagnosticManager> = OnceLock::new();

fn get_manager() -> &'static DiagnosticManager {
    DIAGNOSTIC_MANAGER.get_or_init(DiagnosticManager::new)
}

// -----------------------------------------------------------------------------
// Global debug flag
// -----------------------------------------------------------------------------

static GLOBAL_DEBUG: AtomicBool = AtomicBool::new(false);

pub fn set_global_debug(enabled: bool) {
    // Enable virtual terminal processing on Windows (if available)
    vt::enable();

    GLOBAL_DEBUG.store(enabled, Ordering::Relaxed);
}

pub fn global_debug() -> bool {
    GLOBAL_DEBUG.load(Ordering::Relaxed)
}

// -----------------------------------------------------------------------------
// Test run active flag – suppresses per‑file phase updates during test runs.
// -----------------------------------------------------------------------------

static TEST_RUN_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_test_run_active(active: bool) {
    TEST_RUN_ACTIVE.store(active, Ordering::Relaxed);
}

pub fn test_run_active() -> bool {
    TEST_RUN_ACTIVE.load(Ordering::Relaxed)
}

// -----------------------------------------------------------------------------
// GUI terminal buffer forwarding
// -----------------------------------------------------------------------------

/// The GUI's terminal buffer to which all phase updates and logs are forwarded.
static GUI_TERMINAL: OnceLock<Arc<Mutex<TerminalBuffer>>> = OnceLock::new();

/// Set the GUI's terminal buffer to receive all diagnostic output.
pub fn set_gui_terminal(buffer: Arc<Mutex<TerminalBuffer>>) {
    let _ = GUI_TERMINAL.set(buffer);
    eprintln!("[DIAG] GUI_TERMINAL set");
}

// A flag to prevent recursion when debug logging calls emit_log.
static INSIDE_PUSH: AtomicBool = AtomicBool::new(false);

/// Push a line to the GUI terminal (if set) and request a UI refresh.
fn push_to_gui_terminal(line: String) {
    // Avoid recursion if debug_log tries to call us again.
    if INSIDE_PUSH.swap(true, Ordering::Relaxed) {
        return;
    }

    if DIAG_TRACE {
        println!(
            "[TRACE] push_to_gui_terminal: line.len={}, line={:?}",
            line.len(),
            line
        );
    }

    if let Some(term) = GUI_TERMINAL.get() {
        if let Ok(mut guard) = term.lock() {
            guard.push(line.clone());
            if DIAG_TRACE {
                println!("[TRACE] Pushed to buffer, total lines: {}", guard.len());
            }
        } else {
            if DIAG_TRACE {
                println!("[TRACE] Failed to lock terminal buffer");
            }
        }
    } else {
        if DIAG_TRACE {
            println!("[TRACE] GUI_TERMINAL not set, cannot push");
        }
    }

    // Request a UI refresh using the callback (set by the GUI)
    if let Some(callback) = GUI_REFRESH_CALLBACK.get() {
        callback();
        if DIAG_TRACE {
            println!("[TRACE] Called GUI refresh callback");
        }
    } else {
        if DIAG_TRACE {
            println!("[TRACE] GUI_REFRESH_CALLBACK not set, cannot refresh");
        }
    }

    INSIDE_PUSH.store(false, Ordering::Relaxed);
}

// -----------------------------------------------------------------------------
// Exit code helper
// -----------------------------------------------------------------------------

fn diagnostic_to_exit_code(diag: &Diagnostic) -> i32 {
    if diag.level != Level::Error {
        return 0;
    }

    if let Some(code) = &diag.code {
        match code.as_str() {
            "E0001" | "E0002" | "E0003" => exit_code::LEXICAL_ERROR,
            "E1001" | "E1002" | "E1003" => exit_code::SYNTAX_ERROR,
            "E2001" | "E2002" | "E2003" => exit_code::SEMANTIC_ERROR,
            "VX0306" | "VX0307" | "VX0308" | "VX0316" | "VX0317" | "VX0318" | "E3001" | "E3002" => {
                exit_code::IMPORT_ERROR
            }
            "E4001" => exit_code::IR_GENERATION_ERROR,
            "E5001" => exit_code::IO_ERROR,
            _ => exit_code::GENERIC_ERROR,
        }
    } else {
        let msg = diag.message.to_lowercase();
        if msg.contains("lexical") || msg.contains("token") {
            exit_code::LEXICAL_ERROR
        } else if msg.contains("syntax") || msg.contains("parse") {
            exit_code::SYNTAX_ERROR
        } else if msg.contains("type") || msg.contains("semantic") {
            exit_code::SEMANTIC_ERROR
        } else if msg.contains("import") || msg.contains("module") || msg.contains("use") {
            exit_code::IMPORT_ERROR
        } else if msg.contains("ir") || msg.contains("codegen") {
            exit_code::IR_GENERATION_ERROR
        } else if msg.contains("link") || msg.contains("linker") {
            exit_code::LINKER_ERROR
        } else if msg.contains("io") || msg.contains("file") || msg.contains("not found") {
            exit_code::IO_ERROR
        } else {
            exit_code::GENERIC_ERROR
        }
    }
}

// -----------------------------------------------------------------------------
// Pretty diagnostic formatting (returns colored string)
// -----------------------------------------------------------------------------

/// Format a diagnostic as a pretty, color‑coded string (with ANSI escapes).
/// This mirrors the stderr output but returns a String.
/// TODO: Add source context (file content, caret, underline) using the Span.
fn format_pretty_diagnostic(diag: &Diagnostic) -> String {
    let (level_str, colour) = match diag.level {
        Level::Error => ("error", "\x1b[31m"),
        Level::Warning => ("warning", "\x1b[33m"),
        Level::Note => ("note", "\x1b[36m"),
        Level::Help => ("help", "\x1b[32m"),
    };

    let mut out = String::new();
    out.push_str(colour);
    out.push_str(level_str);
    out.push_str("\x1b[0m");

    if let Some(code) = &diag.code {
        out.push('[');
        out.push_str(code);
        out.push(']');
    }

    out.push_str(": ");
    out.push_str(&diag.message);
    out.push('\n');

    if let Some(span) = &diag.span {
        out.push_str(&format!("  --> {}:{}\n", span.line, span.col));
        // TODO: read source file and include the line with a caret
        // This will be implemented when we have a source cache.
    }

    for sug in &diag.suggestions {
        out.push_str("  \x1b[32mhelp:\x1b[0m ");
        out.push_str(&sug.message);
        out.push('\n');
        if let Some(span) = &sug.span {
            out.push_str(&format!("       at {}:{}\n", span.line, span.col));
        }
    }

    out.push('\n');
    out
}

// -----------------------------------------------------------------------------
// Pretty printing (stdout)
// -----------------------------------------------------------------------------

fn render_pretty_diagnostic(diag: &Diagnostic) {
    let formatted = format_pretty_diagnostic(diag);
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(formatted.as_bytes()).unwrap();
    if DIAG_TRACE {
        println!(
            "[TRACE] render_pretty_diagnostic wrote {} bytes to stdout",
            formatted.len()
        );
    }
}

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

pub fn emit_diagnostic(diag: &Diagnostic) {
    let manager = get_manager();
    manager.emit(CompilerEvent::Diagnostic(diag.clone()));

    // Output to stdout if not collecting
    if !manager.is_collecting() {
        match determine_current_format() {
            OutputFormat::Json => {
                let json = diagnostic_to_json(diag);
                println!("{}", json);
            }
            OutputFormat::Pretty => {
                render_pretty_diagnostic(diag);
            }
            OutputFormat::Auto => unreachable!(),
        }
    }

    // Always push the pretty (colored) version to the GUI terminal if active.
    // This ensures that errors/warnings appear in the GUI during compilation.
    if GUI_TERMINAL.get().is_some() {
        let pretty = format_pretty_diagnostic(diag);
        if DIAG_TRACE {
            println!("[TRACE] emit_diagnostic pushing to GUI: {:?}", pretty);
        }
        push_to_gui_terminal(pretty);
    } else if DIAG_TRACE {
        println!("[TRACE] emit_diagnostic: GUI_TERMINAL not set, skipping GUI push");
    }
}

pub fn start_collecting() {
    let manager = get_manager();
    manager.set_collecting(true);
    manager.take_buffer();
    manager.reset_exit_code();
}

pub fn stop_collecting() -> Vec<Diagnostic> {
    let manager = get_manager();
    manager.set_collecting(false);
    let buffer = manager.take_buffer();
    let mut diags = Vec::new();
    for line in String::from_utf8_lossy(&buffer).lines() {
        if let Some(diag) = parse_diagnostic_from_json(line) {
            diags.push(diag);
        }
    }
    diags
}

/// Emit a phase update with a percentage.
/// This will both log to the internal buffer and forward to the GUI terminal and status bar.
/// If `test_run_active()` is true, we suppress GUI updates (except for "Test complete").
pub fn emit_phase_update(phase: &'static str, percent: usize) {
    let percent = percent.min(100);
    get_manager().emit(CompilerEvent::PhaseUpdate { phase, percent });

    // Log line for terminal / listbox (always record internally)
    let log_line = format!("[PHASE] {} at {}%", phase, percent);
    if global_debug() {
        println!("[DIAG] emit_phase_update: {}", log_line);
    }

    // Suppress GUI updates during a test run (unless it's the final "Test complete" phase)
    let should_update_gui = !test_run_active() || phase == "Test complete";

    if should_update_gui {
        push_to_gui_terminal(log_line);

        // Update status bar via callback (set by GUI)
        if let Some(callback) = GUI_PHASE_CALLBACK.get() {
            callback(phase, percent);
            if DIAG_TRACE {
                println!("[TRACE] Called GUI phase callback for phase: {}", phase);
            }
        } else {
            if DIAG_TRACE {
                println!("[TRACE] GUI_PHASE_CALLBACK not set, cannot update status");
            }
        }
    } else {
        if DIAG_TRACE {
            println!(
                "[TRACE] Suppressing phase update during test run: {}",
                phase
            );
        }
    }
}

/// Emit a log message.
/// This will both log to the internal buffer and forward to the GUI terminal.
/// If the GUI is active, the line is colorized before pushing.
/// Also, if `global_debug()` is true, the (colorized) line is printed to stdout.
pub fn emit_log(msg: String) {
    // Log to internal buffer
    get_manager().emit(CompilerEvent::Log(msg.clone()));

    // Colorize for GUI if active
    let gui_line = if GUI_TERMINAL.get().is_some() {
        let colored = colorize_prefix(&msg);
        if DIAG_TRACE {
            println!("[TRACE] emit_log: raw msg: {:?}", msg);
            println!("[TRACE] emit_log: colored msg: {:?}", colored);
        }
        colored
    } else {
        if DIAG_TRACE {
            println!("[TRACE] emit_log: GUI_TERMINAL not set, using raw msg");
        }
        msg.clone()
    };
    push_to_gui_terminal(gui_line);

    // If debug is enabled, also print to stdout (colorized)
    if global_debug() {
        let colored = colorize_prefix(&msg);
        println!("{}", colored);
    }
}

pub fn flush_logs(file_path: Option<PathBuf>) -> std::io::Result<()> {
    get_manager().flush_to_file(file_path)
}

pub fn get_exit_code() -> i32 {
    get_manager().get_exit_code()
}

pub fn set_exit_code(code: i32) {
    get_manager().set_exit_code(code);
}

pub fn spawn_ui_thread() -> thread::JoinHandle<()> {
    let manager = get_manager();
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    let handle = thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {
            let percent = manager.current_percent.load(Ordering::Relaxed);
            let phase = manager.get_current_phase();
            let filled = (percent as usize) / 5;
            let empty = 20 - filled;
            let bar = "█".repeat(filled) + &"░".repeat(empty);
            print!("\r\x1B[K[\x1b[32m{bar}\x1b[0m] {percent:3}% – {phase}");
            std::io::Write::flush(&mut std::io::stdout()).unwrap();
            thread::sleep(Duration::from_millis(100));
        }
        println!();
    });
    crate::ui::set_ui_running_flag(running);
    handle
}

pub fn stop_ui_thread(handle: thread::JoinHandle<()>) {
    crate::ui::stop_ui();
    handle.join().unwrap();
}

// -----------------------------------------------------------------------------
// JSON helpers
// -----------------------------------------------------------------------------

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn diagnostic_to_json(diag: &Diagnostic) -> String {
    let mut s = String::new();
    s.push('{');
    let level_str = match diag.level {
        Level::Error => "error",
        Level::Warning => "warning",
        Level::Note => "note",
        Level::Help => "help",
    };
    s.push_str(&format!("\"level\":\"{}\"", level_str));
    s.push_str(&format!(",\"message\":\"{}\"", escape_json(&diag.message)));
    if let Some(code) = &diag.code {
        s.push_str(&format!(",\"code\":\"{}\"", escape_json(code)));
    }
    if let Some(span) = &diag.span {
        s.push_str(&format!(
            ",\"span\":{{\"start\":{},\"end\":{},\"line\":{},\"col\":{}}}",
            span.start, span.end, span.line, span.col
        ));
    }
    if !diag.suggestions.is_empty() {
        s.push_str(",\"suggestions\":[");
        for (i, sug) in diag.suggestions.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('{');
            s.push_str(&format!("\"message\":\"{}\"", escape_json(&sug.message)));
            if let Some(span) = &sug.span {
                s.push_str(&format!(
                    ",\"span\":{{\"start\":{},\"end\":{},\"line\":{},\"col\":{}}}",
                    span.start, span.end, span.line, span.col
                ));
            }
            s.push('}');
        }
        s.push(']');
    }
    s.push('}');
    s
}

/// Parse a diagnostic from a JSON string (expected to be a single JSON object).
/// Used for both compiler diagnostics and LSP diagnostics.
pub fn parse_diagnostic_from_json(json: &str) -> Option<Diagnostic> {
    if !json.starts_with('{') || !json.ends_with('}') {
        return None;
    }
    let level = if json.contains("\"level\":\"error\"") || json.contains("\"severity\":1") {
        Level::Error
    } else if json.contains("\"level\":\"warning\"") || json.contains("\"severity\":2") {
        Level::Warning
    } else if json.contains("\"level\":\"note\"") || json.contains("\"severity\":3") {
        Level::Note
    } else if json.contains("\"level\":\"help\"") || json.contains("\"severity\":4") {
        Level::Help
    } else {
        return None;
    };
    let message = extract_json_string(json, "message")?;
    let code = extract_json_string(json, "code");

    // Try to parse range (LSP format: range -> start/end with line/character)
    let range = if let (Some(start_line), Some(start_col), Some(end_line), Some(end_col)) = (
        extract_json_number_from_object(json, "range", "start", "line"),
        extract_json_number_from_object(json, "range", "start", "character"),
        extract_json_number_from_object(json, "range", "end", "line"),
        extract_json_number_from_object(json, "range", "end", "character"),
    ) {
        Some(Range {
            start_line: start_line as u32,
            start_col: start_col as u32,
            end_line: end_line as u32,
            end_col: end_col as u32,
        })
    } else {
        None
    };

    Some(Diagnostic {
        level,
        message,
        code,
        span: None, // we don't convert for LSP
        range,
        suggestions: Vec::new(),
    })
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = json.find(&pattern)? + pattern.len();
    let mut end = start;
    let bytes = json.as_bytes();
    while end < bytes.len() && bytes[end] != b'"' {
        if bytes[end] == b'\\' {
            end += 1;
        }
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    let raw = &json[start..end];
    Some(raw.replace("\\\\", "\\").replace("\\\"", "\""))
}

fn extract_json_number_from_object(
    json: &str,
    outer: &str,
    inner: &str,
    key: &str,
) -> Option<usize> {
    let outer_pattern = format!("\"{}\":", outer);
    let outer_start = json.find(&outer_pattern)? + outer_pattern.len();
    // find the inner object
    let rest = &json[outer_start..];
    let inner_pattern = format!("\"{}\":", inner);
    let inner_start = rest.find(&inner_pattern)? + inner_pattern.len();
    let after_inner = &rest[inner_start..];
    let key_pattern = format!("\"{}\":", key);
    let key_start = after_inner.find(&key_pattern)? + key_pattern.len();
    let value_rest = &after_inner[key_start..];
    let end = value_rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value_rest.len());
    value_rest[..end].parse().ok()
}

// -----------------------------------------------------------------------------
// Counterexample formatting
// -----------------------------------------------------------------------------

pub fn format_counterexample(pairs: &[(&str, i64)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .map(|(name, val)| format!("{} = {}", name, val))
        .collect();
    parts.join(", ")
}

// -----------------------------------------------------------------------------
// UI thread flag
// -----------------------------------------------------------------------------
#[allow(dead_code)]
mod ui {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    static UI_RUNNING: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

    pub fn set_ui_running_flag(flag: Arc<AtomicBool>) {
        let _ = UI_RUNNING.set(flag);
    }

    pub fn stop_ui() {
        if let Some(flag) = UI_RUNNING.get() {
            flag.store(false, Ordering::Relaxed);
        }
    }
}

/// Log a debug message. If global debug is enabled, prints to stdout immediately.
/// Also forwards a **colored** version to the GUI terminal (always).
pub fn debug_log(msg: impl Into<String>) {
    let msg = msg.into();
    if global_debug() {
        // Push plain version to internal buffer (for file logs)
        get_manager().emit(CompilerEvent::Log(msg.clone()));

        // Always generate colored version for GUI
        let colored = colorize_prefix(&msg);
        if DIAG_TRACE {
            println!("[TRACE] debug_log: raw: {:?}", msg);
            println!("[TRACE] debug_log: colored: {:?}", colored);
        }
        push_to_gui_terminal(colored.clone());

        // Print the colored version to stdout (so colors appear in VS2022 and other consoles)
        println!("{}", colored);
    }
}
