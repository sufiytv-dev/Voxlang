// src/shell/lsp.rs – LSP client with diagnostic forwarding via emit_diagnostic

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::ChildStdin;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};

// Windows‑specific constant for diagnostic notifications (used by windows_gui.rs)
#[cfg(windows)]
pub const WM_USER_DIAGNOSTICS: u32 = 0x0400 + 200;

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
// LSP client struct – stores HWND for Windows messaging (optional)
// -----------------------------------------------------------------------------

pub struct LspClient {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    next_id: u64,
    reader_thread: Option<thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    #[cfg(windows)]
    #[allow(dead_code)]
    hwnd: *mut std::os::raw::c_void, // stored but not used directly in lsp.rs
}

impl LspClient {
    #[cfg(windows)]
    pub fn start(hwnd: *mut std::os::raw::c_void) -> io::Result<Self> {
        debug_log("[LSP] Starting LSP client with HWND...");
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

        let reader_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while !stop_flag_clone.load(Ordering::Relaxed) {
                let mut content_length: Option<usize> = None;
                let headers_done = false;
                while !headers_done {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.is_empty() {
                        return;
                    }
                    let line = line.trim_end_matches(&['\r', '\n'][..]);
                    if line.is_empty() {
                        break;
                    }
                    if let Some(rest) = line.strip_prefix("Content-Length: ") {
                        if let Ok(len) = rest.parse::<usize>() {
                            content_length = Some(len);
                        }
                    }
                }

                if let Some(len) = content_length {
                    let mut body = vec![0u8; len];
                    if reader.read_exact(&mut body).is_err() {
                        break;
                    }
                    if let Ok(body_str) = String::from_utf8(body) {
                        if let Some(diagnostics) = extract_diagnostics_from_notification(&body_str)
                        {
                            // Forward each diagnostic via emit_diagnostic
                            for diag in diagnostics {
                                emit_diagnostic(&diag);
                            }
                        }
                    }
                } else {
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

    #[cfg(not(windows))]
    pub fn start() -> io::Result<Self> {
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

        let reader_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while !stop_flag_clone.load(Ordering::Relaxed) {
                let mut content_length: Option<usize> = None;
                let headers_done = false;
                while !headers_done {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.is_empty() {
                        return;
                    }
                    let line = line.trim_end_matches(&['\r', '\n'][..]);
                    if line.is_empty() {
                        break;
                    }
                    if let Some(rest) = line.strip_prefix("Content-Length: ") {
                        if let Ok(len) = rest.parse::<usize>() {
                            content_length = Some(len);
                        }
                    }
                }

                if let Some(len) = content_length {
                    let mut body = vec![0u8; len];
                    if reader.read_exact(&mut body).is_err() {
                        break;
                    }
                    if let Ok(body_str) = String::from_utf8(body) {
                        if let Some(diagnostics) = extract_diagnostics_from_notification(&body_str)
                        {
                            for diag in diagnostics {
                                emit_diagnostic(&diag);
                            }
                        }
                    }
                } else {
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

    #[allow(dead_code)]
    pub fn send_close(&mut self, uri: &str) -> io::Result<()> {
        let msg = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didClose","params":{{"textDocument":{{"uri":"{}"}}}}}}"#,
            uri
        );
        debug_log(&format!("[LSP] Sending didClose for {}", uri));
        self.send_message(&msg)
    }

    pub fn shutdown(mut self) -> io::Result<()> {
        debug_log("[LSP] Shutting down LSP...");
        self.stop_flag.store(true, Ordering::Relaxed);

        let _ = self.send_message(r#"{"jsonrpc":"2.0","method":"shutdown","id":999}"#);
        let _ = self.send_message(r#"{"jsonrpc":"2.0","method":"exit"}"#);

        drop(self.stdin.take());

        thread::sleep(Duration::from_millis(100));

        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => {
                    debug_log("[LSP] Child still running, killing...");
                    let _ = child.kill();
                    let _ = child.wait();
                }
                _ => {}
            }
        }

        drop(self.reader_thread.take());
        debug_log("[LSP] Shutdown complete");
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Helper: extract diagnostics and forward via emit_diagnostic
// -----------------------------------------------------------------------------

fn extract_diagnostics_from_notification(json: &str) -> Option<Vec<Diagnostic>> {
    if !json.contains("\"method\":\"textDocument/publishDiagnostics\"") {
        return None;
    }
    let diag_start = json.find("\"diagnostics\":")? + 13;
    let diag_part = &json[diag_start..];
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
                        if let Some(diag) = crate::diagnostic::parse_diagnostic_from_json(&current)
                        {
                            diagnostics.push(diag);
                        }
                        brace_depth = 0;
                        current.clear();
                    }
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

pub fn path_to_uri(path: &Path) -> String {
    let path_str = path.to_str().expect("path must be UTF-8");
    if cfg!(windows) {
        format!("file:///{}", path_str.replace('\\', "/"))
    } else {
        format!("file://{}", path_str)
    }
}
