// semantics/builtins.rs
//! Registration of built‑in generic enums and structs (Option, Result, Vec, HashMap).

use crate::frontend::span::Span;
use crate::parser::EnumVariant;
use crate::semantics::symbol::SymbolTable;
use crate::semantics::types::Type;

/// Register all built‑in types into the symbol table.
/// This is called from `SymbolTable::new()`.
pub(crate) fn register_builtins(st: &mut SymbolTable) {
    // Register built‑in generic enum Option<T>
    st.register_enum(
        "Option",
        vec!["T".to_string()],
        vec![
            EnumVariant {
                name: "None".to_string(),
                span: Span::dummy(),
            },
            EnumVariant {
                name: "Some".to_string(),
                span: Span::dummy(),
            },
        ],
        vec![None, Some(Type::GenericParam("T".to_string()))],
    );

    // Register built‑in generic enum Result<T, E>
    st.register_enum(
        "Result",
        vec!["T".to_string(), "E".to_string()],
        vec![
            EnumVariant {
                name: "Ok".to_string(),
                span: Span::dummy(),
            },
            EnumVariant {
                name: "Err".to_string(),
                span: Span::dummy(),
            },
        ],
        vec![
            Some(Type::GenericParam("T".to_string())),
            Some(Type::GenericParam("E".to_string())),
        ],
    );

    // Register built‑in generic struct Vec<T> (opaque, no fields)
    st.register_struct(
        "Vec",
        vec!["T".to_string()],
        vec![], // no fields – the type is opaque for the frontend
    );

    // Register built‑in generic struct HashMap<K, V> (opaque, no fields)
    st.register_struct(
        "HashMap",
        vec!["K".to_string(), "V".to_string()],
        vec![], // opaque – no accessible fields
    );
}
