// src/shell/tui.rs – Terminal UI implementation (fallback for non‑Windows)

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

use crate::CacheConfig;
use crate::host_triple;

use super::editor::Editor;
use super::lsp::LspClient;
use super::runner::compile_and_run_file;
use super::terminal::TerminalBuffer;
use super::tty; // raw terminal I/O

// -----------------------------------------------------------------------------
// Workspace state
// -----------------------------------------------------------------------------
struct WorkspaceState {
    editor: Editor,
    terminal: TerminalBuffer,
    lsp: Option<LspClient>,
    compilation_in_progress: Arc<AtomicBool>,
}

impl WorkspaceState {
    fn new() -> Self {
        Self {
            editor: Editor::new(),
            terminal: TerminalBuffer::new(),
            lsp: None,
            compilation_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }

    fn start_lsp(&mut self) -> io::Result<()> {
        let mut lsp = LspClient::start()?;
        lsp.send_initialize("file://")?;
        if let Some(path) = self.editor.file_path() {
            let uri = super::lsp::path_to_uri(path);
            let content = self.editor.lines().join("\n");
            lsp.send_open(&uri, &content)?;
        }
        self.lsp = Some(lsp);
        Ok(())
    }

    fn run_file(&mut self) {
        if self.compilation_in_progress.load(Ordering::SeqCst) {
            self.terminal
                .push("Compilation already in progress...".to_string());
            return;
        }
        self.compilation_in_progress.store(true, Ordering::SeqCst);
        let path = match self.editor.file_path().cloned() {
            Some(p) => p,
            None => {
                self.terminal
                    .push("No file open. Use :open <path>".to_string());
                self.compilation_in_progress.store(false, Ordering::SeqCst);
                return;
            }
        };
        if let Err(e) = self.editor.save() {
            self.terminal.push(format!("Save failed: {}", e));
            self.compilation_in_progress.store(false, Ordering::SeqCst);
            return;
        }
        self.terminal.clear();
        self.terminal.push(format!("Compiling {}", path.display()));
        let terminal = Arc::new(Mutex::new(self.terminal.clone()));
        let target = host_triple();
        let config = CacheConfig {
            no_cache: true,
            reuse_proofs: false,
            reuse_bitcode: false,
            offline: true,
            trust_modules: false,
        };
        let comp_in_progress = self.compilation_in_progress.clone();
        thread::spawn(move || {
            let res = compile_and_run_file(&path, &target, &config);
            let mut term = terminal.lock().unwrap();
            match res {
                Ok(output) => {
                    for line in output.lines {
                        term.push(line);
                    }
                }
                Err(e) => term.push(format!("Error: {}", e)),
            }
            comp_in_progress.store(false, Ordering::SeqCst);
        });
    }
}

// -----------------------------------------------------------------------------
// Command parsing
// -----------------------------------------------------------------------------
fn parse_command(line: &str) -> Option<&str> {
    if line.starts_with(':') {
        Some(&line[1..])
    } else {
        None
    }
}

// -----------------------------------------------------------------------------
// Main loop
// -----------------------------------------------------------------------------
pub fn run(_hide_console: bool) -> Result<(), String> {
    tty::enable_raw_mode().map_err(|e| format!("tty init: {}", e))?;

    let mut state = WorkspaceState::new();
    let mut need_redraw = true;

    // Load file from command line argument if provided
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        if let Err(e) = state.editor.load_file(Path::new(&args[1])) {
            eprintln!("Error loading file: {}", e);
        } else if let Err(e) = state.start_lsp() {
            eprintln!("LSP start error: {}", e);
        }
    }

    loop {
        if need_redraw {
            let (term_w, term_h) = tty::terminal_size().unwrap_or((80, 24));
            let editor_height = term_h / 2;
            let terminal_height = term_h - editor_height - 1;

            print!("\x1b[2J\x1b[1;1H");
            print!("{}", state.editor.render(editor_height, term_w));
            println!("\x1b[{};1H{}", editor_height + 1, "─".repeat(term_w));
            print!("\x1b[{};1H", editor_height + 2);
            print!("{}", state.terminal.render(terminal_height, term_w));
            let status = if let Some(path) = state.editor.file_path() {
                path.file_name().unwrap().to_string_lossy()
            } else {
                "[no file]".into()
            };
            print!(
                "\x1b[{};1H\x1b[7m {} | F5=Run | :open <file> | :quit \x1b[0m",
                term_h, status
            );
            io::stdout().flush().map_err(|e| e.to_string())?;
            need_redraw = false;
        }

        let key = tty::read_key().map_err(|e| e.to_string())?;

        // Escape sequences (arrows, home, end)
        if key == b"\x1b" {
            let mut seq = [0; 2];
            if io::stdin().read(&mut seq).is_ok() && seq[0] == b'[' {
                let mut c = [0];
                if io::stdin().read(&mut c).is_ok() {
                    match c[0] {
                        b'A' => state.editor.move_up(),
                        b'B' => state.editor.move_down(),
                        b'C' => state.editor.move_right(),
                        b'D' => state.editor.move_left(),
                        b'H' => state.editor.home(),
                        b'F' => state.editor.end(),
                        _ => {}
                    }
                }
            }
            need_redraw = true;
            continue;
        }

        // F5 detection: ESC [ 1 5 ~
        if key.len() >= 3 && key[0] == 0x1b && key[1] == b'[' && key[2] == b'1' {
            let mut rest = vec![0; 4];
            if io::stdin().read(&mut rest).is_ok() && rest[0] == b'5' && rest[1] == b'~' {
                state.run_file();
                need_redraw = true;
                continue;
            }
        }

        // Ctrl+R as alternate run
        if key == b"\x12" {
            state.run_file();
            need_redraw = true;
            continue;
        }

        // Normal key handling
        match key[0] {
            b'\r' => {
                let line = state.editor.current_line().to_string();
                if let Some(cmd) = parse_command(&line) {
                    if cmd == "quit" {
                        break;
                    } else if cmd.starts_with("open ") {
                        let path = &cmd[5..];
                        match state.editor.load_file(Path::new(path)) {
                            Ok(_) => {
                                state.terminal.push(format!("Opened {}", path));
                                if let Err(e) = state.start_lsp() {
                                    state.terminal.push(format!("LSP error: {}", e));
                                }
                            }
                            Err(e) => state.terminal.push(format!("Open error: {}", e)),
                        }
                        state.editor.clear_line();
                    } else {
                        state.terminal.push(format!("Unknown command: {}", cmd));
                        state.editor.clear_line();
                    }
                } else {
                    state.editor.newline();
                }
                need_redraw = true;
            }
            b'\x7f' => state.editor.backspace(),
            b'\t' => state.editor.insert_char(' '),
            c if c >= 32 && c < 127 => {
                state.editor.insert_char(c as char);
                // Notify LSP of change
                if let Some(lsp) = &mut state.lsp {
                    if let Some(path) = state.editor.file_path() {
                        let uri = super::lsp::path_to_uri(path);
                        let content = state.editor.lines().join("\n");
                        let _ = lsp.send_change(&uri, &content);
                    }
                }
                need_redraw = true;
            }
            _ => {}
        }
        need_redraw = true;
    }

    tty::disable_raw_mode().ok();
    if let Some(lsp) = state.lsp {
        let _ = lsp.shutdown();
    }
    Ok(())
}
