// src/lsp/io.rs – JSON‑RPC message framing over stdio
//
// Implements the LSP wire protocol: headers terminated by \r\n\r\n, then body.
// Only Content-Length is required; other headers (Content-Type) are ignored.

use std::io::{self, BufRead, Read, Write};
use std::str;

/// Reads a single LSP message from stdin.
/// Returns the JSON body as a string, or an error.
pub fn read_message() -> Result<String, String> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("EOF while reading headers".to_string());
        }
        let line = line.trim_end_matches(&['\r', '\n'][..]);
        if line.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = line.strip_prefix("Content-Length: ") {
            let len = rest
                .parse::<usize>()
                .map_err(|e| format!("Invalid Content-Length: {}", e))?;
            content_length = Some(len);
        }
        // Other headers ignored per spec
    }

    let len = content_length.ok_or_else(|| "Missing Content-Length header".to_string())?;
    let mut buffer = vec![0u8; len];
    reader
        .read_exact(&mut buffer)
        .map_err(|e| format!("Failed to read body ({} bytes): {}", len, e))?;
    let body = str::from_utf8(&buffer).map_err(|e| format!("Invalid UTF-8 in body: {}", e))?;
    Ok(body.to_string())
}

/// Writes a JSON‑RPC response to stdout with the correct header.
pub fn write_message(response: &str) -> Result<(), String> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    write!(handle, "Content-Length: {}\r\n\r\n", response.len()).map_err(|e| e.to_string())?;
    write!(handle, "{}", response).map_err(|e| e.to_string())?;
    handle.flush().map_err(|e| e.to_string())?;
    Ok(())
}

