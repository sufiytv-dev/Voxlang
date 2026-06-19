// src/lsp/mod.rs – Language Server Protocol server with manual JSON (no serde)

mod io;

use crate::CacheConfig;
use crate::diagnostic::{Diagnostic, Level};
use crate::diagnostic::{debug_log, start_collecting, stop_collecting};
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
        debug_log("[LSP] Voxlang Language Server running...");
        loop {
            let request = match io::read_message() {
                Ok(msg) => msg,
                Err(e) => {
                    debug_log(&format!("[LSP] Error reading message: {}", e));
                    continue;
                }
            };
            let (method, id, params) = match parse_request(&request) {
                Ok(v) => v,
                Err(e) => {
                    debug_log(&format!("[LSP] Failed to parse request: {}", e));
                    // Send a parse error response
                    let error_response =
                        r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"}}"#;
                    let _ = io::write_message(error_response);
                    continue;
                }
            };
            match method.as_str() {
                "initialize" => {
                    if let Err(e) = self.handle_initialize(id.as_deref(), &params) {
                        debug_log(&format!("[LSP] Error in initialize: {}", e));
                    }
                }
                "initialized" => {
                    if let Err(e) = self.handle_initialized(id.as_deref(), &params) {
                        debug_log(&format!("[LSP] Error in initialized: {}", e));
                    }
                }
                "textDocument/didOpen" => {
                    if let Err(e) = self.handle_did_open(id.as_deref(), &params) {
                        debug_log(&format!("[LSP] Error in didOpen: {}", e));
                    }
                }
                "textDocument/didChange" => {
                    if let Err(e) = self.handle_did_change(id.as_deref(), &params) {
                        debug_log(&format!("[LSP] Error in didChange: {}", e));
                    }
                }
                "textDocument/didClose" => {
                    if let Err(e) = self.handle_did_close(id.as_deref(), &params) {
                        debug_log(&format!("[LSP] Error in didClose: {}", e));
                    }
                }
                "shutdown" => {
                    if let Err(e) = self.handle_shutdown(id.as_deref(), &params) {
                        debug_log(&format!("[LSP] Error in shutdown: {}", e));
                    }
                }
                "exit" => {
                    if let Err(e) = self.handle_exit() {
                        debug_log(&format!("[LSP] Error in exit: {}", e));
                    }
                    break;
                }
                _ => {
                    debug_log(&format!("[LSP] Unhandled method: {}", method));
                    // Send method not found
                    let id_str = id.as_deref().unwrap_or("null");
                    let error_response = format!(
                        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":-32601,"message":"Method not found"}}}}"#,
                        id_str
                    );
                    let _ = io::write_message(&error_response);
                }
            }
        }
        Ok(())
    }

    fn handle_initialize(&mut self, id: Option<&str>, _params: &str) -> Result<(), String> {
        if let Some(id) = id {
            let response = format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{{"capabilities":{{"textDocumentSync":{{"openClose":true,"change":1}},"hoverProvider":false,"completionProvider":false}},"serverInfo":{{"name":"Voxlang Language Server","version":{}"}}}}"#,
                id,
                env!("CARGO_PKG_VERSION")
            );
            io::write_message(&response)?;
        }
        Ok(())
    }

    fn handle_initialized(&mut self, _id: Option<&str>, _params: &str) -> Result<(), String> {
        debug_log("[LSP] Client initialized");
        Ok(())
    }

    fn handle_did_open(&mut self, _id: Option<&str>, params: &str) -> Result<(), String> {
        let (uri, text) = extract_uri_and_text(params)?;
        let path = uri_to_path(&uri)?;
        self.vfs.insert(path.clone(), text.to_string());
        self.publish_diagnostics(&path, &text)?;
        Ok(())
    }

    fn handle_did_change(&mut self, _id: Option<&str>, params: &str) -> Result<(), String> {
        let (uri, text) = extract_uri_and_text(params)?;
        let path = uri_to_path(&uri)?;
        self.vfs.insert(path.clone(), text.to_string());
        self.publish_diagnostics(&path, &text)?;
        Ok(())
    }

    fn handle_did_close(&mut self, _id: Option<&str>, params: &str) -> Result<(), String> {
        let uri = extract_uri_from_params(params)?;
        let path = uri_to_path(&uri)?;
        self.vfs.remove(&path);
        self.clear_diagnostics(&path)?;
        Ok(())
    }

    fn handle_shutdown(&mut self, id: Option<&str>, _params: &str) -> Result<(), String> {
        if let Some(id) = id {
            let response = format!(r#"{{"jsonrpc":"2.0","id":{},"result":null}}"#, id);
            io::write_message(&response)?;
        }
        Ok(())
    }

    fn handle_exit(&mut self) -> Result<(), String> {
        debug_log("[LSP] Exiting LSP server.");
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
// Robust JSON parsing helpers
// -----------------------------------------------------------------------------

/// Parse a JSON string value with full escape handling.
/// Expects the string to start with a double quote and end with a double quote.
fn parse_json_string(s: &str) -> Result<String, String> {
    if !s.starts_with('"') {
        return Err("Expected opening quote".to_string());
    }
    let mut result = String::new();
    let mut chars = s.chars().skip(1); // skip opening quote
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if escaped {
            match ch {
                '"' => result.push('"'),
                '\\' => result.push('\\'),
                '/' => result.push('/'),
                'b' => result.push('\x08'),
                'f' => result.push('\x0C'),
                'n' => result.push('\n'),
                'r' => result.push('\r'),
                't' => result.push('\t'),
                'u' => {
                    // Parse 4 hex digits
                    let mut hex = String::with_capacity(4);
                    for _ in 0..4 {
                        if let Some(c) = chars.next() {
                            hex.push(c);
                        } else {
                            return Err("Incomplete Unicode escape".to_string());
                        }
                    }
                    let code = u32::from_str_radix(&hex, 16)
                        .map_err(|_| format!("Invalid Unicode escape: \\u{}", hex))?;
                    if let Some(c) = char::from_u32(code) {
                        result.push(c);
                    } else {
                        return Err(format!("Invalid Unicode code point: {}", code));
                    }
                }
                _ => return Err(format!("Invalid escape character: \\{}", ch)),
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            // Closing quote – end of string
            return Ok(result);
        } else {
            result.push(ch);
        }
    }
    Err("Unterminated string".to_string())
}

/// Extract a JSON string value by key, handling escapes correctly.
fn extract_string_field(json: &str, key: &str) -> Result<String, String> {
    let pattern = format!("\"{}\":", key);
    let start = json
        .find(&pattern)
        .ok_or_else(|| format!("Missing field: {}", key))?
        + pattern.len();
    let rest = &json[start..];
    // Skip whitespace
    let mut idx = 0;
    while idx < rest.len() && rest.chars().nth(idx).unwrap().is_whitespace() {
        idx += 1;
    }
    let rest = &rest[idx..];
    parse_json_string(rest)
}

/// Extract (method, id, params) from a JSON-RPC request.
/// Returns (method, id_string, params_string). id can be number or string, or None.
fn parse_request(request: &str) -> Result<(String, Option<String>, String), String> {
    let method = extract_string_field(request, "method")?;
    let id = extract_id_optional(request)?;
    let params = extract_object_field(request, "params")?;
    Ok((method, id, params))
}

fn extract_id_optional(json: &str) -> Result<Option<String>, String> {
    let pattern = "\"id\":";
    if let Some(start) = json.find(pattern) {
        let rest = &json[start + pattern.len()..];
        if rest.starts_with('\"') {
            // string id
            parse_json_string(rest).map(Some)
        } else {
            // number id
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            Ok(Some(rest[..end].to_string()))
        }
    } else {
        Ok(None)
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

/// Escape a string for JSON output.
fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn diagnostics_to_json(diags: &[Diagnostic], source: &str) -> String {
    let mut parts = Vec::new();
    for diag in diags {
        parts.push(diagnostic_to_json(diag, source));
    }
    format!("[{}]", parts.join(","))
}

fn diagnostic_to_json(diag: &Diagnostic, source: &str) -> String {
    let span = diag
        .span
        .unwrap_or_else(|| crate::frontend::span::Span::new(0, 0, 0, 0));
    let (start_line, start_char) = offset_to_line_col(source, span.start);
    let (end_line, end_char) = offset_to_line_col(source, span.end);
    let severity = match diag.level {
        Level::Error => 1,
        Level::Warning => 2,
        Level::Note => 3,
        Level::Help => 4,
    };
    let message = escape_json(&diag.message);
    let code = diag.code.as_deref().unwrap_or("VX0000");
    format!(
        r#"{{"range":{{"start":{{"line":{},"character":{}}},"end":{{"line":{},"character":{}}},"severity":{},"message":"{}","source":"vox","code":"{}"}}"#,
        start_line, start_char, end_line, end_char, severity, message, code
    )
}

// -----------------------------------------------------------------------------
// Utility functions
// -----------------------------------------------------------------------------

fn analyze_source(source: &str, path: &Path, config: &CacheConfig) -> Vec<Diagnostic> {
    start_collecting();

    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(()) => return stop_collecting(),
    };
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse();
    let mut resolver = ModuleResolver::new(path, config);
    let (resolved_ast, import_errors) = resolve_imports(&ast, &mut resolver);
    if import_errors {
        debug_log("[LSP] Import resolution errors occurred during LSP analysis");
    }
    let mut semantic = SemanticAnalyzer::new();
    let ffi_functions = vec![];
    semantic.register_ffi_signatures(ffi_functions);
    semantic.check(&resolved_ast);

    stop_collecting()
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
