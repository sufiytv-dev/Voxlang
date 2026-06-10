// semantics/unify.rs
//! Unification table and type variable management for bidirectional type inference.

use crate::diagnostic::{Diagnostic, Suggestion, emit_diagnostic};
use crate::frontend::span::Span;
use crate::semantics::types::Type;

/// An inference variable that can be bound to a type.
#[derive(Debug, Clone)]
struct TypeVar {
    bound: Option<Type>,
}

/// Unification table that stores inference variables and their bindings.
pub struct UnificationTable {
    vars: Vec<TypeVar>,
}

impl UnificationTable {
    /// Create a new empty unification table.
    pub fn new() -> Self {
        Self { vars: Vec::new() }
    }

    /// Create a new inference variable and return it as a `Type`.
    pub fn new_var(&mut self) -> Type {
        let idx = self.vars.len();
        self.vars.push(TypeVar { bound: None });
        Type::InferVar(idx)
    }

    /// Create a fresh inference variable (alias for `new_var`).
    pub fn fresh_infer_var(&mut self) -> Type {
        self.new_var()
    }

    /// Bind an inference variable to a type, performing an occurs check.
    fn bind(&mut self, idx: usize, ty: Type, span: Span) -> bool {
        // Occurs check: prevent ?idx from appearing inside ty
        if self.occurs_check(idx, &ty) {
            emit_diagnostic(
                &Diagnostic::error("Recursive type detected (occurs check failed)")
                    .with_code("VX1003")
                    .with_span(span),
            );
            return false;
        }
        self.vars[idx].bound = Some(ty);
        true
    }

    /// Check whether a type variable appears inside a type (occurs check).
    fn occurs_check(&self, idx: usize, ty: &Type) -> bool {
        match ty {
            Type::InferVar(other) => *other == idx,
            Type::Struct(_, args) | Type::Enum(_, args) => {
                args.iter().any(|a| self.occurs_check(idx, a))
            }
            Type::Reference(_, inner) => self.occurs_check(idx, inner),
            Type::Array(inner, _) => self.occurs_check(idx, inner),
            Type::Tuple(types) => types.iter().any(|t| self.occurs_check(idx, t)),
            _ => false,
        }
    }

    /// Fully resolve a type by following any variable bindings.
    pub fn resolve(&self, ty: &Type) -> Type {
        match ty {
            Type::InferVar(id) => {
                if let Some(bound) = &self.vars[*id].bound {
                    self.resolve(bound)
                } else {
                    ty.clone()
                }
            }
            _ => ty.clone(),
        }
    }

