// src/lsp/mod.rs – Language Server Protocol server with manual JSON (no serde)

mod io;

use crate::CacheConfig;
use crate::diagnostic;
use crate::frontend::lexer::Lexer;
use crate::import::resolve_imports;
use crate::module::ModuleResolver;
use crate::parser::Parser;
use crate::semantics::SemanticAnalyzer;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct LspServer {
    vfs: HashMap<PathBuf, String>,
    config: CacheConfig,
}

impl LspServer {
    fn new() -> Self {
        let config = CacheConfig {
            no_cache: true,
            reuse_proofs: false,
            reuse_bitcode: false,
            offline: true,
            trust_modules: false,
        };
        Self {
            vfs: HashMap::new(),
            config,
        }
    }

    fn run(&mut self) -> Result<(), String> {
        eprintln!("Voxlang Language Server running...");
        loop {
            let request = match io::read_message() {
                Ok(msg) => msg,
                Err(e) => {
                    eprintln!("Error reading message: {}", e);
                    continue;
                }
            };
            let (method, id, params) = parse_request(&request)?;
            match method.as_str() {
                "initialize" => self.handle_initialize(&id, &params)?,
                "initialized" => self.handle_initialized(&id, &params)?,
                "textDocument/didOpen" => self.handle_did_open(&id, &params)?,
                "textDocument/didChange" => self.handle_did_change(&id, &params)?,
                "textDocument/didClose" => self.handle_did_close(&id, &params)?,
                "shutdown" => self.handle_shutdown(&id, &params)?,
                "exit" => {
                    self.handle_exit()?;
                    break;
                }
                _ => {
                    eprintln!("Unhandled method: {}", method);
                }
            }
        }
        Ok(())
    }

    fn handle_initialize(&mut self, id: &str, _params: &str) -> Result<(), String> {
        let response = format!(
            r#"{{"jsonrpc":"2.0","id":{},"result":{{"capabilities":{{"textDocumentSync":{{"openClose":true,"change":1}},"hoverProvider":false,"completionProvider":false}},"serverInfo":{{"name":"Voxlang Language Server","version":{}"}}}}"#,
            id,
            env!("CARGO_PKG_VERSION")
        );
        io::write_message(&response)?;
        Ok(())
    }

    fn handle_initialized(&mut self, _id: &str, _params: &str) -> Result<(), String> {
        eprintln!("Client initialized");
        Ok(())
    }

    fn handle_did_open(&mut self, _id: &str, params: &str) -> Result<(), String> {
        let (uri, text) = extract_uri_and_text(params)?;
        let path = uri_to_path(&uri)?;
        self.vfs.insert(path.clone(), text.to_string());
        self.publish_diagnostics(&path, &text)?;
        Ok(())
    }

    fn handle_did_change(&mut self, _id: &str, params: &str) -> Result<(), String> {
        let (uri, text) = extract_uri_and_text(params)?;
        let path = uri_to_path(&uri)?;
        self.vfs.insert(path.clone(), text.to_string());
        self.publish_diagnostics(&path, &text)?;
        Ok(())
    }

    fn handle_did_close(&mut self, _id: &str, params: &str) -> Result<(), String> {
        let uri = extract_uri_from_params(params)?;
        let path = uri_to_path(&uri)?;
        self.vfs.remove(&path);
        self.clear_diagnostics(&path)?;
        Ok(())
    }

    fn handle_shutdown(&mut self, id: &str, _params: &str) -> Result<(), String> {
        let response = format!(r#"{{"jsonrpc":"2.0","id":{},"result":null}}"#, id);
        io::write_message(&response)?;
        Ok(())
    }

    fn handle_exit(&mut self) -> Result<(), String> {
        eprintln!("Exiting LSP server.");
        Ok(())
    }

    fn publish_diagnostics(&self, path: &Path, source: &str) -> Result<(), String> {
        let diags = analyze_source(source, path, &self.config);
        let lsp_diagnostics = diagnostics_to_json(&diags, source);
        let uri = path_to_uri(path);
        let notification = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{{"uri":"{}","diagnostics":{}}}}}"#,
            uri, lsp_diagnostics
        );
        io::write_message(&notification)?;
        Ok(())
    }

    fn clear_diagnostics(&self, path: &Path) -> Result<(), String> {
        let uri = path_to_uri(path);
        let notification = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{{"uri":"{}","diagnostics":[]}}}}"#,
            uri
        );
        io::write_message(&notification)?;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Manual JSON parsing helpers (minimal, only for LSP methods we need)
// -----------------------------------------------------------------------------

/// Extract (method, id, params) from a JSON-RPC request.
/// Returns (method, id_string, params_string). id can be number or string.
fn parse_request(request: &str) -> Result<(String, String, String), String> {
    // Very simplistic: find "method":"...", "id":..., "params":...
    let method = extract_string_field(request, "method")?;
    let id = extract_id(request)?;
    let params = extract_object_field(request, "params")?;
    Ok((method, id, params))
}

