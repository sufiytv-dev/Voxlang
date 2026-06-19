// type_map.rs - Vox → LLVM type mapping and related utilities.
//
// Contains methods for mapping Vox types to LLVM types, computing sizes,
// stripping generic arguments/references, and resolving concrete field types.
//
// UPDATED (2026-06-14):
// - For NVPTX, uses opaque pointers (`ptr`) with address spaces.
// - `device_ptr_type` returns `"ptr addrspace(5)"` for local memory.
// - Reference parameters become `ptr addrspace(1)` for global memory.
// - Allocas get `addrspace(5)` via `alloca_addrspace_suffix`.
// - Added support for qualified struct names (e.g., `math::Point`).

use crate::codegen::CodegenEngine;
use crate::codegen::helpers::sanitize_type_name;
use crate::diagnostic::{Diagnostic, emit_diagnostic};
use std::collections::HashMap;

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

/// Helper to generate a mangled LLVM struct name from a base name and concrete arguments.
fn mangle_struct_name(base: &str, args: &[String]) -> String {
    let args_str = args
        .iter()
        .map(|a| sanitize_type_name(a))
        .collect::<Vec<_>>()
        .join("_");
    format!("%{}_{}", base, args_str)
}

impl CodegenEngine {
    /// Log a type mapping debug message if global debug is enabled.
    fn debug_log_type(&self, msg: &str) {
        if crate::diagnostic::global_debug() {
            crate::diagnostic::debug_log(format!("[CODEGEN:TYPE_MAP] {}", msg));
        }
    }

    // ------------------------------------------------------------------------
    // Target detection helpers (NVPTX / AMD)
    // ------------------------------------------------------------------------
    /// Detect if target is NVPTX (NVIDIA GPU)
    pub fn is_nvptx(&self) -> bool {
        self.device_triple
            .as_ref()
            .map(|t| t.contains("nvptx"))
            .unwrap_or(false)
    }

