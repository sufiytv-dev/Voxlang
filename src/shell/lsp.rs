// src/shell/lsp.rs – LSP client that spawns the current executable as the server
// Now includes a background reader thread to receive diagnostics and post them to the GUI.
// Graceful shutdown with timeout, forced kill, and non‑blocking thread detach.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::diagnostic::{Diagnostic, Level, debug_log};

// -----------------------------------------------------------------------------
// Win32 constants and types (minimal for message posting)
// -----------------------------------------------------------------------------

type HWND = *mut std::ffi::c_void;
type WPARAM = usize;
type LPARAM = isize;
type UINT = u32;
type BOOL = i32;

const WM_USER: UINT = 0x0400;
/// Custom message sent to the GUI with a `Box<Vec<Diagnostic>>` as lParam.
pub const WM_USER_DIAGNOSTICS: UINT = WM_USER + 2;

unsafe extern "system" {
    fn PostMessageW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> BOOL;
}

// -----------------------------------------------------------------------------
// JSON string escape helper
// -----------------------------------------------------------------------------

fn escape_json_string(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\x08' => escaped.push_str("\\b"),
            '\x0C' => escaped.push_str("\\f"),
            c if c.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => escaped.push(c),
        }
    }
    escaped
}

// -----------------------------------------------------------------------------
// LSP client struct
// -----------------------------------------------------------------------------

pub struct LspClient {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    next_id: u64,
    // Reader thread and stop flag for background processing
    reader_thread: Option<thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    // GUI window handle to post diagnostics
    hwnd: HWND,
}

