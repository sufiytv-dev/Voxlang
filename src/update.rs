// update.rs - `vox update` command: refresh remote import hashes.

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic, emit_phase_update};
use crate::frontend::lexer::Lexer;
use crate::frontend::span::Span;
use crate::module::{compute_hash, read_source_file};
use crate::parser::{ASTNode, HashValue, ImportSource, Parser};
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct RemoteImportInfo {
    pub url: String,
    pub expected_hash: HashValue,
    pub hash_span: Span,
}

/// Collect all remote imports from an AST, recursively visiting all nodes.
pub fn collect_remote_imports(ast: &ASTNode) -> Vec<RemoteImportInfo> {
    let mut imports = Vec::new();
    collect_imports_recursive(ast, &mut imports);
    imports
}

fn collect_imports_recursive(node: &ASTNode, out: &mut Vec<RemoteImportInfo>) {
    match node {
        ASTNode::Program(stmts, _) => {
            for stmt in stmts {
                collect_imports_recursive(stmt, out);
            }
        }
        ASTNode::Import { source, hash, .. } => {
            if let ImportSource::RemoteUrl(url) = source {
                if let Some(hash) = hash {
                    out.push(RemoteImportInfo {
                        url: url.clone(),
                        expected_hash: hash.clone(),
                        hash_span: hash.span,
                    });
                }
            }
        }
        ASTNode::FunctionDef { body, .. } => {
            for stmt in body {
                collect_imports_recursive(stmt, out);
            }
        }
        ASTNode::KernelFn { body, .. } => {
            for stmt in body {
                collect_imports_recursive(stmt, out);
            }
        }
        ASTNode::IfStatement {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            collect_imports_recursive(condition, out);
            for stmt in then_branch {
                collect_imports_recursive(stmt, out);
            }
            if let Some(b) = else_branch {
                for stmt in b {
                    collect_imports_recursive(stmt, out);
                }
            }
        }
        ASTNode::WhileStatement {
            condition, body, ..
        } => {
            collect_imports_recursive(condition, out);
            for stmt in body {
                collect_imports_recursive(stmt, out);
            }
        }
        ASTNode::ParallelLoop {
            start, end, body, ..
        } => {
            collect_imports_recursive(start, out);
            collect_imports_recursive(end, out);
            for stmt in body {
                collect_imports_recursive(stmt, out);
            }
        }
        ASTNode::ComptimeBlock { body, .. } => {
            for stmt in body {
                collect_imports_recursive(stmt, out);
            }
        }
        ASTNode::Lemma { proof, .. } => {
            for stmt in proof {
                collect_imports_recursive(stmt, out);
            }
        }
        ASTNode::VariableDecl { value, .. } | ASTNode::DeviceVarDecl { value, .. } => {
            collect_imports_recursive(value, out);
        }
        ASTNode::Assignment { lhs, value, .. } => {
            collect_imports_recursive(lhs, out);
            collect_imports_recursive(value, out);
        }
        ASTNode::ReturnStatement(Some(expr), _) => {
            collect_imports_recursive(expr, out);
        }
        ASTNode::BinaryExpr { left, right, .. } => {
            collect_imports_recursive(left, out);
            collect_imports_recursive(right, out);
        }
        ASTNode::CastExpr { expr, .. } => {
            collect_imports_recursive(expr, out);
        }
        ASTNode::CallExpr { args, .. } => {
            for arg in args {
                collect_imports_recursive(arg, out);
            }
        }
        ASTNode::BorrowExpr { expr, .. } => {
            collect_imports_recursive(expr, out);
        }
        ASTNode::DerefExpr(expr, _) => {
            collect_imports_recursive(expr, out);
        }
        ASTNode::RefinedType { condition, .. } => {
            collect_imports_recursive(condition, out);
        }
        _ => {}
    }
}

