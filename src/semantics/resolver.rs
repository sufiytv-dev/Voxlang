// semantics/resolver.rs
//! Name resolution for `use` statements and import processing.

use crate::diagnostic::{Diagnostic, emit_diagnostic};
use crate::frontend::span::Span;
use crate::parser::ASTNode;
use crate::semantics::SemanticAnalyzer;
use std::collections::HashSet;
use std::path::Path;

impl SemanticAnalyzer<'_> {
    // -------------------------------------------------------------------------
    // Import processing – collects the AST for code generation
    // -------------------------------------------------------------------------
    pub(crate) fn process_import(&mut self, import: &ASTNode, span: Span) -> bool {
        let import_node = match import {
            ASTNode::Import {
                source,
                alias,
                hash,
                span: _,
            } => (source, alias, hash),
            _ => {
                emit_diagnostic(
                    &Diagnostic::error("Expected import node")
                        .with_code("VX0310")
                        .with_span(span),
                );
                return false;
            }
        };

        let resolver = match self.module_resolver.as_mut() {
            Some(r) => r,
            None => {
                emit_diagnostic(
                    &Diagnostic::error("Module resolver not available for import")
                        .with_code("VX0311")
                        .with_span(span),
                );
                return false;
            }
        };

        let ast = match resolver.resolve_import(import, span) {
            Some(ast) => ast,
            None => {
                emit_diagnostic(
                    &Diagnostic::error(&format!("Failed to resolve import: {:?}", import_node.0))
                        .with_code("VX9004")
                        .with_span(span),
                );
                return false;
            }
        };

        let canonical = match resolver.canonicalize_source(import_node.0, span) {
            Some(p) => p,
            None => return false,
        };

        let import_path = Path::new(&canonical);
        if self.processing_paths.iter().any(|p| p == import_path) {
            emit_diagnostic(
                &Diagnostic::error(&format!(
                    "Circular import detected: {}",
                    import_path.display()
                ))
                .with_code("VX0312")
                .with_span(span),
            );
            return false;
        }
        self.processing_paths.push(import_path.to_path_buf());

        let module_symbols = match resolver.get_module_symbols(&canonical) {
            Some(sym) => sym.clone(),
            None => {
                let syms = crate::module::extract_module_symbols(&ast);
                self.processing_paths.pop();
                emit_diagnostic(
                    &Diagnostic::warning(&format!("Module symbols not cached for {}", canonical))
                        .with_code("VX9005")
                        .with_span(span),
                );
                syms
            }
        };

        let alias = if let Some(alias_str) = import_node.1 {
            alias_str.clone()
        } else {
            let path = Path::new(&canonical);
            path.file_stem()
                .unwrap_or_else(|| std::ffi::OsStr::new("unknown"))
                .to_string_lossy()
                .to_string()
        };

        if self.debug {
            crate::diagnostic::debug_log(format!(
                "[SEM] Imported module '{}' with functions: {:?}",
                alias,
                module_symbols.functions.keys().collect::<Vec<_>>()
            ));
        }

        if !self.symbols.register_module(alias.clone(), module_symbols) {
            self.processing_paths.pop();
            return false;
        }

        // Store the module AST for later code generation
        self.imported_modules.push((alias, ast));

        self.processing_paths.pop();
        true
    }

    // -------------------------------------------------------------------------
    // `use` statement resolution
    // -------------------------------------------------------------------------
    pub(crate) fn resolve_use_decl(&mut self, node: &ASTNode, span: Span) -> bool {
        let (path, alias, is_glob) = match node {
            ASTNode::UseDecl {
                path,
                alias,
                is_glob,
                span: _,
            } => (path, alias, is_glob),
            _ => {
                emit_diagnostic(&Diagnostic::error("Expected use declaration").with_code("VX0316"));
                return false;
            }
        };

        if path.len() < 2 {
            emit_diagnostic(
                &Diagnostic::error(&format!(
                    "Invalid use path: expected `module::item`, got `{}`",
                    path.join("::")
                ))
                .with_code("VX0317")
                .with_span(span),
            );
            return false;
        }

        let module_segments = &path[..path.len() - 1];
        let item_segment = &path[path.len() - 1];
        let module_alias = module_segments.join("::");

        if !self.symbols.modules.contains_key(&module_alias) {
            emit_diagnostic(
                &Diagnostic::error(&format!("Module '{}' not found in use path", module_alias))
                    .with_code("VX0306")
                    .with_span(span),
            );
            return false;
        }

        // Find the imported module's AST
        let module_ast = match self
            .imported_modules
            .iter()
            .find(|(name, _)| name == &module_alias)
            .map(|(_, ast)| ast)
        {
            Some(ast) => ast,
            None => {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Module '{}' resolved but AST not found",
                        module_alias
                    ))
                    .with_code("VX0318")
                    .with_span(span),
                );
                return false;
            }
        };

        // Helper to collect top‑level item names from a module AST
        fn collect_item_names(ast: &ASTNode) -> HashSet<String> {
            let mut names = HashSet::new();
            if let ASTNode::Program(stmts, _) = ast {
                for stmt in stmts {
                    match stmt {
                        ASTNode::FunctionDef { name, .. } => {
                            names.insert(name.clone());
                        }
                        ASTNode::StructDef { name, .. } => {
                            names.insert(name.clone());
                        }
                        ASTNode::EnumDef { name, .. } => {
                            names.insert(name.clone());
                        }
                        ASTNode::TypeAlias { name, .. } => {
                            names.insert(name.clone());
                        }
                        _ => {}
                    }
                }
            }
            names
        }

        if *is_glob {
            // Glob import: bring all top‑level items into scope
            let item_names = collect_item_names(module_ast);
            for item in item_names {
                let qualified = format!("{}::{}", module_alias, item);
                if self.use_imports.contains_key(&item) {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Name '{}' already imported (conflict with previous use)",
                            item
                        ))
                        .with_code("VX0308")
                        .with_span(span),
                    );
                    self.error_occurred = true;
                    continue;
                }
                if self.symbols.lookup_info(&item).is_some() {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Name '{}' already defined locally, cannot import from glob use",
                            item
                        ))
                        .with_code("VX0308")
                        .with_span(span),
                    );
                    self.error_occurred = true;
                    continue;
                }
                self.use_imports.insert(item, qualified);
            }
        } else {
            // Single item import
            let local_name = alias.as_ref().unwrap_or(item_segment).clone();
            let qualified = format!("{}::{}", module_alias, item_segment);

            // Check if the item actually exists in the module
            let item_names = collect_item_names(module_ast);
            if !item_names.contains(item_segment) {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Item '{}' not found in module '{}'",
                        item_segment, module_alias
                    ))
                    .with_code("VX0307")
                    .with_span(span),
                );
                return false;
            }

            // Conflict detection
            if self.use_imports.contains_key(&local_name) {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Name '{}' already imported (conflict with previous use)",
                        local_name
                    ))
                    .with_code("VX0308")
                    .with_span(span),
                );
                return false;
            }
            if self.symbols.lookup_info(&local_name).is_some() {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Name '{}' already defined locally, cannot import",
                        local_name
                    ))
                    .with_code("VX0308")
                    .with_span(span),
                );
                return false;
            }

            self.use_imports.insert(local_name, qualified);
        }

        true
    }
}
