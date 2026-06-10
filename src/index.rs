// index.rs - Symbol indexing for LSP and `vox index`.

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};
use crate::frontend::lexer::Lexer;
use crate::frontend::span::Span;
use crate::module::read_source_file;
use crate::parser::{ASTNode, ImportSource, Parser};
use crate::std::fs;
use crate::std::path::{Path, PathBuf};

// -----------------------------------------------------------------------------
// Data structures for the index
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum SymbolKind {
    Function,
    Variable,
    Kernel,
    Import,
    Lemma,
    DeviceVar,
    Struct,
}

#[derive(Debug, Clone)]
pub struct SymbolSpan {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}

impl From<Span> for SymbolSpan {
    fn from(span: Span) -> Self {
        SymbolSpan {
            start: span.start,
            end: span.end,
            line: span.line,
            col: span.col,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    pub span: SymbolSpan,
    pub signature: Option<String>,
    pub ty: Option<String>,
    pub mutable: Option<bool>,
    pub refinement: Option<String>,
    pub params: Option<Vec<ParamInfo>>,
    pub return_type: Option<String>,
    pub device_triple: Option<String>,
    pub import_source: Option<String>,
    pub alias: Option<String>,
    pub hash: Option<String>,
    pub fields: Option<Vec<StructFieldInfo>>,
}

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: String,
    pub ty: String,
    pub refinement: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StructFieldInfo {
    pub name: String,
    pub ty: String,
}

#[derive(Debug)]
pub struct FileIndex {
    pub path: String,
    pub symbols: Vec<Symbol>,
}

#[derive(Debug)]
pub struct ProjectIndex {
    pub version: u32,
    pub project_root: String,
    pub files: Vec<FileIndex>,
}

// -----------------------------------------------------------------------------
// Helper: format refinement node to a string (for indexing)
// -----------------------------------------------------------------------------
fn refinement_to_string(refinement: &Option<Box<ASTNode>>) -> Option<String> {
    refinement.as_ref().map(|r| format!("{:?}", r))
}

// -----------------------------------------------------------------------------
// Extract symbols from a single file
// -----------------------------------------------------------------------------
pub fn extract_symbols_from_file(path: &Path, source: &str) -> Result<Vec<Symbol>, String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer
        .tokenize()
        .map_err(|_| format!("Lexing failed in {}", path.display()))?;
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse();
    if matches!(ast, ASTNode::Error) {
        return Err(format!("Parsing failed in {}", path.display()));
    }
    Ok(extract_symbols_from_ast(&ast))
}

fn extract_symbols_from_ast(node: &ASTNode) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    extract_symbols_recursive(node, &mut symbols);
    symbols
}

