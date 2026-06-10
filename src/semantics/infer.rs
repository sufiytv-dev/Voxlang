// semantics/infer.rs
//! Type inference helpers: fresh variables, type resolution, and type parsing with imports.

use crate::diagnostic::{Diagnostic, emit_diagnostic};
use crate::frontend::span::Span;
use crate::semantics::SemanticAnalyzer; // to be defined in mod.rs
use crate::semantics::types::{Type, parse_type_str};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};

impl SemanticAnalyzer<'_> {
    /// Create a fresh inference variable.
    pub fn fresh_infer_var(&mut self) -> Type {
        self.unify.new_var()
    }

    /// Generate a fresh temporary variable name for desugaring.
    pub fn fresh_tmp_var(&self) -> String {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("__vox_tmp_{}", id)
    }

    /// Parse a type string, resolving imported type names via `use_imports`.
    pub fn parse_type_str_with_imports(&self, s: &str, generic_params: &HashSet<String>) -> Type {
        let s = s.trim();
        // Plain identifier that might be an imported type
        if s.chars().all(|c| c.is_alphabetic() || c == '_') && !generic_params.contains(s) {
            if let Some(qualified) = self.use_imports.get(s) {
                // Recurse with the qualified name
                return self.parse_type_str_with_imports(qualified, generic_params);
            }
        }
        parse_type_str(s, generic_params)
    }

    /// Resolve a type to its proper Struct/Enum variant and handle type aliases.
    pub fn resolve_type(&mut self, ty: Type, span: Span) -> Type {
        match ty {
            Type::Concrete(ref name) => {
                if self.symbols.structs.contains_key(name.as_str()) {
                    Type::Struct(name.clone(), vec![])
                } else if self.symbols.enums.contains_key(name.as_str()) {
                    Type::Enum(name.clone(), vec![])
                } else {
                    // Check type aliases
                    if let Some(target) = self.symbols.lookup_type_alias(name) {
                        let empty_set = HashSet::new();
                        let parsed = self.parse_type_str_with_imports(target, &empty_set);
                        return self.resolve_type(parsed, span);
                    }
                    ty
                }
            }
            Type::Struct(name, args) => {
                let resolved_args: Vec<Type> = args
                    .into_iter()
                    .map(|a| self.resolve_type(a, span))
                    .collect();
                if self.symbols.enums.contains_key(&name) {
                    Type::Enum(name, resolved_args)
                } else if self.symbols.structs.contains_key(&name) {
                    Type::Struct(name, resolved_args)
                } else {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Unknown type '{}'", name))
                            .with_code("VX0273")
                            .with_span(span),
                    );
                    self.error_occurred = true;
                    Type::Concrete(name)
                }
            }
            Type::Enum(name, args) => {
                let resolved_args: Vec<Type> = args
                    .into_iter()
                    .map(|a| self.resolve_type(a, span))
                    .collect();
                if self.symbols.structs.contains_key(&name) {
                    Type::Struct(name, resolved_args)
                } else if self.symbols.enums.contains_key(&name) {
                    Type::Enum(name, resolved_args)
                } else {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Unknown type '{}'", name))
                            .with_code("VX0273")
                            .with_span(span),
                    );
                    self.error_occurred = true;
                    Type::Concrete(name)
                }
            }
            Type::Reference(mut_, inner) => {
                let resolved_inner = self.resolve_type(*inner, span);
                Type::Reference(mut_, Box::new(resolved_inner))
            }
            Type::Array(elem, len) => {
                let resolved_elem = self.resolve_type(*elem, span);
                Type::Array(Box::new(resolved_elem), len)
            }
            _ => ty,
        }
    }

    /// Extract the element type of an array type.
    pub fn extract_array_element_type(ty: &Type) -> Option<Type> {
        match ty {
            Type::Array(elem, _) => Some(elem.as_ref().clone()),
            _ => None,
        }
    }

    /// Check compatibility of declared and initializer array types.
    pub fn is_array_compatible(&mut self, decl_ty: &Type, init_ty: &Type) -> bool {
        match (decl_ty, init_ty) {
            (Type::Array(elem1, len1), Type::Array(elem2, len2)) => {
                len1 == len2 && self.unify.unify(elem1, elem2, Span::dummy())
            }
            _ => false,
        }
    }

    /// Strip outer references from a type (uses the method from `Type`).
    pub fn strip_references(ty: &Type) -> &Type {
        ty.strip_references()
    }
}