impl LspClient {
    /// Starts the LSP server and a background reader thread.
    /// `hwnd` is the main GUI window to receive `WM_USER_DIAGNOSTICS`.
    pub fn start(hwnd: HWND) -> io::Result<Self> {
        debug_log("[LSP] Starting LSP client...");
        let exe = std::env::current_exe()?;
        let mut child = Command::new(exe)
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();

        // Convert HWND to usize so it can be sent between threads (Send + Sync).
        let hwnd_usize = hwnd as usize;

        // Spawn the reader thread – we move `stdout` into it and don't store it in the struct.
        let reader_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while !stop_flag_clone.load(Ordering::Relaxed) {
                // Read headers
                let mut content_length: Option<usize> = None;
                let mut headers_done = false;
                while !headers_done {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.is_empty() {
                        // EOF or error – exit thread
                        return;
                    }
                    let line = line.trim_end_matches(&['\r', '\n'][..]);
                    if line.is_empty() {
                        headers_done = true;
                        break;
                    }
                    if let Some(rest) = line.strip_prefix("Content-Length: ") {
                        if let Ok(len) = rest.parse::<usize>() {
                            content_length = Some(len);
                        }
                    }
                }

                if let Some(len) = content_length {
                    // Read the exact body
                    let mut body = vec![0u8; len];
                    if reader.read_exact(&mut body).is_err() {
                        // If we can't read the body, break
                        break;
                    }
                    // Convert to string
                    if let Ok(body_str) = String::from_utf8(body) {
                        // Try to parse as a notification
                        if let Some(diagnostics) = extract_diagnostics_from_notification(&body_str)
                        {
                            // Post diagnostics to GUI
                            let boxed = Box::new(diagnostics);
                            unsafe {
                                let hwnd = hwnd_usize as HWND;
                                PostMessageW(
                                    hwnd,
                                    WM_USER_DIAGNOSTICS,
                                    0,
                                    Box::into_raw(boxed) as isize,
                                );
                            }
                        }
                    }
                } else {
                    // Malformed message – skip
                    continue;
                }
            }
        });

        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            next_id: 0,
            reader_thread: Some(reader_thread),
            stop_flag,
            hwnd,
        })
    }

    fn send_message(&mut self, msg: &str) -> io::Result<()> {
        if let Some(stdin) = &mut self.stdin {
            write!(stdin, "Content-Length: {}\r\n\r\n{}", msg.len(), msg)?;
            stdin.flush()?;
        }
        Ok(())
    }

    pub fn send_initialize(&mut self, root_uri: &str) -> io::Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"initialize","params":{{"rootUri":"{}","capabilities":{{}}}}}}"#,
            id, root_uri
        );
        debug_log(&format!("[LSP] Sending initialize"));
        self.send_message(&msg)
    }

    pub fn send_open(&mut self, uri: &str, text: &str) -> io::Result<()> {
        let escaped = escape_json_string(text);
        let msg = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","languageId":"vox","version":1,"text":"{}"}}}}}}"#,
            uri, escaped
        );
        debug_log(&format!("[LSP] Sending didOpen for {}", uri));
        self.send_message(&msg)
    }

    pub fn send_change(&mut self, uri: &str, text: &str) -> io::Result<()> {
        let escaped = escape_json_string(text);
        let msg = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{}","version":2}},"contentChanges":[{{"text":"{}"}}]}}}}"#,
            uri, escaped
        );
        debug_log(&format!("[LSP] Sending didChange for {}", uri));
        self.send_message(&msg)
    }

    pub fn send_close(&mut self, uri: &str) -> io::Result<()> {
        let msg = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didClose","params":{{"textDocument":{{"uri":"{}"}}}}}}"#,
            uri
        );
        debug_log(&format!("[LSP] Sending didClose for {}", uri));
        self.send_message(&msg)
    }

    /// Gracefully shut down the LSP server.
    /// Sends shutdown and exit, closes stdin, waits briefly, kills if necessary,
    /// and detaches the reader thread.
    pub fn shutdown(mut self) -> io::Result<()> {
        eprintln!("[LSP] Shutting down LSP...");
        debug_log("[LSP] Shutting down LSP...");

        // Signal the reader thread to stop
        self.stop_flag.store(true, Ordering::Relaxed);

        // Send shutdown and exit
        let _ = self.send_message(r#"{"jsonrpc":"2.0","method":"shutdown","id":999}"#);
        let _ = self.send_message(r#"{"jsonrpc":"2.0","method":"exit"}"#);

        // Close stdin to unblock the child and force it to exit
        drop(self.stdin.take());
        eprintln!("[LSP] stdin closed");

        // Give the child a moment to exit gracefully
        thread::sleep(Duration::from_millis(100));

        // If the child is still running, kill it
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => {
                    eprintln!("[LSP] Child still running, killing...");
                    debug_log("[LSP] Child still running, killing...");
                    let _ = child.kill();
                    let _ = child.wait(); // reap
                }
                Ok(Some(_)) => {
                    eprintln!("[LSP] Child exited gracefully.");
                    debug_log("[LSP] Child exited gracefully.");
                }
                Err(e) => {
                    eprintln!("[LSP] Error waiting for child: {}", e);
                    debug_log(&format!("[LSP] Error waiting for child: {}", e));
                }
            }
        }

        // Detach the reader thread – it will exit when the process exits.
        // We don't need to join it because the process is terminating.
        drop(self.reader_thread.take());
        eprintln!("[LSP] Reader thread detached");

        eprintln!("[LSP] Shutdown complete");
        debug_log("[LSP] Shutdown complete");
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Helper: extract diagnostics from LSP notification
// -----------------------------------------------------------------------------

fn extract_diagnostics_from_notification(json: &str) -> Option<Vec<Diagnostic>> {
    // Check if it's a publishDiagnostics notification
    if !json.contains("\"method\":\"textDocument/publishDiagnostics\"") {
        return None;
    }
    // Find the diagnostics array inside params
    // We'll use a simple extraction: locate "diagnostics":[ ... ]
    let diag_start = json.find("\"diagnostics\":")? + 13; // length of the key
    let diag_part = &json[diag_start..];
    // Find the matching closing bracket
    let mut depth = 0;
    let mut start_idx = 0;
    for (i, ch) in diag_part.char_indices() {
        if ch == '[' {
            if depth == 0 {
                start_idx = i;
            }
            depth += 1;
        } else if ch == ']' {
            depth -= 1;
            if depth == 0 {
                let diag_array = &diag_part[start_idx..=i];
                // Now parse each diagnostic object
                let mut diagnostics = Vec::new();
                let mut current = String::new();
                let mut in_string = false;
                let mut escaped = false;
                let mut brace_depth = 0;
                for ch in diag_array.chars() {
                    if !in_string && ch == '{' {
                        brace_depth += 1;
                        if brace_depth == 1 {
                            current.clear();
                        }
                    }
                    if brace_depth > 0 {
                        current.push(ch);
                    }
                    if brace_depth == 1 && ch == '}' && !in_string {
                        // Complete a diagnostic object
                        if let Some(diag) = crate::diagnostic::parse_diagnostic_from_json(&current)
                        {
                            diagnostics.push(diag);
                        }
                        brace_depth = 0;
                        current.clear();
                    }
                    // Handle string state
                    if ch == '"' && !escaped {
                        in_string = !in_string;
                    }
                    if ch == '\\' && !escaped {
                        escaped = true;
                    } else {
                        escaped = false;
                    }
                }
                return Some(diagnostics);
            }
        }
    }
    None
}

// -----------------------------------------------------------------------------
// URI helper
// -----------------------------------------------------------------------------

/// Converts a file path to a `file://` URI.
pub fn path_to_uri(path: &Path) -> String {
    let path_str = path.to_str().expect("path must be UTF-8");
    if cfg!(windows) {
        format!("file:///{}", path_str.replace('\\', "/"))
    } else {
        format!("file://{}", path_str)
    }
}
