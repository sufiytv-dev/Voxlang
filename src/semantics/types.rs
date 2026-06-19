// semantics/types.rs
//! Definition of the core `Type` enum and its fundamental operations.
//! This module is independent and has no dependencies on other semantic modules.

use std::collections::{HashMap, HashSet};

/// Structured type representation used throughout semantic analysis and type inference.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// Primitive or built‑in type (i32, f64, char, bool, void, String, &str)
    Concrete(String),
    /// A generic parameter name (e.g., "T" in struct Vec<T>)
    GenericParam(String),
    /// An inference variable created during bidirectional inference
    InferVar(usize),
    /// Named struct with type arguments, e.g., Vec<i32>
    Struct(String, Vec<Type>),
    /// Named enum with type arguments, e.g., Option<i32>
    Enum(String, Vec<Type>),
    /// Reference: (mutable, pointee type)
    Reference(bool, Box<Type>),
    /// Array: (element type, optional length). None length means dynamic array ([]T)
    Array(Box<Type>, Option<usize>),
    /// Tuple (currently not used but present for completeness)
    Tuple(Vec<Type>),
}

impl Type {
    /// Convert the type to a human‑readable string representation.
    pub fn to_string(&self) -> String {
        match self {
            Type::Concrete(s) => s.clone(),
            Type::GenericParam(s) => s.clone(),
            Type::InferVar(id) => format!("?{}", id),
            Type::Struct(name, args) => {
                if args.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{}<{}>",
                        name,
                        args.iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                }
            }
            Type::Enum(name, args) => {
                if args.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{}<{}>",
                        name,
                        args.iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                }
            }
            Type::Reference(mut_, inner) => {
                format!(
                    "{}{}",
                    if *mut_ { "&mut " } else { "& " },
                    inner.to_string()
                )
            }
            Type::Array(elem, len) => {
                if let Some(n) = len {
                    format!("[{} x {}]", n, elem.to_string())
                } else {
                    format!("[]{}", elem.to_string())
                }
            }
            Type::Tuple(types) => {
                format!(
                    "({})",
                    types
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }

    /// Returns true if the type is a concrete primitive (no generics or inference variables).
    pub fn is_concrete(&self) -> bool {
        matches!(self, Type::Concrete(_))
    }

    /// If this is a `Concrete` type, return its name.
    pub fn as_concrete(&self) -> Option<&str> {
        match self {
            Type::Concrete(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns true if the type is a reference (shared or mutable).
    pub fn is_reference(&self) -> bool {
        matches!(self, Type::Reference(_, _))
    }

    /// Returns true if the type is a mutable reference.
    pub fn is_mutable_reference(&self) -> bool {
        matches!(self, Type::Reference(true, _))
    }

    /// Strip outer references: &T -> T, &mut T -> T.
    pub fn strip_references(&self) -> &Type {
        let mut ty = self;
        while let Type::Reference(_, inner) = ty {
            ty = inner.as_ref();
        }
        ty
    }

    /// Apply a substitution map to this type.
    pub fn substitute(&self, subst: &HashMap<String, Type>) -> Type {
        match self {
            Type::Concrete(_) => self.clone(),
            Type::GenericParam(name) => subst.get(name).cloned().unwrap_or(self.clone()),
            Type::InferVar(_) => self.clone(),
            Type::Struct(name, args) => Type::Struct(
                name.clone(),
                args.iter().map(|a| a.substitute(subst)).collect(),
            ),
            Type::Enum(name, args) => Type::Enum(
                name.clone(),
                args.iter().map(|a| a.substitute(subst)).collect(),
            ),
            Type::Reference(mut_, inner) => {
                Type::Reference(*mut_, Box::new(inner.substitute(subst)))
            }
            Type::Array(elem, len) => Type::Array(Box::new(elem.substitute(subst)), *len),
            Type::Tuple(types) => Type::Tuple(types.iter().map(|t| t.substitute(subst)).collect()),
        }
    }
}

// -----------------------------------------------------------------------------
// Type parsing utilities (moved from original semantics.rs)
// -----------------------------------------------------------------------------

/// Parse a type string into a structured `Type`.
///
/// Handles primitives, references, arrays (static and dynamic), and generic types.
/// Recognizes generic parameters from the provided set.
pub(crate) fn parse_type_str(s: &str, generic_params: &HashSet<String>) -> Type {
    let s = s.trim();

    // Helper to find matching closing bracket for an opening bracket at start of slice
    fn find_matching_bracket(s: &str) -> Option<usize> {
        let mut depth = 0;
        for (i, ch) in s.chars().enumerate() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    // Handle primitive types
    match s {
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64" | "char"
        | "bool" | "void" | "String" | "&str" => return Type::Concrete(s.to_string()),
        _ => {}
    }

    // Reference: & T or &mut T
    if s.starts_with('&') {
        let mut rest = s[1..].trim_start();
        let mut mutable = false;
        if rest.starts_with("mut ") {
            mutable = true;
            rest = rest[4..].trim_start();
        } else if rest.starts_with(' ') {
            rest = rest[1..].trim_start();
        }
        let inner = parse_type_str(rest, generic_params);
        return Type::Reference(mutable, Box::new(inner));
    }

    // Array: []T or [N x T] (with proper bracket matching)
    if s.starts_with('[') {
        if let Some(close) = find_matching_bracket(s) {
            let inside = &s[1..close];
            let rest = s[close + 1..].trim_start();
            // Check if there is an 'x' inside the brackets (fixed-size array)
            if let Some(x_pos) = inside.find('x') {
                // Fixed-size: [N x T]
                let len_str = inside[..x_pos].trim();
                let elem_str = inside[x_pos + 1..].trim();
                let elem = parse_type_str(elem_str, generic_params);
                if let Ok(n) = len_str.parse::<usize>() {
                    return Type::Array(Box::new(elem), Some(n));
                } else if len_str == "?" {
                    return Type::Array(Box::new(elem), None);
                } else {
                    // Fallback: treat as dynamic array
                    return Type::Array(Box::new(elem), None);
                }
            } else if inside.is_empty() {
                // Dynamic array: []T
                let elem = parse_type_str(rest, generic_params);
                return Type::Array(Box::new(elem), None);
            } else {
                // Could be something else? Fallback: treat whole thing as concrete
                // but this shouldn't happen for valid types.
            }
        }
    }

    // Generic type: Name<Args>
    if let Some(angle_start) = s.find('<') {
        let name = &s[..angle_start];
        let args_str = &s[angle_start + 1..s.len() - 1];
        let args: Vec<Type> = split_type_args(args_str)
            .into_iter()
            .map(|a| parse_type_str(&a, generic_params))
            .collect();
        // Return as Struct (will be resolved later)
        return Type::Struct(name.to_string(), args);
    }

    // Plain identifier
    if s.chars().all(|c| c.is_alphabetic() || c == '_') {
        if generic_params.contains(s) {
            return Type::GenericParam(s.to_string());
        } else {
            return Type::Concrete(s.to_string());
        }
    }

    Type::Concrete(s.to_string())
}

/// Helper to split type arguments at top level (ignoring nested angle brackets).
pub(crate) fn split_type_args(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    let chars: Vec<char> = s.chars().collect();
    for i in 0..chars.len() {
        match chars[i] {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                result.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        result.push(s[start..].trim().to_string());
    }
    result
}

// -----------------------------------------------------------------------------
// NEW: Parse generic type string into base name and arguments
// -----------------------------------------------------------------------------
/// Parse a generic type string (e.g., `Vec<i32>`) into base name and concrete arguments.
pub(crate) fn parse_generic_type(ty: &str) -> Option<(String, Vec<String>)> {
    if let Some(angle_start) = ty.find('<') {
        let base = ty[..angle_start].to_string();
        let args_str = &ty[angle_start + 1..ty.len() - 1];
        let args: Vec<String> = args_str.split(',').map(|s| s.trim().to_string()).collect();
        Some((base, args))
    } else {
        None
    }
}