/// Download a URL using curl (fallback to wget), returning the content as a string.
fn download_with_curl_or_wget(url: &str) -> Result<String, String> {
    debug_log(format!("Downloading {} via curl", url));
    let curl_status = Command::new("curl")
        .args(["-L", "-s", "-S", "--fail", url])
        .output();

    if let Ok(output) = curl_status {
        if output.status.success() {
            return String::from_utf8(output.stdout)
                .map_err(|e| format!("Invalid UTF-8 in downloaded content: {}", e));
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "curl failed with exit code {:?}: {}",
                output.status.code(),
                stderr
            ));
        }
    }

    debug_log("curl failed, trying wget");
    let wget_status = Command::new("wget").args(["-q", "-O-", url]).output();

    match wget_status {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .map_err(|e| format!("Invalid UTF-8 in downloaded content: {}", e)),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "wget failed with exit code {:?}: {}",
                output.status.code(),
                stderr
            ))
        }
        Err(e) => Err(format!(
            "Neither curl nor wget is available or both failed to download '{}'. \
             Please install curl or wget. (Error: {})",
            url, e
        )),
    }
}

/// Download a module and verify its hash. Returns the new hash string (same as expected if unchanged).
pub fn download_and_verify(url: &str, expected_hash: &HashValue) -> Result<String, String> {
    let content = download_with_curl_or_wget(url)?;
    let actual = compute_hash(&content, &expected_hash.algorithm);
    Ok(actual)
}

/// Replace the hash literal in the source code at the given span.
/// The span should cover only the digest characters (not the quotes).
pub fn replace_hash_in_source(source: &str, new_hash: &str, span: Span) -> String {
    let start = span.start;
    let end = span.end;
    let mut result = String::new();
    result.push_str(&source[0..start]);
    result.push_str(new_hash);
    result.push_str(&source[end..]);
    result
}

/// Process a single .vx file: find remote imports, download, and optionally update.
/// Returns true if any file was updated (or would be updated without --write).
pub fn process_file(path: &Path, write: bool) -> Result<bool, String> {
    emit_phase_update("Updating imports", 0);
    let source =
        read_source_file(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let mut lexer = Lexer::new(&source);
    let tokens = lexer
        .tokenize()
        .map_err(|_| format!("Lexing failed in {}", path.display()))?;
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse();
    if matches!(ast, ASTNode::Error) {
        return Err(format!("Parsing failed in {}", path.display()));
    }

    let imports = collect_remote_imports(&ast);
    if imports.is_empty() {
        debug_log(format!("No remote imports in {}", path.display()));
        return Ok(false);
    }

    let mut updated = false;
    let mut new_source = source;
    let total = imports.len();
    for (idx, import) in imports.iter().enumerate() {
        let percent = ((idx + 1) * 100) / total;
        emit_phase_update("Updating imports", percent);

        match download_and_verify(&import.url, &import.expected_hash) {
            Ok(actual_hash) => {
                if actual_hash != import.expected_hash.digest {
                    let msg = format!(
                        "Hash mismatch for {}: expected {}, actual {}",
                        import.url, import.expected_hash.digest, actual_hash
                    );
                    if write {
                        new_source =
                            replace_hash_in_source(&new_source, &actual_hash, import.hash_span);
                        emit_diagnostic(
                            &Diagnostic::note(format!(
                                "Updated hash in {}: {}",
                                path.display(),
                                import.url
                            ))
                            .with_code("VX0820")
                            .with_span(import.hash_span),
                        );
                        updated = true;
                    } else {
                        emit_diagnostic(
                            &Diagnostic::warning(msg)
                                .with_code("VX0821")
                                .with_span(import.hash_span),
                        );
                        emit_diagnostic(
                            &Diagnostic::help("Use --write to update the hash in the source file.")
                                .with_code("VX0822")
                                .with_span(import.hash_span),
                        );
                    }
                } else {
                    debug_log(format!("Up-to-date: {}", import.url));
                }
            }
            Err(e) => {
                emit_diagnostic(
                    &Diagnostic::error(format!("Failed to check {}: {}", import.url, e))
                        .with_code("VX0823")
                        .with_span(import.hash_span),
                );
            }
        }
    }

    if write && updated {
        fs::write(path, &new_source)
            .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
        emit_diagnostic(
            &Diagnostic::note(format!(
                "Updated {} hash(es) in {}",
                updated,
                path.display()
            ))
            .with_code("VX0824"),
        );
    }
    emit_phase_update("Update complete", 100);
    Ok(updated)
}
