// diagnostic.rs - Structured diagnostics with event buffering, UI thread support,
// and hybrid output (pretty/JSON). Tracks exit codes and global debug flag.

use crate::frontend::span::Span;
use std::fs;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

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
    pub span: Option<Span>,
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            message: message.into(),
            code: None,
            span: None,
            suggestions: Vec::new(),
        }
    }
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            message: message.into(),
            code: None,
            span: None,
            suggestions: Vec::new(),
        }
    }
    pub fn note(message: impl Into<String>) -> Self {
        Self {
            level: Level::Note,
            message: message.into(),
            code: None,
            span: None,
            suggestions: Vec::new(),
        }
    }
    pub fn help(message: impl Into<String>) -> Self {
        Self {
            level: Level::Help,
            message: message.into(),
            code: None,
            span: None,
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
    GLOBAL_DEBUG.store(enabled, Ordering::Relaxed);
}

pub fn global_debug() -> bool {
    GLOBAL_DEBUG.load(Ordering::Relaxed)
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
// Pretty printing
// -----------------------------------------------------------------------------

fn render_pretty_diagnostic(diag: &Diagnostic) {
    use std::io::Write;

    let (level_str, colour) = match diag.level {
        Level::Error => ("error", "\x1b[31m"),
        Level::Warning => ("warning", "\x1b[33m"),
        Level::Note => ("note", "\x1b[36m"),
        Level::Help => ("help", "\x1b[32m"),
    };

    let mut stderr = std::io::stderr().lock();

    write!(&mut stderr, "{}", colour).unwrap();
    write!(&mut stderr, "{}", level_str).unwrap();
    write!(&mut stderr, "\x1b[0m").unwrap();

    if let Some(code) = &diag.code {
        write!(&mut stderr, "[{}]", code).unwrap();
    }

    write!(&mut stderr, ": ").unwrap();
    writeln!(&mut stderr, "{}", diag.message).unwrap();

    if let Some(span) = &diag.span {
        writeln!(&mut stderr, "  --> {}:{}", span.line, span.col).unwrap();
    }

    for sug in &diag.suggestions {
        write!(&mut stderr, "  \x1b[32mhelp:\x1b[0m ").unwrap();
        writeln!(&mut stderr, "{}", sug.message).unwrap();
        if let Some(span) = &sug.span {
            writeln!(&mut stderr, "       at {}:{}", span.line, span.col).unwrap();
        }
    }

    writeln!(&mut stderr).unwrap();
}

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

pub fn emit_diagnostic(diag: &Diagnostic) {
    let manager = get_manager();
    manager.emit(CompilerEvent::Diagnostic(diag.clone()));

    if !manager.is_collecting() {
        match determine_current_format() {
            OutputFormat::Json => {
                let json = diagnostic_to_json(diag);
                eprintln!("{}", json);
            }
            OutputFormat::Pretty => {
                render_pretty_diagnostic(diag);
            }
            OutputFormat::Auto => unreachable!(),
        }
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

pub fn emit_phase_update(phase: &'static str, percent: usize) {
    let percent = percent.min(100);
    get_manager().emit(CompilerEvent::PhaseUpdate { phase, percent });
}

pub fn emit_log(msg: String) {
    if global_debug() {
        eprintln!("{}", msg);
    }
    get_manager().emit(CompilerEvent::Log(msg));
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

fn parse_diagnostic_from_json(line: &str) -> Option<Diagnostic> {
    if !line.starts_with('{') || !line.ends_with('}') {
        return None;
    }
    let level = if line.contains("\"level\":\"error\"") {
        Level::Error
    } else if line.contains("\"level\":\"warning\"") {
        Level::Warning
    } else if line.contains("\"level\":\"note\"") {
        Level::Note
    } else if line.contains("\"level\":\"help\"") {
        Level::Help
    } else {
        return None;
    };
    let message = extract_json_string(line, "message")?;
    let code = extract_json_string(line, "code");
    Some(Diagnostic {
        level,
        message,
        code,
        span: None,
        suggestions: Vec::new(),
    })
}

fn extract_json_string(line: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = line.find(&pattern)? + pattern.len();
    let mut end = start;
    let bytes = line.as_bytes();
    while end < bytes.len() && bytes[end] != b'"' {
        if bytes[end] == b'\\' {
            end += 1;
        }
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    let raw = &line[start..end];
    let unescaped = raw.replace("\\\\", "\\").replace("\\\"", "\"");
    Some(unescaped)
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

/// Log a debug message. If global debug is enabled, prints to stderr immediately.
pub fn debug_log(msg: impl Into<String>) {
    let msg = msg.into();
    if global_debug() {
        eprintln!("{}", msg);
    }
    emit_log(msg);
}
