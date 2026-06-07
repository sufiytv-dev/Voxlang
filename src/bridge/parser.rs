// src/bridge/parser.rs – C function declaration parser

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};

/// Represents a C function declaration.
#[derive(Debug, Clone)]
pub struct CFunction {
    pub name: String,
    pub return_type: String,
    pub param_types: Vec<String>,
    pub param_names: Vec<String>,
}

/// Parse a C header and return a list of function declarations.
/// On any parse error, emits a diagnostic and returns an error.
pub fn parse_header(header: &str) -> Result<Vec<CFunction>, String> {
    debug_log("bridge: parsing header");
    let cleaned = preprocess_header(header)?;
    let mut functions = Vec::new();

    for line in cleaned.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((ret, name, params_str)) = parse_function_decl(line) {
            let (param_types, param_names) = parse_params(&params_str).map_err(|err_msg| {
                let diag =
                    Diagnostic::error(format!("Failed to parse function '{}': {}", name, err_msg))
                        .with_code("VX0402");
                emit_diagnostic(&diag);
                err_msg
            })?;
            functions.push(CFunction {
                name,
                return_type: ret,
                param_types,
                param_names,
            });
        } else {
            debug_log(format!("bridge: skipping non‑function line: {}", line));
        }
    }
    Ok(functions)
}

/// Parse a single line like "int add(int a, int b);" into (return_type, name, params_str)
fn parse_function_decl(line: &str) -> Option<(String, String, String)> {
    let line = line.trim();
    if !line.ends_with(';') {
        return None;
    }
    let line = &line[..line.len() - 1];

    let paren_open = line.find('(')?;
    let mut paren_depth = 0;
    let mut paren_close = None;
    for (i, ch) in line.chars().enumerate() {
        match ch {
            '(' => paren_depth += 1,
            ')' => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    paren_close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = paren_close?;
    let params_str = &line[paren_open + 1..close];

    let before_paren = &line[..paren_open].trim();
    if before_paren.is_empty() {
        return None;
    }

    let tokens: Vec<&str> = before_paren.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let name = tokens.last().unwrap().to_string();
    let return_type = tokens[..tokens.len() - 1].join(" ");
    if return_type.is_empty() {
        return None;
    }

    Some((return_type, name, params_str.to_string()))
}

/// Remove preprocessor directives, comments, and backslash continuations.
fn preprocess_header(src: &str) -> Result<String, String> {
    // Join backslash‑continued lines
    let mut merged = String::new();
    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if line.ends_with('\\') {
            merged.push_str(&line[..line.len() - 1]);
            merged.push(' ');
        } else {
            merged.push_str(line);
            merged.push('\n');
        }
    }

    // Remove block comments /* ... */
    let mut no_block_comments = String::new();
    let chars = merged.chars().collect::<Vec<_>>();
    let mut i = 0;
    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else {
            no_block_comments.push(chars[i]);
            i += 1;
        }
    }

    // Remove preprocessor lines and // comments
    let mut result = String::new();
    for line in no_block_comments.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        let line = line.split("//").next().unwrap_or("").trim_end();
        if !line.is_empty() {
            result.push_str(line);
            result.push('\n');
        }
    }
    Ok(result)
}

/// Parse parameter list like "int a, const char* name, void"
fn parse_params(params: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let mut types = Vec::new();
    let mut names = Vec::new();

    let params = params.trim();
    if params.is_empty() || params == "void" {
        return Ok((types, names));
    }

    for part in split_params(params) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "..." {
            types.push("...".to_string());
            names.push("__va_args__".to_string());
            continue;
        }

        let tokens: Vec<&str> = part.split_whitespace().collect();
        if tokens.is_empty() {
            return Err(format!("Empty parameter in: {}", params));
        }
        let last = tokens[tokens.len() - 1];
        let is_name = !matches!(
            last,
            "const"
                | "volatile"
                | "restrict"
                | "struct"
                | "enum"
                | "union"
                | "unsigned"
                | "signed"
                | "int"
                | "char"
                | "short"
                | "long"
                | "float"
                | "double"
                | "void"
        );
        if tokens.len() == 1 && !is_name {
            let ty = tokens.join(" ");
            types.push(ty);
            names.push(format!("arg{}", types.len()));
        } else if is_name {
            let ty = tokens[0..tokens.len() - 1].join(" ");
            let name = last.to_string();
            types.push(ty);
            names.push(name);
        } else {
            let ty = tokens.join(" ");
            types.push(ty);
            names.push(format!("arg{}", types.len()));
        }
    }
    Ok((types, names))
}

/// Split by commas, ignoring commas inside parentheses.
fn split_params(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut paren_depth = 0;
    for ch in s.chars() {
        match ch {
            '(' => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' => {
                paren_depth -= 1;
                current.push(ch);
            }
            ',' if paren_depth == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let header = "int add(int a, int b);";
        let funcs = parse_header(header).unwrap();
        assert_eq!(funcs.len(), 1);
        let f = &funcs[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.return_type, "int");
        assert_eq!(f.param_types, vec!["int", "int"]);
        assert_eq!(f.param_names, vec!["a", "b"]);
    }

    #[test]
    fn test_parse_void_params() {
        let header = "void foo(void);";
        let funcs = parse_header(header).unwrap();
        assert_eq!(funcs[0].param_types.len(), 0);
    }

    #[test]
    fn test_parse_const_ptr() {
        let header = "const char* get_name(void);";
        let funcs = parse_header(header).unwrap();
        assert_eq!(funcs[0].return_type, "const char*");
    }

    #[test]
    fn test_parse_multi_word_type() {
        let header = "unsigned long long foo(unsigned int x);";
        let funcs = parse_header(header).unwrap();
        assert_eq!(funcs[0].return_type, "unsigned long long");
        assert_eq!(funcs[0].param_types[0], "unsigned int");
    }

    #[test]
    fn test_parse_error_hard_fail() {
        let header = "int add(int a, int b);\nvoid bad(int x,);\nint sub(int a);";
        let result = parse_header(header);
        assert!(result.is_err());
        // Should stop at the bad declaration
    }
}