fn extract_symbols_recursive(node: &ASTNode, out: &mut Vec<Symbol>) {
    match node {
        ASTNode::Program(stmts, _) => {
            for stmt in stmts {
                extract_symbols_recursive(stmt, out);
            }
        }

        ASTNode::StructDef {
            name,
            fields,
            span,
            generic_params: _,
        } => {
            let field_infos: Vec<StructFieldInfo> = fields
                .iter()
                .map(|f| StructFieldInfo {
                    name: f.name.clone(),
                    ty: f.ty.clone(),
                })
                .collect();
            out.push(Symbol {
                kind: SymbolKind::Struct,
                name: name.clone(),
                span: (*span).into(),
                signature: Some(format!("struct {}", name)),
                ty: None,
                mutable: None,
                refinement: None,
                params: None,
                return_type: None,
                device_triple: None,
                import_source: None,
                alias: None,
                hash: None,
                fields: Some(field_infos),
            });
        }

        ASTNode::FieldAccess { expr, .. } => {
            extract_symbols_recursive(expr, out);
        }

        ASTNode::ArrayLiteral { elements, .. } => {
            for elem in elements {
                extract_symbols_recursive(elem, out);
            }
        }

        ASTNode::ArrayIndex { array, index, .. } => {
            extract_symbols_recursive(array, out);
            extract_symbols_recursive(index, out);
        }

        ASTNode::UnaryExpr { expr, .. } => {
            extract_symbols_recursive(expr, out);
        }

        ASTNode::FloatLiteral(_, _) | ASTNode::CharLiteral(_, _) => {}

        ASTNode::FunctionDef {
            name,
            params,
            return_type,
            return_refinement,
            body,
            span,
            ..
        } => {
            let param_infos: Vec<ParamInfo> = params
                .iter()
                .map(|p| ParamInfo {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    refinement: refinement_to_string(&p.refinement),
                })
                .collect();
            out.push(Symbol {
                kind: SymbolKind::Function,
                name: name.clone(),
                span: (*span).into(),
                signature: Some(format!("fn {}({:?}) -> {}", name, param_infos, return_type)),
                ty: None,
                mutable: None,
                refinement: refinement_to_string(return_refinement),
                params: Some(param_infos),
                return_type: Some(return_type.clone()),
                device_triple: None,
                import_source: None,
                alias: None,
                hash: None,
                fields: None,
            });
            for stmt in body {
                extract_symbols_recursive(stmt, out);
            }
        }

        ASTNode::KernelFn {
            name,
            params,
            device_triple,
            body,
            span,
            ..
        } => {
            let param_infos: Vec<ParamInfo> = params
                .iter()
                .map(|p| ParamInfo {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    refinement: refinement_to_string(&p.refinement),
                })
                .collect();
            out.push(Symbol {
                kind: SymbolKind::Kernel,
                name: name.clone(),
                span: (*span).into(),
                signature: Some(format!("@kernel fn {}({:?}) -> void", name, param_infos)),
                ty: None,
                mutable: None,
                refinement: None,
                params: Some(param_infos),
                return_type: Some("void".to_string()),
                device_triple: Some(device_triple.clone()),
                import_source: None,
                alias: None,
                hash: None,
                fields: None,
            });
            for stmt in body {
                extract_symbols_recursive(stmt, out);
            }
        }

        ASTNode::VariableDecl {
            name,
            ty,
            refinement,
            mutable,
            span,
            ..
        } => {
            out.push(Symbol {
                kind: SymbolKind::Variable,
                name: name.clone(),
                span: (*span).into(),
                signature: None,
                ty: ty.clone(),
                mutable: Some(*mutable),
                refinement: refinement_to_string(refinement),
                params: None,
                return_type: None,
                device_triple: None,
                import_source: None,
                alias: None,
                hash: None,
                fields: None,
            });
        }

        ASTNode::DeviceVarDecl {
            name,
            ty,
            refinement,
            span,
            ..
        } => {
            out.push(Symbol {
                kind: SymbolKind::DeviceVar,
                name: name.clone(),
                span: (*span).into(),
                signature: None,
                ty: ty.clone(),
                mutable: Some(false),
                refinement: refinement_to_string(refinement),
                params: None,
                return_type: None,
                device_triple: None,
                import_source: None,
                alias: None,
                hash: None,
                fields: None,
            });
        }

        ASTNode::Import {
            source,
            alias,
            hash,
            span,
        } => {
            let source_str = match source {
                ImportSource::LocalPath(p) => p.clone(),
                ImportSource::RemoteUrl(u) => u.clone(),
            };
            let hash_str = hash
                .as_ref()
                .map(|h| format!("{:?}:{}", h.algorithm, h.digest));
            out.push(Symbol {
                kind: SymbolKind::Import,
                name: alias.clone().unwrap_or_else(|| source_str.clone()),
                span: (*span).into(),
                signature: None,
                ty: None,
                mutable: None,
                refinement: None,
                params: None,
                return_type: None,
                device_triple: None,
                import_source: Some(source_str),
                alias: alias.clone(),
                hash: hash_str,
                fields: None,
            });
        }

        ASTNode::Lemma {
            name,
            params,
            return_type,
            span,
            ..
        } => {
            let param_infos: Vec<ParamInfo> = params
                .iter()
                .map(|p| ParamInfo {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    refinement: refinement_to_string(&p.refinement),
                })
                .collect();
            out.push(Symbol {
                kind: SymbolKind::Lemma,
                name: name.clone(),
                span: (*span).into(),
                signature: Some(format!(
                    "@lemma fn {}({:?}) -> {}",
                    name, param_infos, return_type
                )),
                ty: None,
                mutable: None,
                refinement: None,
                params: Some(param_infos),
                return_type: Some(return_type.clone()),
                device_triple: None,
                import_source: None,
                alias: None,
                hash: None,
                fields: None,
            });
        }

        ASTNode::IfStatement {
            then_branch,
            else_branch,
            ..
        } => {
            for stmt in then_branch {
                extract_symbols_recursive(stmt, out);
            }
            if let Some(b) = else_branch {
                for stmt in b {
                    extract_symbols_recursive(stmt, out);
                }
            }
        }

        ASTNode::WhileStatement { body, .. } | ASTNode::ParallelLoop { body, .. } => {
            for stmt in body {
                extract_symbols_recursive(stmt, out);
            }
        }

        ASTNode::ComptimeBlock { body, .. } => {
            for stmt in body {
                extract_symbols_recursive(stmt, out);
            }
        }

        _ => {}
    }
}

// -----------------------------------------------------------------------------
// Index cache directory
// -----------------------------------------------------------------------------
pub fn cache_dir() -> PathBuf {
    crate::std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".vox/index")
}

// -----------------------------------------------------------------------------
// Index writing (placeholder - disabled to avoid serde dependency)
// -----------------------------------------------------------------------------
pub fn write_index(project_root: &Path, file_indices: Vec<FileIndex>) -> Result<(), String> {
    debug_log(format!(
        "Would write index for {} files in {}",
        file_indices.len(),
        project_root.display()
    ));
    Ok(())
}

// -----------------------------------------------------------------------------
// Public API for main.rs
// -----------------------------------------------------------------------------
pub fn index_project(root: &Path, watch: bool) -> Result<(), String> {
    if watch {
        emit_diagnostic(
            &Diagnostic::error("Watch mode not yet implemented in indexer").with_code("VX0901"),
        );
        return Err("Watch mode not implemented".to_string());
    }

    debug_log(format!("Indexing project at {}", root.display()));
    let mut files: Vec<PathBuf> = Vec::new();
    crate::std::walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("vx"))
        .for_each(|e| {
            files.push(e.path().to_path_buf());
        });

    if files.is_empty() {
        let err_msg = format!("No .vx files found in {}", root.display());
        emit_diagnostic(&Diagnostic::error(&err_msg).with_code("VX0902"));
        return Err(err_msg);
    }

    let mut file_indices = Vec::new();
    for file in files {
        debug_log(format!("Indexing file: {}", file.display()));
        let source = read_source_file(&file)
            .map_err(|e| format!("Failed to read {}: {}", file.display(), e))?;
        let symbols = extract_symbols_from_file(&file, &source)?;
        let rel_path = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .to_string();
        file_indices.push(FileIndex {
            path: rel_path,
            symbols,
        });
    }

    write_index(root, file_indices)?;
    emit_diagnostic(
        &Diagnostic::note(format!("Index written to {}", cache_dir().display()))
            .with_code("VX0903"),
    );
    Ok(())
}
