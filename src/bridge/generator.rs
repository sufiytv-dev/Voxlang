// src/bridge/generator.rs – Generate Vox FFI wrappers from C functions

use super::parser::CFunction;
use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};

/// Map C types to Vox types.
/// Returns a Vox type string (e.g., "i32", "*mut i8", "*const i8", "void").
/// On unknown or unsupported types, emits a diagnostic and returns Err.
fn map_type(c_ty: &str) -> Result<String, Diagnostic> {
    let ty = c_ty.trim();
    debug_log(format!("bridge: mapping C type '{}'", ty));

    let mapped = match ty {
        // Signed integers
        "int" | "long" | "long long" => "i32".to_string(),
        "short" => "i16".to_string(),
        "char" => "i8".to_string(),
        // Unsigned integers
        "unsigned int" | "unsigned long" => "u32".to_string(),
        "unsigned short" => "u16".to_string(),
        "unsigned char" => "u8".to_string(),
        "size_t" => "usize".to_string(),
        "uint32_t" => "u32".to_string(),
        "uint64_t" => "u64".to_string(),
        // Floating point
        "float" => "f32".to_string(),
        "double" => "f64".to_string(),
        // Void
        "void" => "void".to_string(),
        // Pointers
        t if t.ends_with('*') => {
            let base = &t[..t.len() - 1].trim();
            if base.starts_with("const ") {
                let real_base = &base[6..]; // strip "const "
                format!("*const {}", map_type(real_base)?)
            } else {
                format!("*mut {}", map_type(base)?)
            }
        }
        _ => {
            let diag =
                Diagnostic::error(format!("Unsupported C type: '{}'", ty)).with_code("VX0401");
            emit_diagnostic(&diag);
            return Err(diag);
        }
    };
    Ok(mapped)
}

/// Generate a safe Vox wrapper that:
/// - declares an `extern` function (unsafe to call directly)
/// - provides a safe wrapper with non‑null where‑clauses for pointer parameters
/// - wraps the extern call in an `unsafe` block
/// - correctly handles `void` return type
pub fn generate_vox_decl(func: &CFunction) -> Result<String, Diagnostic> {
    debug_log(format!("bridge: generating wrapper for '{}'", func.name));

    let ret_ty = map_type(&func.return_type)?;
    let mut param_decls = Vec::new();
    let mut param_names = Vec::new();
    for (ty, name) in func.param_types.iter().zip(func.param_names.iter()) {
        let vox_ty = map_type(ty)?;
        param_decls.push(format!("{}: {}", vox_ty, name));
        param_names.push(name.clone());
    }
    let param_list = param_decls.join(", ");

    // Where clauses: pointer parameters must not be null
    let where_clauses: Vec<String> = func
        .param_types
        .iter()
        .zip(func.param_names.iter())
        .filter_map(|(ty, name)| {
            if ty.contains('*') {
                Some(format!("{} != null", name))
            } else {
                None
            }
        })
        .collect();
    let where_str = if where_clauses.is_empty() {
        String::new()
    } else {
        format!(" where {}", where_clauses.join(" && "))
    };

    // External declaration (unsafe to call directly)
    let extern_decl = format!("extern fn {}({}) -> {};", func.name, param_list, ret_ty);

    // Safe wrapper body
    let call_args = param_names.join(", ");
    let call_expr = if ret_ty == "void" {
        format!("unsafe {{ {} ({}) }}", func.name, call_args)
    } else {
        format!("return unsafe {{ {} ({}) }}", func.name, call_args)
    };

    let safe_wrapper = format!(
        "fn safe_{}({}) -> {}{}:\n    {}\n}}",
        func.name, param_list, ret_ty, where_str, call_expr
    );

    Ok(format!("{}\n\n{}\n", extern_decl, safe_wrapper))
}