    /// Detect if target is AMDGCN (AMD GPU)
    pub fn is_amdgcn(&self) -> bool {
        self.device_triple
            .as_ref()
            .map(|t| t.contains("amdgcn"))
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------------
    // Strip helpers
    // ------------------------------------------------------------------------
    /// Strip generic arguments from a type string (e.g., `Vec<i32>` → `Vec`).
    pub(crate) fn strip_generic_args(ty: &str) -> String {
        if let Some(angle_pos) = ty.find('<') {
            ty[..angle_pos].to_string()
        } else {
            ty.to_string()
        }
    }

    /// Strip outer references from a type string (e.g., `&mut i32` → `i32`).
    pub(crate) fn strip_references(ty: &str) -> &str {
        let mut stripped = ty;
        while stripped.starts_with('&') {
            if let Some(s) = stripped.strip_prefix("&mut ") {
                stripped = s;
            } else if let Some(s) = stripped.strip_prefix("& ") {
                stripped = s;
            } else {
                break;
            }
        }
        stripped.trim()
    }

    /// Strip trailing `*` and optional address space qualifier from a pointer type.
    pub fn strip_pointer_and_addrspace(ty: &str) -> String {
        let without_star = ty.trim_end_matches('*');
        let without_addrspace = if let Some(idx) = without_star.find(" addrspace(") {
            &without_star[0..idx]
        } else {
            without_star
        };
        without_addrspace.trim().to_string()
    }

    /// Strip the module prefix from a qualified name (e.g., `math::Point` → `Point`).
    pub(crate) fn strip_module_prefix(ty: &str) -> String {
        if let Some(last) = ty.rsplit("::").next() {
            last.to_string()
        } else {
            ty.to_string()
        }
    }

    // ------------------------------------------------------------------------
    // Address space and pointer type helpers (for device IR)
    // ------------------------------------------------------------------------
    /// Return an address space suffix for `alloca` (e.g., `, addrspace(5)` for GPU).
    pub fn alloca_addrspace_suffix(&self) -> String {
        if self.is_nvptx() || self.is_amdgcn() {
            ", addrspace(5)".to_string()
        } else {
            String::new()
        }
    }

    /// For device IR, return the pointer type string for local memory (allocas).
    /// For NVPTX/AMD, this is `ptr addrspace(5)`. For host, explicit element pointer.
    pub fn device_ptr_type(&self, _elem_ty: &str) -> String {
        if self.is_nvptx() || self.is_amdgcn() {
            "ptr addrspace(5)".to_string()
        } else {
            // Host: keep explicit pointer type
            format!("{}*", _elem_ty)
        }
    }

    /// Return the LLVM pointer type to use for host `alloca` (opaque `ptr`).
    /// Kept for host code only.
    pub fn alloca_pointer_type(&self) -> String {
        if let Some(triple) = &self.device_triple {
            if triple.contains("amdgcn") {
                return "ptr addrspace(5)".to_string();
            }
        }
        "ptr".to_string()
    }

    // ------------------------------------------------------------------------
    // Type mapping
    // ------------------------------------------------------------------------
    /// Resolve the LLVM type of a field in a concrete generic struct.
    pub fn get_concrete_field_llvm_type(
        &self,
        base_struct: &str,
        concrete_struct_ty: &str,
        field_name: &str,
        is_device: bool,
    ) -> Option<String> {
        // Strip module prefix from base_struct if present
        let base_no_module = Self::strip_module_prefix(base_struct);
        let fields = self.struct_fields.get(&base_no_module)?;
        let generic_params = self.struct_generic_params.get(&base_no_module)?;

        let (_, args) = parse_generic_type(concrete_struct_ty)?;
        if args.len() != generic_params.len() {
            return None;
        }

        let mut subst = HashMap::new();
        for (gp, arg) in generic_params.iter().zip(args.iter()) {
            subst.insert(gp.clone(), arg.clone());
        }

        for (fname, fty) in fields {
            if fname == field_name {
                let concrete_fty = Self::substitute_type_string(fty, &subst);
                let llvm_ty = self.map_type(&concrete_fty, is_device);
                return Some(llvm_ty);
            }
        }
        None
    }

    /// Map a Vox type to its LLVM representation.
    pub fn map_type(&self, vox_type: &str, is_device: bool) -> String {
        let trimmed = vox_type.trim();
        self.debug_log_type(&format!(
            "map_type: input='{}', is_device={}",
            trimmed, is_device
        ));

        // Expand type aliases recursively before mapping
        let expanded = self.expand_type_aliases(trimmed);
        if expanded != trimmed {
            self.debug_log_type(&format!("  expanded alias '{}' -> '{}'", trimmed, expanded));
        }
        let trimmed_str = expanded.as_str();

        // Plain Option without type parameter → anonymous { i32, i32 }
        if trimmed_str == "Option" {
            self.debug_log_type("  -> plain Option -> { i32, i32 }");
            return "{ i32, i32 }".to_string();
        }

        if trimmed_str == "String" {
            self.debug_log_type("  -> String -> { i8*, i64, i64 }");
            return "{ i8*, i64, i64 }".to_string();
        }
        if trimmed_str == "&str" {
            self.debug_log_type("  -> &str -> { i8*, i64 }");
            return "{ i8*, i64 }".to_string();
        }

        // Already‑mapped LLVM types – pass through.
        if trimmed_str == "{ i32, i32 }"
            || trimmed_str == "{ i8*, i64 }"
            || trimmed_str == "{ i8*, i64, i64 }"
        {
            self.debug_log_type(&format!("  -> already LLVM type: {}", trimmed_str));
            return trimmed_str.to_string();
        }

        // --------------------------------------------------------------------
        // References (&mut T, & T)
        // --------------------------------------------------------------------
        if trimmed_str.starts_with("&mut ") || trimmed_str.starts_with("& ") {
            let inner = if trimmed_str.starts_with("&mut ") {
                &trimmed_str[5..]
            } else {
                &trimmed_str[2..]
            };
            let inner_ty = self.map_type(inner, is_device);
            let result = if is_device {
                // On GPU, kernel arguments that are pointers must be in address space 1 (global)
                "ptr addrspace(1)".to_string()
            } else {
                format!("{}*", inner_ty)
            };
            self.debug_log_type(&format!("  -> {} -> {}", trimmed_str, result));
            return result;
        }

        // Dynamic array ([]T) → opaque struct { i8*, i64, i64 }
        if trimmed_str.starts_with("[]") {
            self.debug_log_type("  -> [] -> { i8*, i64, i64 }");
            return "{ i8*, i64, i64 }".to_string();
        }

        // Fixed‑size array [N x T]
        if trimmed_str.starts_with('[') && trimmed_str.contains('x') {
            let parts: Vec<&str> = trimmed_str.split('x').collect();
            if parts.len() == 2 {
                let len_part = parts[0].trim_start_matches('[').trim();
                let elem_ty = parts[1].trim_end_matches(']').trim();
                let elem_llvm = self.map_type(elem_ty, is_device);
                if len_part == "?" {
                    self.debug_log_type(&format!("  -> [? x {}] -> {}*", elem_ty, elem_llvm));
                    return format!("{}*", elem_llvm);
                } else if let Ok(len) = len_part.parse::<u32>() {
                    let result = format!("[{} x {}]", len, elem_llvm);
                    self.debug_log_type(&format!("  -> [{} x {}] -> {}", len, elem_ty, result));
                    return result;
                }
            }
        }

        // Vec<T> and HashMap<K,V> → opaque i8*
        if let Some((base_name, _)) = parse_generic_type(trimmed_str) {
            if base_name == "Vec" || base_name == "HashMap" {
                self.debug_log_type(&format!("  -> {} -> i8*", base_name));
                return "i8*".to_string();
            }
        }

        // Option<T> → named struct %Option_T
        if let Some((base_name, args)) = parse_generic_type(trimmed_str) {
            if base_name == "Option" && args.len() == 1 {
                let arg = &args[0];
                if arg.is_empty() || arg == "?" || arg.contains('?') {
                    self.debug_log_type(&format!(
                        "  -> Option with unknown parameter '{}' -> {{ i32, i32 }}",
                        arg
                    ));
                    return "{ i32, i32 }".to_string();
                }
                let payload_ty = self.map_type(arg, is_device);
                let mangled = mangle_struct_name("Option", &args);
                {
                    let cache = self.concrete_struct_defs.borrow();
                    if cache.contains_key(&mangled) {
                        self.debug_log_type(&format!(
                            "  -> Option<{}> -> already defined {}",
                            arg, mangled
                        ));
                        return mangled;
                    }
                }
                let struct_body = format!("i32, {}", payload_ty);
                let def_line = format!("{} = type {{ {} }}", mangled, struct_body);
                self.pending_concrete_struct_defs
                    .borrow_mut()
                    .push(def_line);
                self.concrete_struct_defs
                    .borrow_mut()
                    .insert(mangled.clone(), struct_body);
                self.debug_log_type(&format!(
                    "  -> Option<{}> -> new named struct {}",
                    arg, mangled
                ));
                return mangled;
            }
        }

        // Result<T,E> → anonymous struct literal (no forward declaration)
        if let Some((base_name, args)) = parse_generic_type(trimmed_str) {
            if base_name == "Result" && args.len() == 2 {
                let (ok_ty, err_ty) = (args[0].clone(), args[1].clone());
                if ok_ty.contains('?') || err_ty.contains('?') {
                    self.debug_log_type(&format!(
                        "  -> Result with unknown parameters: Ok='{}', Err='{}' -> {{ i32, i32 }}",
                        ok_ty, err_ty
                    ));
                    return "{ i32, i32 }".to_string();
                }
                let ok_llvm = self.map_type(&ok_ty, is_device);
                let err_llvm = self.map_type(&err_ty, is_device);
                let anonymous_struct = format!("{{ i32, {}, {} }}", ok_llvm, err_llvm);
                self.debug_log_type(&format!(
                    "  -> Result<{},{}> -> {}",
                    ok_ty, err_ty, anonymous_struct
                ));
                return anonymous_struct;
            }
        }

        // Special‑case non‑generic struct with empty angle brackets (e.g., "Point<>")
        if let Some((base_name, args)) = parse_generic_type(trimmed_str) {
            let base_no_module = Self::strip_module_prefix(&base_name);
            if let Some(generic_params) = self.struct_generic_params.get(&base_no_module) {
                if generic_params.is_empty() && args.iter().all(|a| a.is_empty() || a == "?") {
                    self.debug_log_type(&format!(
                        "  -> non‑generic struct with empty brackets '{}' -> %{}",
                        trimmed_str, base_no_module
                    ));
                    return format!("%{}", base_no_module);
                }
            }
        }

        // Generic structs (e.g., Pair<T,U>) – generate a concrete named struct on demand.
        if let Some((base_name, args)) = parse_generic_type(trimmed_str) {
            let base_no_module = Self::strip_module_prefix(&base_name);
            if self.struct_fields.contains_key(&base_no_module) {
                let mangled = mangle_struct_name(&base_no_module, &args);
                {
                    let cache = self.concrete_struct_defs.borrow();
                    if cache.contains_key(&mangled) {
                        self.debug_log_type(&format!(
                            "  -> generic struct {}{:?} -> cached {}",
                            base_no_module, args, mangled
                        ));
                        return mangled;
                    }
                }
                let generic_fields = self.struct_fields.get(&base_no_module).unwrap();
                let generic_params = self.struct_generic_params.get(&base_no_module).unwrap();
                let mut subst = HashMap::new();
                for (gp, arg) in generic_params.iter().zip(args.iter()) {
                    subst.insert(gp.clone(), arg.clone());
                }
                let mut field_llvm_types = Vec::new();
                for (_, field_ty) in generic_fields {
                    let concrete_ty = if let Some(arg) = subst.get(field_ty) {
                        arg.clone()
                    } else {
                        field_ty.clone()
                    };
                    let llvm_ty = self.map_type(&concrete_ty, is_device);
                    field_llvm_types.push(llvm_ty);
                }
                let struct_body = field_llvm_types.join(", ");
                let def_line = format!("{} = type {{ {} }}", mangled, struct_body);
                self.pending_concrete_struct_defs
                    .borrow_mut()
                    .push(def_line);
                self.concrete_struct_defs
                    .borrow_mut()
                    .insert(mangled.clone(), struct_body);
                self.debug_log_type(&format!(
                    "  -> generic struct {}{:?} -> new named struct {}",
                    base_no_module, args, mangled
                ));
                return mangled;
            }
        }

        // Plain struct (non‑generic) – already defined as %StructName.
        let base_name = Self::strip_generic_args(trimmed_str);
        let base_no_module = Self::strip_module_prefix(&base_name);
        if self.struct_fields.contains_key(&base_no_module) {
            let result = format!("%{}", base_no_module);
            self.debug_log_type(&format!("  -> struct {} -> {}", base_name, result));
            return result;
        }

        // Enum types (except Option and Result) → anonymous { i32, i32 }
        let base_enum = Self::strip_generic_args(trimmed_str);
        let base_enum_no_module = Self::strip_module_prefix(&base_enum);
        if self.enum_variants.contains_key(&base_enum_no_module) {
            self.debug_log_type(&format!("  -> enum {} -> {{ i32, i32 }}", base_enum));
            return "{ i32, i32 }".to_string();
        }

        // Primitives and fallback
        let result = match trimmed_str {
            "i8" => "i8",
            "i8*" => "i8*",
            "i16" => "i16",
            "i32" => "i32",
            "i64" => "i64",
            "u8" => "i8",
            "u16" => "i16",
            "u32" => "i32",
            "u64" => "i64",
            "f32" => "float",
            "f64" => "double",
            "void" => "void",
            "bool" => "i1",
            "char" => "i32",
            "*const u8" => "i8*",
            "*mut u8" => "i8*",
            "usize" => "i32",
            _ => {
                if trimmed_str.len() == 1
                    && trimmed_str.chars().next().unwrap().is_ascii_uppercase()
                {
                    "i32"
                } else {
                    emit_diagnostic(
                        &Diagnostic::warning(&format!(
                            "Unknown type '{}', defaulting to i32",
                            trimmed_str
                        ))
                        .with_code("VX0401"),
                    );
                    "i32"
                }
            }
        }
        .to_string();
        self.debug_log_type(&format!(
            "  -> primitive or fallback: {} -> {}",
            trimmed_str, result
        ));
        result
    }

    /// Compute the size in bytes of a Vox type.
    pub fn size_of_type(&self, vox_type: &str) -> u64 {
        let expanded = self.expand_type_aliases(vox_type);
        let ty = expanded.as_str();
        match ty {
            "i8" | "u8" => 1,
            "i16" | "u16" => 2,
            "i32" | "u32" | "f32" => 4,
            "i64" | "u64" | "f64" => 8,
            "bool" => 1,
            "char" => 4,
            "i8*" | "String" => 8,
            "&str" => 16,
            "*const u8" | "*mut u8" => 8,
            "usize" => 4,
            t if t == "String" => 24,
            t if t.starts_with('[') => {
                if let Some(len_start) = t.find('[') {
                    let after = &t[len_start + 1..];
                    if let Some(x_pos) = after.find('x') {
                        let len_str = after[..x_pos].trim();
                        let elem_str = after[x_pos + 1..].trim_end_matches(']').trim();
                        let len = len_str.parse::<u64>().unwrap_or(0);
                        let elem_size = self.size_of_type(elem_str);
                        return len * elem_size;
                    }
                }
                0
            }
            t if t.starts_with("[]") => 24,
            t if t.starts_with('%') => {
                let struct_name = &t[1..];
                let struct_no_module = Self::strip_module_prefix(struct_name);
                if struct_no_module.starts_with("Option_") {
                    if let Some(underscore_pos) = struct_no_module.find('_') {
                        let ty_part = &struct_no_module[underscore_pos + 1..];
                        let mut payload_size = 4;
                        if let Some((base, _)) = parse_generic_type(ty_part) {
                            payload_size = self.size_of_type(&base);
                        } else {
                            payload_size = self.size_of_type(ty_part);
                        }
                        return 4 + payload_size;
                    }
                } else if struct_no_module.starts_with("Result_") {
                    let mut total = 4;
                    let parts: Vec<&str> = struct_no_module.split('_').collect();
                    if parts.len() >= 3 {
                        let t_part = parts[1];
                        let e_part = parts[2];
                        total += self.size_of_type(t_part);
                        total += self.size_of_type(e_part);
                    } else {
                        total += 4 + 4;
                    }
                    return total;
                }
                if let Some(fields) = self.struct_fields.get(&struct_no_module) {
                    let mut total = 0;
                    for (_, fty) in fields {
                        total += self.size_of_type(fty);
                    }
                    total
                } else {
                    8
                }
            }
            "HashMap" => 8,
            _ => 0,
        }
    }

    /// Substitute types in a type string using a substitution map.
    pub(crate) fn substitute_type_string(ty: &str, subst: &HashMap<String, String>) -> String {
        let mut result = ty.to_string();
        for (gp, conc) in subst {
            result = result.replace(gp, conc);
        }
        if result.ends_with("<>") {
            result = result.trim_end_matches("<>").to_string();
        }
        result
    }
}