fn extract_string_field(json: &str, field: &str) -> Result<String, String> {
    let pattern = format!("\"{}\":\"", field);
    let start = json
        .find(&pattern)
        .ok_or_else(|| format!("Missing field: {}", field))?
        + pattern.len();
    let end = json[start..].find('\"').ok_or("Missing closing quote")?;
    Ok(json[start..start + end].to_string())
}

fn extract_id(json: &str) -> Result<String, String> {
    let pattern = "\"id\":";
    let start = json.find(pattern).ok_or("Missing id field")? + pattern.len();
    let rest = &json[start..];
    if rest.starts_with('\"') {
        // string id
        let end = rest[1..].find('\"').ok_or("Missing closing quote for id")?;
        Ok(rest[1..1 + end].to_string())
    } else {
        // number id
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        Ok(rest[..end].to_string())
    }
}

fn extract_object_field(json: &str, field: &str) -> Result<String, String> {
    let pattern = format!("\"{}\":", field);
    let start = json
        .find(&pattern)
        .ok_or_else(|| format!("Missing field: {}", field))?
        + pattern.len();
    let rest = &json[start..];
    let mut depth = 0;
    let mut end = 0;
    for (i, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    if end == 0 {
        return Err(format!("Could not find object for field: {}", field));
    }
    Ok(rest[..end].to_string())
}

/// Extract uri and text from "params" object of didOpen/didChange.
/// Expects: {"textDocument":{"uri":"...","text":"..."}}
fn extract_uri_and_text(params: &str) -> Result<(String, String), String> {
    let uri = extract_string_field(params, "uri")?;
    let text = extract_string_field(params, "text")?;
    Ok((uri, text))
}

/// Extract only the uri from params (for didClose)
fn extract_uri_from_params(params: &str) -> Result<String, String> {
    extract_string_field(params, "uri")
}

// -----------------------------------------------------------------------------
// Manual JSON construction for diagnostics
// -----------------------------------------------------------------------------

fn diagnostics_to_json(diags: &[diagnostic::Diagnostic], source: &str) -> String {
    let mut parts = Vec::new();
    for diag in diags {
        parts.push(diagnostic_to_json(diag, source));
    }
    format!("[{}]", parts.join(","))
}

fn diagnostic_to_json(diag: &diagnostic::Diagnostic, source: &str) -> String {
    let span = diag
        .span
        .unwrap_or_else(|| crate::frontend::span::Span::new(0, 0, 0, 0));
    let (start_line, start_char) = offset_to_line_col(source, span.start);
    let (end_line, end_char) = offset_to_line_col(source, span.end);
    let severity = match diag.level {
        diagnostic::Level::Error => 1,
        diagnostic::Level::Warning => 2,
        diagnostic::Level::Note => 3,
        diagnostic::Level::Help => 4,
    };
    let message = escape_json(&diag.message);
    let code = diag.code.as_deref().unwrap_or("VX0000");
    format!(
        r#"{{"range":{{"start":{{"line":{},"character":{}}},"end":{{"line":{},"character":{}}},"severity":{},"message":"{}","source":"vox","code":"{}"}}"#,
        start_line, start_char, end_line, end_char, severity, message, code
    )
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// -----------------------------------------------------------------------------
// Utility functions (unchanged)
// -----------------------------------------------------------------------------

fn analyze_source(source: &str, path: &Path, config: &CacheConfig) -> Vec<diagnostic::Diagnostic> {
    diagnostic::start_collecting();

    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(()) => return diagnostic::stop_collecting(),
    };
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse();
    let mut resolver = ModuleResolver::new(path, config);
    let (resolved_ast, import_errors) = resolve_imports(&ast, &mut resolver);
    if import_errors {
        eprintln!("Import resolution errors occurred during LSP analysis");
    }
    let mut semantic = SemanticAnalyzer::new();
    let ffi_functions = vec![];
    semantic.register_ffi_signatures(ffi_functions);
    semantic.check(&resolved_ast);

    diagnostic::stop_collecting()
}

fn offset_to_line_col(source: &str, offset: usize) -> (u32, u32) {
    let mut line = 0;
    let mut col = 0;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn uri_to_path(uri: &str) -> Result<PathBuf, String> {
    if let Some(path_str) = uri.strip_prefix("file://") {
        let path_str = if cfg!(windows) && path_str.starts_with('/') {
            &path_str[1..]
        } else {
            path_str
        };
        Ok(PathBuf::from(path_str))
    } else {
        Err(format!("Unsupported URI scheme: {}", uri))
    }
}

fn path_to_uri(path: &Path) -> String {
    let path_str = path.to_str().expect("path must be valid UTF-8 for LSP URI");
    if cfg!(windows) {
        format!("file:///{}", path_str.replace('\\', "/"))
    } else {
        format!("file://{}", path_str)
    }
}

pub fn run_server() -> Result<(), String> {
    let mut server = LspServer::new();
    server.run()
}