    /// Unify two types, possibly binding inference variables.
    pub fn unify(&mut self, a: &Type, b: &Type, span: Span) -> bool {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (&a, &b) {
            (Type::Concrete(s1), Type::Concrete(s2)) => {
                if s1 == s2 {
                    true
                } else {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Type mismatch: expected `{}`, found `{}`",
                            s1, s2
                        ))
                        .with_code("VX1001")
                        .with_span(span),
                    );
                    false
                }
            }
            // Concrete ↔ Struct (when struct has no arguments)
            (Type::Concrete(s), Type::Struct(name, args)) if *s == *name && args.is_empty() => true,
            (Type::Struct(name, args), Type::Concrete(s)) if *s == *name && args.is_empty() => true,
            // Concrete ↔ Enum (when enum has no arguments)
            (Type::Concrete(s), Type::Enum(name, args)) if *s == *name && args.is_empty() => true,
            (Type::Enum(name, args), Type::Concrete(s)) if *s == *name && args.is_empty() => true,
            (Type::Struct(name1, args1), Type::Struct(name2, args2)) => {
                if name1 != name2 || args1.len() != args2.len() {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Struct mismatch: {} vs {}",
                            a.to_string(),
                            b.to_string()
                        ))
                        .with_code("VX1004")
                        .with_span(span),
                    );
                    return false;
                }
                for (aa, bb) in args1.iter().zip(args2.iter()) {
                    if !self.unify(aa, bb, span) {
                        return false;
                    }
                }
                true
            }
            (Type::Enum(name1, args1), Type::Enum(name2, args2)) => {
                if name1 != name2 || args1.len() != args2.len() {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Enum mismatch: {} vs {}",
                            a.to_string(),
                            b.to_string()
                        ))
                        .with_code("VX1005")
                        .with_span(span),
                    );
                    return false;
                }
                for (aa, bb) in args1.iter().zip(args2.iter()) {
                    if !self.unify(aa, bb, span) {
                        return false;
                    }
                }
                true
            }
            (Type::Reference(mut1, inner1), Type::Reference(mut2, inner2)) => {
                if mut1 != mut2 {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Reference mutability mismatch: {} vs {}",
                            a.to_string(),
                            b.to_string()
                        ))
                        .with_code("VX1006")
                        .with_span(span),
                    );
                    return false;
                }
                self.unify(inner1, inner2, span)
            }
            (Type::Array(elem1, len1), Type::Array(elem2, len2)) => {
                if len1 != len2 {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Array length mismatch: {} vs {}",
                            a.to_string(),
                            b.to_string()
                        ))
                        .with_code("VX1007")
                        .with_span(span),
                    );
                    return false;
                }
                self.unify(elem1, elem2, span)
            }
            // Handle two inference variables – bind the first to the second
            (Type::InferVar(id1), Type::InferVar(id2)) => {
                if id1 == id2 {
                    true
                } else {
                    self.bind(*id1, Type::InferVar(*id2), span)
                }
            }
            (Type::InferVar(id), _) => self.bind(*id, b.clone(), span),
            (_, Type::InferVar(id)) => self.bind(*id, a.clone(), span),
            (Type::GenericParam(_), _) | (_, Type::GenericParam(_)) => {
                if a == b {
                    true
                } else {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Cannot unify generic parameter `{}` with `{}`",
                            a.to_string(),
                            b.to_string()
                        ))
                        .with_code("VX1008")
                        .with_span(span),
                    );
                    false
                }
            }
            _ => {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Cannot unify `{}` and `{}`",
                        a.to_string(),
                        b.to_string()
                    ))
                    .with_code("VX1009")
                    .with_span(span),
                );
                false
            }
        }
    }

    /// After solving, convert a resolved `Type` back to a concrete type string,
    /// or return `None` if still an unbound variable.
    pub fn as_concrete_string(&self, ty: &Type) -> Option<String> {
        let resolved = self.resolve(ty);
        match resolved {
            Type::Concrete(s) => Some(s),
            Type::GenericParam(s) => Some(s),
            Type::InferVar(_) => None,
            Type::Struct(name, args) => {
                let args_str: Vec<String> = args
                    .iter()
                    .filter_map(|a| self.as_concrete_string(a))
                    .collect();
                if args_str.len() == args.len() {
                    Some(format!("{}<{}>", name, args_str.join(",")))
                } else {
                    None
                }
            }
            Type::Enum(name, args) => {
                let args_str: Vec<String> = args
                    .iter()
                    .filter_map(|a| self.as_concrete_string(a))
                    .collect();
                if args_str.len() == args.len() {
                    Some(format!("{}<{}>", name, args_str.join(",")))
                } else {
                    None
                }
            }
            Type::Reference(mut_, inner) => self
                .as_concrete_string(&inner)
                .map(|s| format!("{}{}", if mut_ { "&mut " } else { "& " }, s)),
            Type::Array(elem, len) => self.as_concrete_string(&elem).map(|s| {
                if let Some(n) = len {
                    format!("[{} x {}]", n, s)
                } else {
                    format!("[]{}", s)
                }
            }),
            Type::Tuple(types) => {
                let types_str: Vec<String> = types
                    .iter()
                    .filter_map(|t| self.as_concrete_string(t))
                    .collect();
                if types_str.len() == types.len() {
                    Some(format!("({})", types_str.join(",")))
                } else {
                    None
                }
            }
        }
    }

    /// Report any unbound inference variables in the table.
    pub fn report_unbound(&self, span: Span) -> bool {
        let mut unbound = Vec::new();
        for (idx, var) in self.vars.iter().enumerate() {
            if var.bound.is_none() {
                unbound.push(idx);
            }
        }
        if unbound.is_empty() {
            false
        } else {
            emit_diagnostic(
                &Diagnostic::error(&format!(
                    "Unable to infer type for inference variable(s) {:?}",
                    unbound
                ))
                .with_code("VX1002")
                .with_span(span)
                .with_suggestion(Suggestion {
                    message: "Add a type annotation to help the compiler infer the type."
                        .to_string(),
                    span: Some(span),
                }),
            );
            true
        }
    }
}
