// expr.rs - Expression compilation for Voxlang (host and device).
//
// Contains the `ExprEmitter` which compiles AST expressions to LLVM IR.
// Supports all Vox expressions including literals, identifiers, arithmetic,
// field access, array indexing, struct literals, match expressions, casts,
// borrow/deref, calls (including generic function monomorphisation), and
// built‑ins for Vec, HashMap, String, etc.
//
// UPDATED (2026-06-14): Fixed device IR generation for loads/stores.
// Now emits opaque pointer types with correct address spaces for NVPTX.
//
// FIXED (2026-06-14): KernelLaunch now correctly handles &mut arguments:
// - Copies the initial value from the host variable (not a temporary pointer)
// - Stores the device pointer value in a temporary on the stack, then passes
//   the address of that temporary to the kernel launch API.
// - Uses the correct size for the pointee type

use crate::codegen::CodegenEngine;
use crate::codegen::type_map::parse_generic_type;
use crate::comptime::ComptimeEvaluator;
use crate::diagnostic::{Diagnostic, emit_diagnostic};
use crate::frontend::token::TokenKind;
use crate::parser::{ASTNode, MatchArm, MatchPattern};
use std::collections::HashMap;

// ----------------------------------------------------------------------------
// CodegenTarget – whether we are generating host or device IR
// ----------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CodegenTarget {
    Host,
    Device,
}

// ----------------------------------------------------------------------------
// ExprEmitter – compiles an expression into an SSA value or pointer
// ----------------------------------------------------------------------------
pub(crate) struct ExprEmitter<'a> {
    pub(crate) engine: &'a mut CodegenEngine,
    pub(crate) target: CodegenTarget,
    pub(crate) lvalue: bool, // if true, returns a pointer to the value
    pub(crate) expected_type: Option<String>, // expected Vox type (for generics)
}

impl<'a> ExprEmitter<'a> {
    /// Emit a line of IR, routing to either host or device builder.
    fn emit(&mut self, line: &str) {
        match self.target {
            CodegenTarget::Host => self.engine.debug_emit(line),
            CodegenTarget::Device => self.engine.debug_emit_device(line),
        }
    }

    /// Helper: return the pointer type to use for a load/store of an alloca on the device.
    fn device_ptr_type(&self, elem_ty: &str) -> String {
        if self.target == CodegenTarget::Device {
            self.engine.device_ptr_type(elem_ty)
        } else {
            format!("{}*", elem_ty)
        }
    }

    /// Infer the Vox type of an expression (delegates to the engine).
    fn expr_type(&self, node: &ASTNode) -> Option<String> {
        Some(self.engine.infer_vox_type(node))
    }

    /// Check if a Vox type string represents an integer type.
    fn is_integer_type(ty: &str) -> bool {
        matches!(
            ty,
            "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "char"
        )
    }

    /// Parse a literal LLVM struct type like `{ i32, i32 }` into a list of field type strings.
    fn parse_struct_fields(ty: &str) -> Vec<String> {
        let ty = ty.trim();
        if !ty.starts_with('{') || !ty.ends_with('}') {
            return vec![];
        }
        let inner = &ty[1..ty.len() - 1].trim();
        if inner.is_empty() {
            return vec![];
        }
        let mut fields = Vec::new();
        let mut depth = 0;
        let mut start = 0;
        let chars: Vec<char> = inner.chars().collect();
        for i in 0..chars.len() {
            match chars[i] {
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    fields.push(inner[start..i].trim().to_string());
                    start = i + 1;
                }
                _ => {}
            }
        }
        if start < inner.len() {
            fields.push(inner[start..].trim().to_string());
        }
        fields
    }

    /// Get field types for a struct, handling both literal `{ ... }` and named `%...` types.
    fn get_struct_field_types(&self, llvm_ty: &str) -> Vec<String> {
        let ty = llvm_ty.trim();
        if ty.starts_with('{') && ty.ends_with('}') {
            Self::parse_struct_fields(ty)
        } else if ty.starts_with('%') {
            let cache = self.engine.concrete_struct_defs.borrow();
            if let Some(body) = cache.get(ty) {
                body.split(',').map(|s| s.trim().to_string()).collect()
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    }

    // ------------------------------------------------------------------------
    // Main expression compilation entry point
    // ------------------------------------------------------------------------
    pub(crate) fn compile(&mut self, node: &ASTNode) -> String {
        if self.engine.has_error {
            self.engine
                .debug_log("compile: early exit due to has_error");
            return "0".to_string();
        }
        self.engine
            .debug_log(&format!("compile_expr({:?}) for {:?}", node, self.target));

        match node {
            ASTNode::IntegerLiteral(val, _) => {
                self.engine
                    .debug_log(&format!("compile integer literal {}", val));
                val.to_string()
            }
            ASTNode::FloatLiteral(val, _) => {
                self.engine
                    .debug_log(&format!("compile float literal {}", val));
                format!("{:.10}", val)
            }
            ASTNode::CharLiteral(c, _) => {
                self.engine
                    .debug_log(&format!("compile char literal {}", c));
                c.to_string()
            }
            ASTNode::StringLiteral(s, _) => {
                self.engine
                    .debug_log(&format!("compile string literal \"{}\"", s));
                self.engine.get_string_fat_ptr(s)
            }

            ASTNode::Identifier(name, _) => {
                self.engine
                    .debug_log(&format!("compile identifier '{}'", name));
                if let Some((ty_str, _)) = self.engine.global_variables.get(name).cloned() {
                    if self.lvalue {
                        self.engine.debug_log(&format!("  as lvalue -> @{}", name));
                        format!("@{}", name)
                    } else {
                        let load_reg = self.engine.next_register();
                        let ptr_ty = if self.target == CodegenTarget::Host {
                            format!("{}*", ty_str)
                        } else {
                            // For device, use opaque pointer with address space (local)
                            self.engine.device_ptr_type(&ty_str)
                        };
                        self.emit(&format!(
                            "    {} = load {}, {} {}",
                            load_reg, ty_str, ptr_ty, name
                        ));
                        load_reg
                    }
                } else if let Some((enum_name, variant_name)) = name.split_once("::") {
                    let base_enum = CodegenEngine::strip_generic_args(enum_name);
                    if let Some(variants) = self.engine.enum_variants.get(&base_enum) {
                        if let Some(&discriminant) = variants.get(variant_name) {
                            if discriminant == 0 {
                                self.engine.debug_log(&format!(
                                    "  enum {}::{} -> zeroinitializer",
                                    base_enum, variant_name
                                ));
                                return "zeroinitializer".to_string();
                            } else {
                                let result = format!("{{ i32 {}, i32 0 }}", discriminant);
                                self.engine.debug_log(&format!(
                                    "  enum {}::{} -> {}",
                                    base_enum, variant_name, result
                                ));
                                return result;
                            }
                        }
                    }
                    self.engine.debug_log(&format!(
                        "  enum {}::{} not found, fallback 0",
                        base_enum, variant_name
                    ));
                    "0".to_string()
                } else if let Some((ty_str, alloc_reg, _, _)) =
                    self.engine.variable_symbols.get(name).cloned()
                {
                    if self.lvalue {
                        self.engine.debug_log(&format!(
                            "  variable '{}' as lvalue -> {}",
                            name, alloc_reg
                        ));
                        alloc_reg
                    } else {
                        let result_reg = self.engine.next_register();
                        let ptr_ty = if self.target == CodegenTarget::Host {
                            format!("{}*", ty_str)
                        } else {
                            // For device, use opaque pointer with address space (local)
                            self.engine.device_ptr_type(&ty_str)
                        };
                        self.emit(&format!(
                            "    {} = load {}, {} {}",
                            result_reg, ty_str, ptr_ty, alloc_reg
                        ));
                        result_reg
                    }
                } else {
                    self.engine.debug_log(&format!(
                        "identifier '{}' not found, treating as external",
                        name
                    ));
                    format!("%{}", name)
                }
            }

            ASTNode::FieldAccess { expr, field, .. } => {
                self.engine
                    .debug_log(&format!("FieldAccess: expr={:?}, field={}", expr, field));

                let saved_lvalue = self.lvalue;
                self.lvalue = true;
                let base_ptr = self.compile(expr);
                self.lvalue = saved_lvalue;
                self.engine
                    .debug_log(&format!("FieldAccess: base_ptr = {}", base_ptr));

                let base_vox_ty = self.expr_type(expr).unwrap_or_default();
                self.engine.debug_log(&format!(
                    "FieldAccess: base_vox_ty = '{}', field = '{}'",
                    base_vox_ty, field
                ));

                let mut ty = base_vox_ty.as_str();
                let mut ptr_reg = base_ptr;
                let mut loaded_count = 0;

                while ty.starts_with('&') {
                    self.engine.debug_log(&format!(
                        "FieldAccess: loading through reference ({}), ptr_reg = {}",
                        ty, ptr_reg
                    ));
                    let loaded = self.engine.next_register();
                    let ptr_ty = self
                        .engine
                        .map_type(ty, self.target == CodegenTarget::Device);
                    self.emit(&format!(
                        "    {} = load {}, {} {}",
                        loaded, ptr_ty, ptr_ty, ptr_reg
                    ));
                    ptr_reg = loaded;
                    if let Some(s) = ty.strip_prefix("&mut ") {
                        ty = s;
                    } else if let Some(s) = ty.strip_prefix("& ") {
                        ty = s;
                    } else {
                        break;
                    }
                    ty = ty.trim();
                    loaded_count += 1;
                }

                let stripped_ty = ty;
                let base_name = CodegenEngine::strip_generic_args(stripped_ty);
                self.engine.debug_log(&format!(
                    "FieldAccess after reference stripping: stripped_ty='{}', base_name='{}', ptr_reg={}",
                    stripped_ty, base_name, ptr_reg
                ));

                if base_name == "Vec" {
                    self.engine
                        .debug_log("FieldAccess on Vec<T> – emitting runtime call");
                    // Load the opaque handle (i8*) from the pointer
                    let handle = self.engine.next_register();
                    self.emit(&format!("    {} = load i8*, i8** {}", handle, ptr_reg));
                    if field == "len" {
                        let len_i64 = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = call i64 @vox_vec_len(i8* {})",
                            len_i64, handle
                        ));
                        return len_i64;
                    } else if field == "ptr" {
                        // For Vec::ptr, we need to load the data pointer from the Vec struct
                        let struct_ptr = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = bitcast i8* {} to {{ i8*, i64, i64 }}*",
                            struct_ptr, handle
                        ));
                        let data_ptr_gep = self.engine.next_register();
                        self.emit(&format!("    {} = getelementptr inbounds {{ i8*, i64, i64 }}, {{ i8*, i64, i64 }}* {}, i32 0, i32 0", data_ptr_gep, struct_ptr));
                        let loaded_ptr = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = load i8*, i8** {}",
                            loaded_ptr, data_ptr_gep
                        ));
                        return loaded_ptr;
                    } else {
                        emit_diagnostic(&Diagnostic::error(&format!(
                            "Vec<T> has no field '{}' (only len, ptr, cap)",
                            field
                        )));
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                }

                let fields = match self.engine.struct_fields.get(&base_name) {
                    Some(f) => f.clone(),
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Struct '{}' not found for field access",
                                base_name
                            ))
                            .with_code("VX0427"),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                };

                let field_index = fields.iter().position(|(fname, _)| fname == field);
                let idx = match field_index {
                    Some(i) => i,
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Struct '{}' has no field '{}'",
                                base_name, field
                            ))
                            .with_code("VX0428"),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                };

                let struct_ty = self
                    .engine
                    .map_type(stripped_ty, self.target == CodegenTarget::Device);
                let gep_reg = self.engine.next_register();
                self.emit(&format!(
                    "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                    gep_reg, struct_ty, struct_ty, ptr_reg, idx
                ));

                if self.lvalue {
                    gep_reg
                } else {
                    let field_llvm_ty = if let Some(fty) = self.engine.get_concrete_field_llvm_type(
                        &base_name,
                        stripped_ty,
                        field,
                        self.target == CodegenTarget::Device,
                    ) {
                        fty
                    } else {
                        let field_ty = fields[idx].1.clone();
                        self.engine
                            .map_type(&field_ty, self.target == CodegenTarget::Device)
                    };

                    let result_reg = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        result_reg, field_llvm_ty, field_llvm_ty, gep_reg
                    ));
                    result_reg
                }
            }

            ASTNode::ArrayIndex { array, index, .. } => {
                self.engine.debug_log("compile ArrayIndex");
                let base_vox_ty = self.expr_type(array).unwrap_or_default();
                let is_vec = if let Some((base_name, _)) = parse_generic_type(&base_vox_ty) {
                    base_name == "Vec"
                } else {
                    false
                };

                if is_vec {
                    let saved_lvalue = self.lvalue;
                    self.lvalue = false;
                    let vec_handle = self.compile(array);
                    self.lvalue = saved_lvalue;
                    let idx_reg = self.compile(index);

                    if let Some((_, type_args)) = parse_generic_type(&base_vox_ty) {
                        if type_args.len() == 1 {
                            let elem_vox_ty = &type_args[0];
                            let elem_llvm_ty = self
                                .engine
                                .map_type(elem_vox_ty, self.target == CodegenTarget::Device);
                            let elem_size = self.engine.size_of_type(elem_vox_ty);
                            if elem_size == 0 {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Unknown element size for type '{}'",
                                        elem_vox_ty
                                    ))
                                    .with_code("VX0503"),
                                );
                                self.engine.has_error = true;
                                return "0".to_string();
                            }

                            let tmp = self.engine.next_register();
                            self.emit(&format!("    {} = alloca {}", tmp, elem_llvm_ty));
                            // Bitcast the temporary pointer to i8* for the runtime call
                            let tmp_i8 = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = bitcast {}* {} to i8*",
                                tmp_i8, elem_llvm_ty, tmp
                            ));
                            let idx_usize = self.engine.next_register();
                            self.emit(&format!("    {} = sext i32 {} to i64", idx_usize, idx_reg));
                            let success = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = call i32 @vox_vec_get(i8* {}, i64 {}, i8* {})",
                                success, vec_handle, idx_usize, tmp_i8
                            ));
                            let ok_label = self.engine.next_block();
                            let panic_label = self.engine.next_block();
                            let success_i1 = self.engine.next_register();
                            self.emit(&format!("    {} = icmp eq i32 {}, 0", success_i1, success));
                            self.emit(&format!(
                                "    br i1 {}, label %{}, label %{}",
                                success_i1, panic_label, ok_label
                            ));
                            self.emit(&format!("{}:", panic_label));
                            self.emit(&format!("    call void @vox_panic()"));
                            self.emit(&format!("    unreachable"));
                            self.emit(&format!("{}:", ok_label));
                            let result_reg = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = load {}, {}* {}",
                                result_reg, elem_llvm_ty, elem_llvm_ty, tmp
                            ));
                            return result_reg;
                        }
                    }
                    emit_diagnostic(
                        &Diagnostic::error("Unable to determine element type for Vec indexing")
                            .with_code("VX9999"),
                    );
                    self.engine.has_error = true;
                    return "0".to_string();
                }

                let saved_lvalue = self.lvalue;
                self.lvalue = true;
                let array_ptr = self.compile(array);
                self.lvalue = saved_lvalue;
                let idx_reg = self.compile(index);

                let array_ty = match &**array {
                    ASTNode::Identifier(name, _) => {
                        if let Some((ty, _, _, _)) = self.engine.variable_symbols.get(name) {
                            self.engine
                                .map_type(ty, self.target == CodegenTarget::Device)
                        } else {
                            "i32".to_string()
                        }
                    }
                    _ => "i32".to_string(),
                };

                let elem_ty = match &**array {
                    ASTNode::Identifier(name, _) => {
                        if let Some((ty, _, _, _)) = self.engine.variable_symbols.get(name) {
                            if let Some(inner) = ty.strip_prefix('[') {
                                if let Some(pos) = inner.find('x') {
                                    let after = &inner[pos + 1..];
                                    if let Some(end) = after.find(']') {
                                        after[..end].trim().to_string()
                                    } else {
                                        "i32".to_string()
                                    }
                                } else {
                                    "i32".to_string()
                                }
                            } else {
                                "i32".to_string()
                            }
                        } else {
                            "i32".to_string()
                        }
                    }
                    _ => "i32".to_string(),
                };
                let elem_llvm = self
                    .engine
                    .map_type(&elem_ty, self.target == CodegenTarget::Device);
                let gep_reg = self.engine.next_register();
                self.emit(&format!(
                    "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                    gep_reg, array_ty, array_ty, array_ptr, idx_reg
                ));
                let result_reg = self.engine.next_register();
                self.emit(&format!(
                    "    {} = load {}, {}* {}",
                    result_reg, elem_llvm, elem_llvm, gep_reg
                ));
                result_reg
            }

            ASTNode::StructLiteral { name, fields, span } => {
                self.engine
                    .debug_log(&format!("compile StructLiteral {}", name));
                if name == "Vec" {
                    let elem_vox_ty = if let Some(expected) = &self.expected_type {
                        if let Some((base_name, type_args)) = parse_generic_type(expected) {
                            if base_name == "Vec" && type_args.len() == 1 {
                                type_args[0].clone()
                            } else {
                                "i32".to_string()
                            }
                        } else {
                            "i32".to_string()
                        }
                    } else {
                        "i32".to_string()
                    };
                    let elem_size = self.engine.size_of_type(&elem_vox_ty);
                    if elem_size == 0 {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Unknown element size for type '{}'",
                                elem_vox_ty
                            ))
                            .with_code("VX9009")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let handle = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call i8* @vox_vec_new(i64 {})",
                        handle, elem_size
                    ));
                    return handle;
                }

                let base_fields = match self.engine.struct_fields.get(name) {
                    Some(f) => f.clone(),
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!("Unknown struct '{}' in literal", name))
                                .with_code("VX0429")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                };
                let generic_params = self
                    .engine
                    .struct_generic_params
                    .get(name)
                    .cloned()
                    .unwrap_or_default();

                let field_order = if let Some(expected) = &self.expected_type {
                    if let Some((base_name, args)) = parse_generic_type(expected) {
                        if base_name == *name
                            && !generic_params.is_empty()
                            && args.len() == generic_params.len()
                        {
                            let mut subst = HashMap::new();
                            for (gp, arg) in generic_params.iter().zip(args.iter()) {
                                subst.insert(gp.clone(), arg.clone());
                            }
                            let mut concrete_fields = Vec::new();
                            for (fname, fty) in &base_fields {
                                let concrete_ty = if let Some(arg) = subst.get(fty) {
                                    arg.clone()
                                } else {
                                    fty.clone()
                                };
                                concrete_fields.push((fname.clone(), concrete_ty));
                            }
                            concrete_fields
                        } else {
                            base_fields.clone()
                        }
                    } else {
                        base_fields.clone()
                    }
                } else {
                    base_fields.clone()
                };

                let mut field_map = HashMap::new();
                for (fname, expr) in fields {
                    field_map.insert(fname.clone(), expr);
                }

                for fname in field_map.keys() {
                    if !field_order.iter().any(|(f, _)| f == fname) {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Struct '{}' has no field named '{}'",
                                name, fname
                            ))
                            .with_code("VX0430")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                }

                let mut args = Vec::new();
                for (fname, fty) in &field_order {
                    match field_map.get(fname) {
                        Some(expr) => {
                            let arg_val = self.compile(expr);
                            args.push((fty.clone(), arg_val));
                        }
                        None => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Missing field '{}' in struct literal for '{}'",
                                    fname, name
                                ))
                                .with_code("VX0431")
                                .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                    }
                }

                let concrete_type_name = if let Some(expected) = &self.expected_type {
                    if let Some((base_name, _)) = parse_generic_type(expected) {
                        if base_name == *name {
                            expected.clone()
                        } else {
                            name.clone()
                        }
                    } else {
                        name.clone()
                    }
                } else {
                    name.clone()
                };
                let struct_ty = self
                    .engine
                    .map_type(&concrete_type_name, self.target == CodegenTarget::Device);
                let alloca_reg = self.engine.next_register();
                self.emit(&format!("    {} = alloca {}", alloca_reg, struct_ty));

                for (i, (field_ty, arg_val)) in args.iter().enumerate() {
                    let llvm_field_ty = self
                        .engine
                        .map_type(field_ty, self.target == CodegenTarget::Device);
                    let gep_reg = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                        gep_reg, struct_ty, struct_ty, alloca_reg, i
                    ));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        llvm_field_ty, arg_val, llvm_field_ty, gep_reg
                    ));
                }

                let loaded_reg = self.engine.next_register();
                self.emit(&format!(
                    "    {} = load {}, {}* {}",
                    loaded_reg, struct_ty, struct_ty, alloca_reg
                ));
                loaded_reg
            }

            ASTNode::SliceExpr {
                base, start, end, ..
            } => {
                self.engine.debug_log("compile SliceExpr");
                let base_val = self.compile(base);
                let data = self.engine.next_register();
                self.emit(&format!(
                    "    {} = extractvalue {{ i8*, i64 }} {}, 0",
                    data, base_val
                ));
                let len = self.engine.next_register();
                self.emit(&format!(
                    "    {} = extractvalue {{ i8*, i64 }} {}, 1",
                    len, base_val
                ));
                let start_idx = match start {
                    Some(expr) => {
                        let idx = self.compile(expr);
                        let idx_i64 = self.engine.next_register();
                        self.emit(&format!("    {} = sext i32 {} to i64", idx_i64, idx));
                        idx_i64
                    }
                    None => "0".to_string(),
                };
                let end_idx = match end {
                    Some(expr) => {
                        let idx = self.compile(expr);
                        let idx_i64 = self.engine.next_register();
                        self.emit(&format!("    {} = sext i32 {} to i64", idx_i64, idx));
                        idx_i64
                    }
                    None => len.clone(),
                };
                let new_len = self.engine.next_register();
                self.emit(&format!(
                    "    {} = sub i64 {}, {}",
                    new_len, end_idx, start_idx
                ));
                let new_data = self.engine.next_register();
                self.emit(&format!(
                    "    {} = getelementptr inbounds i8, i8* {}, i64 {}",
                    new_data, data, start_idx
                ));
                let fat_alloca = self.engine.next_register();
                self.emit(&format!("    {} = alloca {{ i8*, i64 }}", fat_alloca));
                let field0 = self.engine.next_register();
                self.emit(&format!(
                    "    {} = getelementptr inbounds {{ i8*, i64 }}, {{ i8*, i64 }}* {}, i32 0, i32 0",
                    field0, fat_alloca
                ));
                self.emit(&format!("    store i8* {}, i8** {}", new_data, field0));
                let field1 = self.engine.next_register();
                self.emit(&format!(
                    "    {} = getelementptr inbounds {{ i8*, i64 }}, {{ i8*, i64 }}* {}, i32 0, i32 1",
                    field1, fat_alloca
                ));
                self.emit(&format!("    store i64 {}, i64* {}", new_len, field1));
                let result = self.engine.next_register();
                self.emit(&format!(
                    "    {} = load {{ i8*, i64 }}, {{ i8*, i64 }}* {}",
                    result, fat_alloca
                ));
                result
            }

            ASTNode::MatchExpr {
                value,
                arms,
                span: _,
            } => {
                self.engine.debug_log("compile MatchExpr");
                let any_by_ref = arms.iter().any(|arm| match &arm.pattern {
                    MatchPattern::UnitVariant { by_ref, .. } => *by_ref,
                    MatchPattern::Binding { by_ref, .. } => *by_ref,
                    _ => false,
                });
                let scr = if any_by_ref {
                    Box::new(ASTNode::DerefExpr(value.clone(), value.span()))
                } else {
                    value.clone()
                };
                let scr_val_raw = self.compile(&scr);
                let scr_val = if scr_val_raw.ends_with('*') {
                    let loaded = self.engine.next_register();
                    let scr_vox_ty = self.expr_type(&scr).unwrap_or_default();
                    let enum_ty = self
                        .engine
                        .map_type(&scr_vox_ty, self.target == CodegenTarget::Device);
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        loaded, enum_ty, enum_ty, scr_val_raw
                    ));
                    loaded
                } else {
                    scr_val_raw
                };
                let scr_vox_ty = self
                    .expr_type(&scr)
                    .unwrap_or_else(|| "Option<i32>".to_string());
                let enum_ty = self
                    .engine
                    .map_type(&scr_vox_ty, self.target == CodegenTarget::Device);
                let scr_alloca = self.engine.next_register();
                self.emit(&format!("    {} = alloca {}", scr_alloca, enum_ty));
                self.emit(&format!(
                    "    store {} {}, {}* {}",
                    enum_ty, scr_val, enum_ty, scr_alloca
                ));

                let result_type = if let Some(expected) = &self.expected_type {
                    expected.clone()
                } else if let Some(first_arm) = arms.first() {
                    if let Some(expr) = first_arm.body.first() {
                        self.expr_type(expr).unwrap_or_else(|| "i32".to_string())
                    } else {
                        "i32".to_string()
                    }
                } else {
                    "i32".to_string()
                };
                let llvm_result_type = self
                    .engine
                    .map_type(&result_type, self.target == CodegenTarget::Device);
                let result_alloca = self.engine.next_register();
                self.emit(&format!(
                    "    {} = alloca {}",
                    result_alloca, llvm_result_type
                ));

                let flattened_arms: Vec<MatchArm> = arms
                    .iter()
                    .map(|arm| {
                        let body = if arm.body.len() == 1 {
                            if let ASTNode::Block { statements, .. } = &arm.body[0] {
                                statements.clone()
                            } else {
                                arm.body.clone()
                            }
                        } else {
                            arm.body.clone()
                        };
                        MatchArm {
                            pattern: arm.pattern.clone(),
                            body,
                            span: arm.span,
                        }
                    })
                    .collect();

                let merge_label = self.engine.next_block();
                let default_label = self.engine.next_block();
                let mut current_label = self.engine.next_block();
                self.emit(&format!("    br label %{}", current_label));

                let mut has_wildcard = false;
                let mut any_arm_terminated = false;

                for (idx, arm) in flattened_arms.iter().enumerate() {
                    if matches!(arm.pattern, MatchPattern::Wildcard(_)) {
                        has_wildcard = true;
                        continue;
                    }
                    let arm_label = self.engine.next_block();
                    let fall_label = if idx + 1 < flattened_arms.len()
                        && !matches!(flattened_arms[idx + 1].pattern, MatchPattern::Wildcard(_))
                    {
                        self.engine.next_block()
                    } else {
                        default_label.clone()
                    };

                    let disc_value = match &arm.pattern {
                        MatchPattern::UnitVariant { variant, .. } => {
                            let mut found = 0u32;
                            for (_, variants) in &self.engine.enum_variants {
                                if let Some(&d) = variants.get(variant.as_str()) {
                                    found = d;
                                    break;
                                }
                            }
                            found
                        }
                        MatchPattern::Binding { variant, .. } => {
                            let mut found = 0u32;
                            for (_, variants) in &self.engine.enum_variants {
                                if let Some(&d) = variants.get(variant.as_str()) {
                                    found = d;
                                    break;
                                }
                            }
                            found
                        }
                        _ => 0,
                    };

                    self.emit(&format!("{}:", current_label));
                    let disc_ptr = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                        disc_ptr, enum_ty, enum_ty, scr_alloca
                    ));
                    let disc_reg = self.engine.next_register();
                    self.emit(&format!("    {} = load i32, i32* {}", disc_reg, disc_ptr));
                    let cmp_reg = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = icmp eq i32 {}, {}",
                        cmp_reg, disc_reg, disc_value
                    ));
                    self.emit(&format!(
                        "    br i1 {}, label %{}, label %{}",
                        cmp_reg, arm_label, fall_label
                    ));
                    self.emit(&format!("{}:", arm_label));

                    if let MatchPattern::Binding {
                        variant,
                        bindings,
                        by_ref: _,
                        enum_name,
                        span,
                    } = &arm.pattern
                    {
                        let enum_base = CodegenEngine::strip_generic_args(&scr_vox_ty);
                        let payload_index = if enum_base == "Result" && variant == "Err" {
                            2
                        } else {
                            1
                        };

                        for binding in bindings {
                            if binding == "_" {
                                continue;
                            }
                            let (payload_vox, payload_llvm_ty) =
                                if let Some((base_name, args)) = parse_generic_type(&scr_vox_ty) {
                                    if base_name == "Option" && args.len() == 1 {
                                        let vox = args[0].clone();
                                        let llvm = self
                                            .engine
                                            .map_type(&vox, self.target == CodegenTarget::Device);
                                        (Some(vox), llvm)
                                    } else if base_name == "Result" && args.len() == 2 {
                                        let vox = if variant == "Ok" {
                                            args[0].clone()
                                        } else {
                                            args[1].clone()
                                        };
                                        let llvm = self
                                            .engine
                                            .map_type(&vox, self.target == CodegenTarget::Device);
                                        (Some(vox), llvm)
                                    } else {
                                        (None, "i32".to_string())
                                    }
                                } else {
                                    (None, "i32".to_string())
                                };

                            let alloc_reg = self.engine.next_register();
                            self.emit(&format!("    {} = alloca {}", alloc_reg, payload_llvm_ty));
                            self.engine.variable_symbols.insert(
                                binding.clone(),
                                (payload_llvm_ty.clone(), alloc_reg.clone(), false, true),
                            );
                            if let Some(vox_ty) = payload_vox {
                                self.engine.var_vox_types.insert(binding.clone(), vox_ty);
                            }

                            let payload_ptr = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                                payload_ptr, enum_ty, enum_ty, scr_alloca, payload_index
                            ));
                            let payload_val = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = load {}, {}* {}",
                                payload_val, payload_llvm_ty, payload_llvm_ty, payload_ptr
                            ));
                            self.emit(&format!(
                                "    store {} {}, {}* {}",
                                payload_llvm_ty, payload_val, payload_llvm_ty, alloc_reg
                            ));
                        }
                    }

                    let mut arm_result = None;
                    for stmt in &arm.body {
                        if let ASTNode::ReturnStatement(..) = stmt {
                            self.engine.compile_statement(stmt);
                            arm_result = None;
                            any_arm_terminated = true;
                            break;
                        } else {
                            arm_result = Some(self.compile(stmt));
                        }
                    }
                    if let Some(val_reg) = arm_result {
                        self.emit(&format!(
                            "    store {} {}, {}* {}",
                            llvm_result_type, val_reg, llvm_result_type, result_alloca
                        ));
                        self.emit(&format!("    br label %{}", merge_label));
                    }
                    current_label = fall_label;
                }

                self.emit(&format!("{}:", default_label));
                if has_wildcard {
                    let wildcard_arm = flattened_arms
                        .iter()
                        .find(|arm| matches!(arm.pattern, MatchPattern::Wildcard(_)))
                        .unwrap();
                    let mut arm_result = None;
                    for stmt in &wildcard_arm.body {
                        if let ASTNode::ReturnStatement(..) = stmt {
                            self.engine.compile_statement(stmt);
                            arm_result = None;
                            any_arm_terminated = true;
                            break;
                        } else {
                            arm_result = Some(self.compile(stmt));
                        }
                    }
                    if let Some(val_reg) = arm_result {
                        self.emit(&format!(
                            "    store {} {}, {}* {}",
                            llvm_result_type, val_reg, llvm_result_type, result_alloca
                        ));
                        self.emit(&format!("    br label %{}", merge_label));
                    }
                } else {
                    self.emit(&format!("    call void @vox_panic()"));
                    self.emit(&format!("    unreachable"));
                    any_arm_terminated = true;
                }

                let all_arms_terminate = flattened_arms.iter().all(|arm| {
                    if let Some(stmt) = arm.body.first() {
                        matches!(stmt, ASTNode::ReturnStatement(..))
                    } else {
                        false
                    }
                });

                if any_arm_terminated && all_arms_terminate {
                    self.engine.block_terminated = true;
                    return "0".to_string();
                }

                self.emit(&format!("{}:", merge_label));
                let result_reg = self.engine.next_register();
                self.emit(&format!(
                    "    {} = load {}, {}* {}",
                    result_reg, llvm_result_type, llvm_result_type, result_alloca
                ));
                result_reg
            }

            ASTNode::CastExpr {
                expr, target_type, ..
            } => {
                self.engine
                    .debug_log(&format!("compile CastExpr to {}", target_type));
                let raw_val = self.compile(expr);
                let target_llvm = self
                    .engine
                    .map_type(target_type, self.target == CodegenTarget::Device);
                let source_is_string = matches!(**expr, ASTNode::StringLiteral(..));

                let mut source_ty = self.expr_type(expr).unwrap_or_default();
                if let ASTNode::Identifier(name, _) = &**expr {
                    if let Some(resolved) = self.engine.get_resolved_type(name) {
                        source_ty = resolved;
                    }
                }

                self.engine.debug_log(&format!(
                    "CastExpr: source_ty='{}', target_llvm='{}', raw_val='{}'",
                    source_ty, target_llvm, raw_val
                ));

                if target_llvm == "i8*" && (source_ty == "&str" || source_ty.starts_with("&str")) {
                    let extracted = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = extractvalue {{ i8*, i64 }} {}, 0",
                        extracted, raw_val
                    ));
                    return extracted;
                }

                if source_ty == "f64" && target_llvm == "i32" {
                    let result_reg = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = fptosi double {} to i32",
                        result_reg, raw_val
                    ));
                    return result_reg;
                }
                if source_ty == "f64" && target_llvm == "i64" {
                    let result_reg = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = fptosi double {} to i64",
                        result_reg, raw_val
                    ));
                    return result_reg;
                }
                if source_ty == "i32" && target_llvm == "double" {
                    let result_reg = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = sitofp i32 {} to double",
                        result_reg, raw_val
                    ));
                    return result_reg;
                }

                let source_is_integer = Self::is_integer_type(&source_ty);
                if target_llvm.ends_with('*') && source_is_integer {
                    let int_ty = if source_ty == "i64" { "i64" } else { "i32" };
                    let result_reg = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = inttoptr {} {} to {}",
                        result_reg, int_ty, raw_val, target_llvm
                    ));
                    return result_reg;
                }

                match target_llvm.as_str() {
                    "i8" => {
                        let result_reg = self.engine.next_register();
                        self.emit(&format!("    {} = trunc i32 {} to i8", result_reg, raw_val));
                        result_reg
                    }
                    "i64" => {
                        let result_reg = self.engine.next_register();
                        self.emit(&format!("    {} = sext i32 {} to i64", result_reg, raw_val));
                        result_reg
                    }
                    "float" => {
                        let result_reg = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = sitofp i32 {} to float",
                            result_reg, raw_val
                        ));
                        result_reg
                    }
                    "double" => {
                        let result_reg = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = sitofp i32 {} to double",
                            result_reg, raw_val
                        ));
                        result_reg
                    }
                    _ => raw_val,
                }
            }

            ASTNode::BorrowExpr { mutable, expr, .. } => match &**expr {
                ASTNode::Identifier(name, _) => {
                    self.engine
                        .debug_log(&format!("compile BorrowExpr of '{}'", name));
                    if *mutable {
                        if let Some((_, _, _, is_mut)) = self.engine.variable_symbols.get(name) {
                            if !is_mut {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Cannot mutably borrow immutable variable '{}'",
                                        name
                                    ))
                                    .with_code("VX0404"),
                                );
                                self.engine.has_error = true;
                                return "0".to_string();
                            }
                        }
                    }
                    if let Some((_, alloc_reg, _, _)) =
                        self.engine.variable_symbols.get(name).cloned()
                    {
                        alloc_reg
                    } else {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot borrow unknown variable '{}'",
                                name
                            ))
                            .with_code("VX0405"),
                        );
                        self.engine.has_error = true;
                        "0".to_string()
                    }
                }
                _ => {
                    emit_diagnostic(
                        &Diagnostic::error("Borrow expression must be applied to an identifier")
                            .with_code("VX0406"),
                    );
                    self.engine.has_error = true;
                    "0".to_string()
                }
            },

            // ****************************************************************
            // FIXED DerefExpr – distinguishes between lvalue and rvalue contexts
            // ****************************************************************
            ASTNode::DerefExpr(inner, _) => {
                self.engine.debug_log("compile DerefExpr");
                // Save original lvalue flag
                let saved_lvalue = self.lvalue;
                // Compile inner as an lvalue to get the address of the pointer variable
                self.lvalue = true;
                let ptr_var_addr = self.compile(inner); // e.g., %y.addr_1 (i32**)
                self.lvalue = saved_lvalue;

                // Determine the LLVM type of the pointer variable (the stored pointer)
                let ptr_llvm_ty = if let ASTNode::Identifier(name, _) = &**inner {
                    if let Some((ty, _, _, _)) = self.engine.variable_symbols.get(name) {
                        // ty is the LLVM type of the variable, e.g., "ptr addrspace(1)" for &mut i32
                        ty.clone()
                    } else {
                        // Fallback: guess from inner Vox type
                        let inner_vox = self.engine.infer_vox_type(inner);
                        let pointee = inner_vox
                            .strip_prefix("&mut ")
                            .or_else(|| inner_vox.strip_prefix("& "))
                            .unwrap_or(&inner_vox);
                        let pointee_llvm = self
                            .engine
                            .map_type(pointee, self.target == CodegenTarget::Device);
                        format!("{}*", pointee_llvm)
                    }
                } else {
                    // For non-identifier inner (e.g., dereference of a call result), fallback
                    let inner_vox = self.engine.infer_vox_type(inner);
                    let pointee = inner_vox
                        .strip_prefix("&mut ")
                        .or_else(|| inner_vox.strip_prefix("& "))
                        .unwrap_or(&inner_vox);
                    let pointee_llvm = self
                        .engine
                        .map_type(pointee, self.target == CodegenTarget::Device);
                    format!("{}*", pointee_llvm)
                };

                // Load the actual pointer from the pointer variable's address
                let ptr_reg = self.engine.next_register();
                let ptr_addr_ty = if self.target == CodegenTarget::Device {
                    self.engine.device_ptr_type(&ptr_llvm_ty) // e.g., "ptr addrspace(5)"
                } else {
                    format!("{}*", ptr_llvm_ty) // e.g., "i32**"
                };
                self.emit(&format!(
                    "    {} = load {}, {} {}",
                    ptr_reg, ptr_llvm_ty, ptr_addr_ty, ptr_var_addr
                ));

                if saved_lvalue {
                    // lvalue: return the pointer to the pointee
                    ptr_reg
                } else {
                    // rvalue: load the final value from that pointer
                    let result_reg = self.engine.next_register();
                    let pointee_llvm = if let Some(inner_ty) = ptr_llvm_ty.strip_suffix('*') {
                        inner_ty.to_string()
                    } else if ptr_llvm_ty == "ptr addrspace(1)" {
                        // For an opaque pointer, we need to know the pointee type from Vox.
                        let inner_vox = self.expr_type(inner).unwrap_or_default();
                        let pointee_vox = inner_vox.strip_prefix("&mut ").unwrap_or(&inner_vox);
                        self.engine.map_type(pointee_vox, true)
                    } else {
                        "i32".to_string()
                    };
                    // The pointer type to use for the load is exactly the type of the pointer variable.
                    // That type is stored in self.engine.variable_symbols for identifiers.
                    let ptr_ty = if let ASTNode::Identifier(name, _) = &**inner {
                        if let Some((ty, _, _, _)) = self.engine.variable_symbols.get(name) {
                            ty.clone()
                        } else {
                            self.engine.device_ptr_type(&pointee_llvm)
                        }
                    } else {
                        self.engine.device_ptr_type(&pointee_llvm)
                    };
                    self.emit(&format!(
                        "    {} = load {}, {} {}",
                        result_reg, pointee_llvm, ptr_ty, ptr_reg
                    ));
                    result_reg
                }
            }

            ASTNode::CallExpr { callee, args, span } => {
                self.engine
                    .debug_log(&format!("compile CallExpr callee='{}'", callee));
                let mut actual_callee = callee.clone();
                self.engine.debug_log(&format!(
                    "CallExpr: processing callee='{}' (original), is_generic={}",
                    callee,
                    self.engine.generic_functions.contains_key(callee)
                ));

                if actual_callee.contains("::") {
                    let parts: Vec<&str> = actual_callee.split("::").collect();
                    if parts.len() == 2 {
                        let enum_name = parts[0];
                        let variant_name = parts[1];
                        let stripped_enum = enum_name.split('<').next().unwrap_or(enum_name);
                        if let Some(variants) = self.engine.enum_variants.get(stripped_enum) {
                            if let Some(&discriminant) = variants.get(variant_name) {
                                let has_payload = args.len() == 1;
                                let concrete_ty = if let Some(expected) = &self.expected_type {
                                    if let Some((base, _)) = parse_generic_type(expected) {
                                        if base == stripped_enum {
                                            Some(self.engine.map_type(
                                                expected,
                                                self.target == CodegenTarget::Device,
                                            ))
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                };
                                if has_payload {
                                    let payload_val = self.compile(&args[0]);
                                    let payload_vox_ty = self.engine.infer_vox_type(&args[0]);
                                    let payload_llvm_ty = self.engine.map_type(
                                        &payload_vox_ty,
                                        self.target == CodegenTarget::Device,
                                    );
                                    let disc_field_idx = 0;
                                    let payload_field_idx = if stripped_enum == "Result" {
                                        if variant_name == "Err" { 2 } else { 1 }
                                    } else {
                                        1
                                    };
                                    let opt_ty =
                                        concrete_ty.unwrap_or_else(|| "{ i32, i32 }".to_string());
                                    let alloca_reg = self.engine.next_register();
                                    self.emit(&format!("    {} = alloca {}", alloca_reg, opt_ty));
                                    let disc_ptr = self.engine.next_register();
                                    self.emit(&format!(
                                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                                        disc_ptr, opt_ty, opt_ty, alloca_reg, disc_field_idx
                                    ));
                                    self.emit(&format!(
                                        "    store i32 {}, i32* {}",
                                        discriminant, disc_ptr
                                    ));
                                    let payload_ptr = self.engine.next_register();
                                    self.emit(&format!(
                                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                                        payload_ptr, opt_ty, opt_ty, alloca_reg, payload_field_idx
                                    ));
                                    self.emit(&format!(
                                        "    store {} {}, {}* {}",
                                        payload_llvm_ty, payload_val, payload_llvm_ty, payload_ptr
                                    ));
                                    let result_reg = self.engine.next_register();
                                    self.emit(&format!(
                                        "    {} = load {}, {}* {}",
                                        result_reg, opt_ty, opt_ty, alloca_reg
                                    ));
                                    return result_reg;
                                } else {
                                    if let Some(concrete_ty) = concrete_ty {
                                        return "zeroinitializer".to_string();
                                    } else {
                                        if discriminant == 0 {
                                            return "zeroinitializer".to_string();
                                        } else {
                                            return format!("{{ i32 {}, i32 0 }}", discriminant);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if !callee.contains("::") && self.engine.generic_functions.contains_key(callee) {
                    let (generic_params, param_tys, return_ty) =
                        self.engine.generic_functions.get(callee).unwrap().clone();

                    self.engine.debug_log(&format!(
                        "Generic function '{}' with params {:?}, return_ty='{}'",
                        callee, generic_params, return_ty
                    ));

                    let mut subst = HashMap::new();

                    for (i, param_ty) in param_tys.iter().enumerate() {
                        if i >= args.len() {
                            break;
                        }
                        let arg_ty = self
                            .expr_type(&args[i])
                            .unwrap_or_else(|| "i32".to_string());
                        self.engine.debug_log(&format!(
                            "  param[{}] type='{}', arg type='{}'",
                            i, param_ty, arg_ty
                        ));
                        for gp in &generic_params {
                            if param_ty.contains(gp) {
                                if let Some(concrete) =
                                    self.engine.unify_generic_parameter(gp, param_ty, &arg_ty)
                                {
                                    self.engine.debug_log(&format!(
                                        "  unify: {} = {} (from arg)",
                                        gp, concrete
                                    ));
                                    subst.insert(gp.clone(), concrete);
                                } else {
                                    subst.insert(gp.clone(), arg_ty.clone());
                                    self.engine.debug_log(&format!(
                                        "  fallback: {} = {} (from arg type)",
                                        gp, arg_ty
                                    ));
                                }
                            }
                        }
                    }

                    if let Some(expected_ret) = &self.expected_type {
                        self.engine
                            .debug_log(&format!("Expected return type: '{}'", expected_ret));
                        let base_ret = CodegenEngine::strip_generic_args(&return_ty);
                        let base_exp = CodegenEngine::strip_generic_args(expected_ret);
                        if base_ret == base_exp
                            && return_ty.contains('<')
                            && expected_ret.contains('<')
                        {
                            if let (Some((_, ret_args)), Some((_, exp_args))) = (
                                parse_generic_type(&return_ty),
                                parse_generic_type(expected_ret),
                            ) {
                                for (i, rarg) in ret_args.iter().enumerate() {
                                    if i < exp_args.len() && generic_params.contains(rarg) {
                                        self.engine.debug_log(&format!(
                                            "  from expected return: {} = {}",
                                            rarg, exp_args[i]
                                        ));
                                        subst.insert(rarg.clone(), exp_args[i].clone());
                                    }
                                }
                            }
                        }
                    }

                    if !subst.is_empty() {
                        let mut key_parts = vec![callee.clone()];
                        for gp in &generic_params {
                            if let Some(conc) = subst.get(gp) {
                                key_parts.push(conc.clone());
                            } else {
                                key_parts.push("?".to_string());
                            }
                        }
                        let monomorphised_name = key_parts
                            .iter()
                            .map(|p| crate::codegen::generic::sanitize_type_name(p))
                            .collect::<Vec<_>>()
                            .join("_");
                        self.engine.debug_log(&format!(
                            "Monomorphised name candidate: '{}' (subst={:?})",
                            monomorphised_name, subst
                        ));

                        let contains_before = self
                            .engine
                            .function_return_types
                            .contains_key(&monomorphised_name);
                        self.engine.debug_log(&format!(
                            "function_return_types contains '{}' before generation? {}",
                            monomorphised_name, contains_before
                        ));

                        if !contains_before {
                            self.engine.generate_monomorphised_function(
                                callee,
                                &monomorphised_name,
                                &subst,
                            );
                            let contains_after = self
                                .engine
                                .function_return_types
                                .contains_key(&monomorphised_name);
                            self.engine.debug_log(&format!(
                                "After generation, contains '{}'? {}",
                                monomorphised_name, contains_after
                            ));
                        }
                        actual_callee = monomorphised_name;
                    } else {
                        self.engine
                            .debug_log("No substitutions, keeping original callee");
                    }
                }

                match actual_callee.as_str() {
                    "None" => {
                        self.engine.debug_log("CallExpr: handling None");
                        if let Some(expected) = &self.expected_type {
                            if let Some((base_name, type_args)) = parse_generic_type(expected) {
                                if base_name == "Option" && type_args.len() == 1 {
                                    let opt_ty = self
                                        .engine
                                        .map_type(expected, self.target == CodegenTarget::Device);
                                    self.engine.debug_log(&format!(
                                        "None with expected Option type: concrete type = {}",
                                        opt_ty
                                    ));
                                    let reg = self.engine.next_register();
                                    self.emit(&format!("    {} = alloca {}", reg, opt_ty));
                                    self.emit(&format!(
                                        "    store {} zeroinitializer, {}* {}",
                                        opt_ty, opt_ty, reg
                                    ));
                                    let loaded = self.engine.next_register();
                                    self.emit(&format!(
                                        "    {} = load {}, {}* {}",
                                        loaded, opt_ty, opt_ty, reg
                                    ));
                                    return loaded;
                                }
                            }
                        }
                        self.engine
                            .debug_log("None with no expected type -> zeroinitializer (anonymous)");
                        return "zeroinitializer".to_string();
                    }
                    "Some" => {
                        self.engine.debug_log("CallExpr: handling Some");
                        if args.len() != 1 {
                            emit_diagnostic(
                                &Diagnostic::error("`Some` expects exactly one argument")
                                    .with_code("VX0504")
                                    .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                        let payload_val = self.compile(&args[0]);
                        let payload_vox_ty = self.engine.infer_vox_type(&args[0]);
                        let opt_vox_ty = format!("Option<{}>", payload_vox_ty);
                        let opt_ty = self
                            .engine
                            .map_type(&opt_vox_ty, self.target == CodegenTarget::Device);
                        let payload_llvm_ty = self
                            .engine
                            .map_type(&payload_vox_ty, self.target == CodegenTarget::Device);

                        let opt_alloca = self.engine.next_register();
                        self.emit(&format!("    {} = alloca {}", opt_alloca, opt_ty));

                        let disc_ptr = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                            disc_ptr, opt_ty, opt_ty, opt_alloca
                        ));
                        self.emit(&format!("    store i32 1, i32* {}", disc_ptr));

                        let payload_ptr = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 1",
                            payload_ptr, opt_ty, opt_ty, opt_alloca
                        ));
                        self.emit(&format!(
                            "    store {} {}, {}* {}",
                            payload_llvm_ty, payload_val, payload_llvm_ty, payload_ptr
                        ));

                        let result_reg = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = load {}, {}* {}",
                            result_reg, opt_ty, opt_ty, opt_alloca
                        ));
                        return result_reg;
                    }
                    "Ok" => {
                        self.engine.debug_log("CallExpr: handling Ok");
                        if args.len() != 1 {
                            emit_diagnostic(
                                &Diagnostic::error("`Ok` expects exactly one argument")
                                    .with_code("VX0606")
                                    .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                        let payload_val = self.compile(&args[0]);
                        let payload_vox_ty = self.engine.infer_vox_type(&args[0]);
                        let result_vox_ty = format!("Result<{}, ?>", payload_vox_ty);
                        let result_ty = self
                            .engine
                            .map_type(&result_vox_ty, self.target == CodegenTarget::Device);
                        let payload_llvm_ty = self
                            .engine
                            .map_type(&payload_vox_ty, self.target == CodegenTarget::Device);

                        let result_alloca = self.engine.next_register();
                        self.emit(&format!("    {} = alloca {}", result_alloca, result_ty));

                        let disc_ptr = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                            disc_ptr, result_ty, result_ty, result_alloca
                        ));
                        self.emit(&format!("    store i32 0, i32* {}", disc_ptr));

                        let payload_ptr = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 1",
                            payload_ptr, result_ty, result_ty, result_alloca
                        ));
                        self.emit(&format!(
                            "    store {} {}, {}* {}",
                            payload_llvm_ty, payload_val, payload_llvm_ty, payload_ptr
                        ));

                        let result_reg = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = load {}, {}* {}",
                            result_reg, result_ty, result_ty, result_alloca
                        ));
                        return result_reg;
                    }
                    "Err" => {
                        self.engine.debug_log("CallExpr: handling Err");
                        if args.len() != 1 {
                            emit_diagnostic(
                                &Diagnostic::error("`Err` expects exactly one argument")
                                    .with_code("VX0608")
                                    .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                        let payload_val = self.compile(&args[0]);
                        let payload_vox_ty = self.engine.infer_vox_type(&args[0]);
                        let result_vox_ty = format!("Result<?, {}>", payload_vox_ty);
                        let result_ty = self
                            .engine
                            .map_type(&result_vox_ty, self.target == CodegenTarget::Device);
                        let payload_llvm_ty = self
                            .engine
                            .map_type(&payload_vox_ty, self.target == CodegenTarget::Device);

                        let result_alloca = self.engine.next_register();
                        self.emit(&format!("    {} = alloca {}", result_alloca, result_ty));

                        let disc_ptr = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                            disc_ptr, result_ty, result_ty, result_alloca
                        ));
                        self.emit(&format!("    store i32 1, i32* {}", disc_ptr));

                        let payload_ptr = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 2",
                            payload_ptr, result_ty, result_ty, result_alloca
                        ));
                        self.emit(&format!(
                            "    store {} {}, {}* {}",
                            payload_llvm_ty, payload_val, payload_llvm_ty, payload_ptr
                        ));

                        let result_reg = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = load {}, {}* {}",
                            result_reg, result_ty, result_ty, result_alloca
                        ));
                        return result_reg;
                    }
                    _ => {}
                }

                if actual_callee == "Vec::new" {
                    self.engine.debug_log("CallExpr: Vec::new");
                    if !args.is_empty() {
                        emit_diagnostic(
                            &Diagnostic::error("`Vec::new` expects no arguments")
                                .with_code("VX0501")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let elem_vox_ty = if let Some(expected) = &self.expected_type {
                        if let Some((base_name, type_args)) = parse_generic_type(expected) {
                            if base_name == "Vec" && type_args.len() == 1 {
                                type_args[0].clone()
                            } else {
                                "i32".to_string()
                            }
                        } else {
                            "i32".to_string()
                        }
                    } else {
                        self.engine
                            .debug_log("Vec::new without expected type, defaulting to i32");
                        "i32".to_string()
                    };
                    let elem_size = self.engine.size_of_type(&elem_vox_ty);
                    if elem_size == 0 {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Unknown element size for type '{}'",
                                elem_vox_ty
                            ))
                            .with_code("VX0504"),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let handle = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call i8* @vox_vec_new(i64 {})",
                        handle, elem_size
                    ));
                    return handle;
                }

                if actual_callee == "HashMap::new" {
                    self.engine.debug_log("CallExpr: HashMap::new");
                    if !args.is_empty() {
                        emit_diagnostic(
                            &Diagnostic::error("`HashMap::new` expects no arguments")
                                .with_code("VX0601")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let (k_ty, v_ty) = if let Some(expected) = &self.expected_type {
                        if let Some((base_name, type_args)) = parse_generic_type(expected) {
                            if base_name == "HashMap" && type_args.len() == 2 {
                                (type_args[0].clone(), type_args[1].clone())
                            } else {
                                ("i32".to_string(), "i32".to_string())
                            }
                        } else {
                            ("i32".to_string(), "i32".to_string())
                        }
                    } else {
                        ("i32".to_string(), "i32".to_string())
                    };
                    let key_size = self.engine.size_of_type(&k_ty);
                    let value_size = self.engine.size_of_type(&v_ty);
                    let handle = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call i8* @vox_hashmap_new(i64 {}, i64 {})",
                        handle, key_size, value_size
                    ));
                    return handle;
                }

                if actual_callee == "insert" {
                    self.engine.debug_log("CallExpr: insert");
                    if args.len() != 3 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`insert` expects exactly 3 arguments: map, key, value",
                            )
                            .with_code("VX0602")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let map_ty = self.expr_type(&args[0]).unwrap_or_default();
                    let (k_ty, v_ty) =
                        if let Some((base_name, type_args)) = parse_generic_type(&map_ty) {
                            if base_name == "HashMap" && type_args.len() == 2 {
                                (type_args[0].clone(), type_args[1].clone())
                            } else {
                                ("i32".to_string(), "i32".to_string())
                            }
                        } else {
                            ("i32".to_string(), "i32".to_string())
                        };

                    let map_ptr = self.compile(&args[0]);
                    let key_reg = self.compile(&args[1]);
                    let value_reg = self.compile(&args[2]);

                    let key_llvm_ty = self
                        .engine
                        .map_type(&k_ty, self.target == CodegenTarget::Device);
                    let value_llvm_ty = self
                        .engine
                        .map_type(&v_ty, self.target == CodegenTarget::Device);

                    let key_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", key_tmp, key_llvm_ty));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        key_llvm_ty, key_reg, key_llvm_ty, key_tmp
                    ));
                    let value_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", value_tmp, value_llvm_ty));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        value_llvm_ty, value_reg, value_llvm_ty, value_tmp
                    ));

                    let key_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        key_i8, key_llvm_ty, key_tmp
                    ));
                    let value_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        value_i8, value_llvm_ty, value_tmp
                    ));

                    self.emit(&format!(
                        "    call void @vox_hashmap_insert(i8* {}, i8* {}, i8* {})",
                        map_ptr, key_i8, value_i8
                    ));
                    return "0".to_string();
                }

                if actual_callee == "get" {
                    self.engine.debug_log("CallExpr: get");
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error("`get` expects exactly 2 arguments: map, key")
                                .with_code("VX0603")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let map_ty = self.expr_type(&args[0]).unwrap_or_default();
                    let (k_ty, v_ty) =
                        if let Some((base_name, type_args)) = parse_generic_type(&map_ty) {
                            if base_name == "HashMap" && type_args.len() == 2 {
                                (type_args[0].clone(), type_args[1].clone())
                            } else {
                                ("i32".to_string(), "i32".to_string())
                            }
                        } else {
                            ("i32".to_string(), "i32".to_string())
                        };

                    let map_ptr = self.compile(&args[0]);
                    let key_reg = self.compile(&args[1]);

                    let key_llvm_ty = self
                        .engine
                        .map_type(&k_ty, self.target == CodegenTarget::Device);
                    let value_llvm_ty = self
                        .engine
                        .map_type(&v_ty, self.target == CodegenTarget::Device);

                    let key_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", key_tmp, key_llvm_ty));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        key_llvm_ty, key_reg, key_llvm_ty, key_tmp
                    ));
                    let key_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        key_i8, key_llvm_ty, key_tmp
                    ));

                    let out_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", out_tmp, value_llvm_ty));
                    let out_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        out_i8, value_llvm_ty, out_tmp
                    ));
                    let flag = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call i32 @vox_hashmap_get(i8* {}, i8* {}, i8* {})",
                        flag, map_ptr, key_i8, out_i8
                    ));

                    let opt_ty = self.engine.map_type(
                        &format!("Option<{}>", v_ty),
                        self.target == CodegenTarget::Device,
                    );
                    let some_discriminant = 1;
                    let none_discriminant = 0;
                    let is_some_label = self.engine.next_block();
                    let is_none_label = self.engine.next_block();
                    let merge_label = self.engine.next_block();

                    let flag_i1 = self.engine.next_register();
                    self.emit(&format!("    {} = icmp eq i32 {}, 0", flag_i1, flag));
                    self.emit(&format!(
                        "    br i1 {}, label %{}, label %{}",
                        flag_i1, is_none_label, is_some_label
                    ));

                    self.emit(&format!("{}:", is_none_label));
                    let none_enum = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", none_enum, opt_ty));
                    let disc_none = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                        disc_none, opt_ty, opt_ty, none_enum
                    ));
                    self.emit(&format!(
                        "    store i32 {}, i32* {}",
                        none_discriminant, disc_none
                    ));
                    let none_val = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        none_val, opt_ty, opt_ty, none_enum
                    ));
                    self.emit(&format!("    br label %{}", merge_label));

                    self.emit(&format!("{}:", is_some_label));
                    let loaded_val = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        loaded_val, value_llvm_ty, value_llvm_ty, out_tmp
                    ));
                    let some_enum = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", some_enum, opt_ty));
                    let disc_some = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                        disc_some, opt_ty, opt_ty, some_enum
                    ));
                    self.emit(&format!(
                        "    store i32 {}, i32* {}",
                        some_discriminant, disc_some
                    ));
                    let payload_field = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 1",
                        payload_field, opt_ty, opt_ty, some_enum
                    ));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        value_llvm_ty, loaded_val, value_llvm_ty, payload_field
                    ));
                    let some_val = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        some_val, opt_ty, opt_ty, some_enum
                    ));
                    self.emit(&format!("    br label %{}", merge_label));

                    self.emit(&format!("{}:", merge_label));
                    let phi = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = phi {} [ {}, %{} ], [ {}, %{} ]",
                        phi, opt_ty, none_val, is_none_label, some_val, is_some_label
                    ));
                    return phi;
                }

                if actual_callee == "contains_key" {
                    self.engine.debug_log("CallExpr: contains_key");
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`contains_key` expects exactly 2 arguments: map, key",
                            )
                            .with_code("VX0604")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let map_ty = self.expr_type(&args[0]).unwrap_or_default();
                    let (k_ty, _) =
                        if let Some((base_name, type_args)) = parse_generic_type(&map_ty) {
                            if base_name == "HashMap" && type_args.len() == 2 {
                                (type_args[0].clone(), type_args[1].clone())
                            } else {
                                ("i32".to_string(), "i32".to_string())
                            }
                        } else {
                            ("i32".to_string(), "i32".to_string())
                        };

                    let map_ptr = self.compile(&args[0]);
                    let key_reg = self.compile(&args[1]);

                    let key_llvm_ty = self
                        .engine
                        .map_type(&k_ty, self.target == CodegenTarget::Device);

                    let key_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", key_tmp, key_llvm_ty));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        key_llvm_ty, key_reg, key_llvm_ty, key_tmp
                    ));
                    let key_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        key_i8, key_llvm_ty, key_tmp
                    ));

                    let flag = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call i32 @vox_hashmap_contains_key(i8* {}, i8* {})",
                        flag, map_ptr, key_i8
                    ));
                    return flag;
                }

                if actual_callee == "remove" {
                    self.engine.debug_log("CallExpr: remove");
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error("`remove` expects exactly 2 arguments: map, key")
                                .with_code("VX0605")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let map_ty = self.expr_type(&args[0]).unwrap_or_default();
                    let (k_ty, v_ty) =
                        if let Some((base_name, type_args)) = parse_generic_type(&map_ty) {
                            if base_name == "HashMap" && type_args.len() == 2 {
                                (type_args[0].clone(), type_args[1].clone())
                            } else {
                                ("i32".to_string(), "i32".to_string())
                            }
                        } else {
                            ("i32".to_string(), "i32".to_string())
                        };

                    let map_ptr = self.compile(&args[0]);
                    let key_reg = self.compile(&args[1]);

                    let key_llvm_ty = self
                        .engine
                        .map_type(&k_ty, self.target == CodegenTarget::Device);
                    let value_llvm_ty = self
                        .engine
                        .map_type(&v_ty, self.target == CodegenTarget::Device);

                    let key_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", key_tmp, key_llvm_ty));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        key_llvm_ty, key_reg, key_llvm_ty, key_tmp
                    ));
                    let key_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        key_i8, key_llvm_ty, key_tmp
                    ));

                    let out_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", out_tmp, value_llvm_ty));
                    let out_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        out_i8, value_llvm_ty, out_tmp
                    ));
                    let flag = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call i32 @vox_hashmap_remove(i8* {}, i8* {}, i8* {})",
                        flag, map_ptr, key_i8, out_i8
                    ));

                    let opt_ty = self.engine.map_type(
                        &format!("Option<{}>", v_ty),
                        self.target == CodegenTarget::Device,
                    );
                    let some_discriminant = 1;
                    let none_discriminant = 0;
                    let is_some_label = self.engine.next_block();
                    let is_none_label = self.engine.next_block();
                    let merge_label = self.engine.next_block();

                    let flag_i1 = self.engine.next_register();
                    self.emit(&format!("    {} = icmp eq i32 {}, 0", flag_i1, flag));
                    self.emit(&format!(
                        "    br i1 {}, label %{}, label %{}",
                        flag_i1, is_none_label, is_some_label
                    ));

                    self.emit(&format!("{}:", is_none_label));
                    let none_enum = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", none_enum, opt_ty));
                    let disc_none = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                        disc_none, opt_ty, opt_ty, none_enum
                    ));
                    self.emit(&format!(
                        "    store i32 {}, i32* {}",
                        none_discriminant, disc_none
                    ));
                    let none_val = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        none_val, opt_ty, opt_ty, none_enum
                    ));
                    self.emit(&format!("    br label %{}", merge_label));

                    self.emit(&format!("{}:", is_some_label));
                    let loaded_val = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        loaded_val, value_llvm_ty, value_llvm_ty, out_tmp
                    ));
                    let some_enum = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", some_enum, opt_ty));
                    let disc_some = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                        disc_some, opt_ty, opt_ty, some_enum
                    ));
                    self.emit(&format!(
                        "    store i32 {}, i32* {}",
                        some_discriminant, disc_some
                    ));
                    let payload_field = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 1",
                        payload_field, opt_ty, opt_ty, some_enum
                    ));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        value_llvm_ty, loaded_val, value_llvm_ty, payload_field
                    ));
                    let some_val = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        some_val, opt_ty, opt_ty, some_enum
                    ));
                    self.emit(&format!("    br label %{}", merge_label));

                    self.emit(&format!("{}:", merge_label));
                    let phi = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = phi {} [ {}, %{} ], [ {}, %{} ]",
                        phi, opt_ty, none_val, is_none_label, some_val, is_some_label
                    ));
                    return phi;
                }

                if actual_callee == "len" {
                    self.engine.debug_log("CallExpr: len");
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`len` expects exactly 1 argument: container or string",
                            )
                            .with_code("VX0436")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let arg_ty_raw = self.expr_type(&args[0]).unwrap_or_default();
                    let arg_ty = self.engine.expand_type_aliases(&arg_ty_raw);
                    if let Some((base_name, type_args)) = parse_generic_type(&arg_ty) {
                        if base_name == "Vec" && type_args.len() == 1 {
                            let saved_lvalue = self.lvalue;
                            self.lvalue = false;
                            let handle = self.compile(&args[0]);
                            self.lvalue = saved_lvalue;
                            let len_i64 = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = call i64 @vox_vec_len(i8* {})",
                                len_i64, handle
                            ));
                            let len_i32 = self.engine.next_register();
                            self.emit(&format!("    {} = trunc i64 {} to i32", len_i32, len_i64));
                            return len_i32;
                        } else if base_name == "HashMap" {
                            let map_ptr = self.compile(&args[0]);
                            let len_i64 = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = call i64 @vox_hashmap_len(i8* {})",
                                len_i64, map_ptr
                            ));
                            let len_i32 = self.engine.next_register();
                            self.emit(&format!("    {} = trunc i64 {} to i32", len_i32, len_i64));
                            return len_i32;
                        }
                    }
                    let arg_val = self.compile(&args[0]);
                    if arg_ty == "&str" {
                        let len_i64 = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = extractvalue {{ i8*, i64 }} {}, 1",
                            len_i64, arg_val
                        ));
                        let result_i32 = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = trunc i64 {} to i32",
                            result_i32, len_i64
                        ));
                        return result_i32;
                    } else {
                        let saved_lvalue = self.lvalue;
                        self.lvalue = true;
                        let arg_ptr = self.compile(&args[0]);
                        self.lvalue = saved_lvalue;
                        let arr_i8 = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = bitcast {{ i8*, i64, i64 }}* {} to i8*",
                            arr_i8, arg_ptr
                        ));
                        let len_val = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = call i64 @vox_array_len(i8* {})",
                            len_val, arr_i8
                        ));
                        let result_i32 = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = trunc i64 {} to i32",
                            result_i32, len_val
                        ));
                        return result_i32;
                    }
                }

                if actual_callee == "assert" {
                    self.engine.debug_log("CallExpr: assert");
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`assert` expects exactly 2 arguments: condition and message",
                            )
                            .with_code("VX9998")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let cond_reg = self.compile(&args[0]);
                    let _msg_reg = self.compile(&args[1]);
                    let cond_i1 = self.engine.next_register();
                    self.emit(&format!("    {} = icmp eq i32 {}, 0", cond_i1, cond_reg));
                    let panic_label = self.engine.next_block();
                    let cont_label = self.engine.next_block();
                    self.emit(&format!(
                        "    br i1 {}, label %{}, label %{}",
                        cond_i1, panic_label, cont_label
                    ));
                    self.emit(&format!("{}:", panic_label));
                    self.emit(&format!("    call void @vox_panic()"));
                    self.emit(&format!("    unreachable"));
                    self.emit(&format!("{}:", cont_label));
                    return "0".to_string();
                }

                if actual_callee == "exit" {
                    self.engine.debug_log("CallExpr: exit");
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error("`exit` expects exactly one argument")
                                .with_code("VX0460")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let arg_reg = self.compile(&args[0]);
                    self.emit(&format!("    call void @exit(i32 {})", arg_reg));
                    self.emit(&format!("    unreachable"));
                    self.engine.block_terminated = true;
                    return "0".to_string();
                }

                if actual_callee == "String::new" {
                    self.engine.debug_log("CallExpr: String::new");
                    if !args.is_empty() {
                        emit_diagnostic(
                            &Diagnostic::error("String::new expects no arguments")
                                .with_code("VX0446")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let result_ptr = self.engine.next_register();
                    self.emit(&format!("    {} = call i8* @vox_string_new()", result_ptr));
                    let struct_ptr = self.engine.next_register();
                    let struct_ty = self
                        .engine
                        .map_type("String", self.target == CodegenTarget::Device);
                    self.emit(&format!(
                        "    {} = bitcast i8* {} to {}*",
                        struct_ptr, result_ptr, struct_ty
                    ));
                    let struct_val = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        struct_val, struct_ty, struct_ty, struct_ptr
                    ));
                    return struct_val;
                }

                if actual_callee == "String::from" {
                    self.engine.debug_log("CallExpr: String::from");
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error("String::from expects exactly one argument")
                                .with_code("VX0447")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let arg_val = self.compile(&args[0]);
                    let data = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = extractvalue {{ i8*, i64 }} {}, 0",
                        data, arg_val
                    ));
                    let len = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = extractvalue {{ i8*, i64 }} {}, 1",
                        len, arg_val
                    ));
                    let result_ptr = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call i8* @vox_string_from(i8* {}, i64 {})",
                        result_ptr, data, len
                    ));
                    let struct_ptr = self.engine.next_register();
                    let struct_ty = self
                        .engine
                        .map_type("String", self.target == CodegenTarget::Device);
                    self.emit(&format!(
                        "    {} = bitcast i8* {} to {}*",
                        struct_ptr, result_ptr, struct_ty
                    ));
                    let struct_val = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        struct_val, struct_ty, struct_ty, struct_ptr
                    ));
                    return struct_val;
                }

                if actual_callee == "as_str" {
                    self.engine.debug_log("CallExpr: as_str");
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error("as_str expects exactly one argument")
                                .with_code("VX0448")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let saved_lvalue = self.lvalue;
                    self.lvalue = true;
                    let arg_ptr = self.compile(&args[0]);
                    self.lvalue = saved_lvalue;
                    let data_ptr = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {{ i8*, i64, i64 }}, {{ i8*, i64, i64 }}* {}, i32 0, i32 0",
                        data_ptr, arg_ptr
                    ));
                    let loaded_data = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load i8*, i8** {}",
                        loaded_data, data_ptr
                    ));
                    let len_ptr = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {{ i8*, i64, i64 }}, {{ i8*, i64, i64 }}* {}, i32 0, i32 1",
                        len_ptr, arg_ptr
                    ));
                    let loaded_len = self.engine.next_register();
                    self.emit(&format!("    {} = load i64, i64* {}", loaded_len, len_ptr));
                    let fat_alloca = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {{ i8*, i64 }}", fat_alloca));
                    let field0 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {{ i8*, i64 }}, {{ i8*, i64 }}* {}, i32 0, i32 0",
                        field0, fat_alloca
                    ));
                    self.emit(&format!("    store i8* {}, i8** {}", loaded_data, field0));
                    let field1 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = getelementptr inbounds {{ i8*, i64 }}, {{ i8*, i64 }}* {}, i32 0, i32 1",
                        field1, fat_alloca
                    ));
                    self.emit(&format!("    store i64 {}, i64* {}", loaded_len, field1));
                    let result = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {{ i8*, i64 }}, {{ i8*, i64 }}* {}",
                        result, fat_alloca
                    ));
                    return result;
                }

                if actual_callee == "push_str" {
                    self.engine.debug_log("CallExpr: push_str");
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "push_str expects exactly two arguments: &mut String and &str",
                            )
                            .with_code("VX0449")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let saved_lvalue = self.lvalue;
                    self.lvalue = true;
                    let string_ptr = self.compile(&args[0]);
                    self.lvalue = saved_lvalue;
                    let slice_val = self.compile(&args[1]);
                    let data = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = extractvalue {{ i8*, i64 }} {}, 0",
                        data, slice_val
                    ));
                    let len = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = extractvalue {{ i8*, i64 }} {}, 1",
                        len, slice_val
                    ));
                    let i8_ptr = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {{ i8*, i64, i64 }}* {} to i8*",
                        i8_ptr, string_ptr
                    ));
                    self.emit(&format!(
                        "    call void @vox_string_append_bytes(i8* {}, i8* {}, i64 {})",
                        i8_ptr, data, len
                    ));
                    return "0".to_string();
                }

                if callee == "as_ptr" {
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`as_ptr` expects exactly one argument (a &str or String)",
                            )
                            .with_code("VX0291")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let arg_ty = self.expr_type(&args[0]).unwrap_or_default();
                    let is_string = arg_ty == "&str" || arg_ty == "String";
                    if !is_string {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "`as_ptr` can only be called on `&str` or `String`, got `{}`",
                                arg_ty
                            ))
                            .with_code("VX0292")
                            .with_span(args[0].span()),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }

                    let arg_val = self.compile(&args[0]);
                    let data_ptr = self.engine.next_register();
                    if arg_ty == "&str" {
                        self.emit(&format!(
                            "    {} = extractvalue {{ i8*, i64 }} {}, 0",
                            data_ptr, arg_val
                        ));
                    } else {
                        self.emit(&format!(
                            "    {} = extractvalue {{ i8*, i64, i64 }} {}, 0",
                            data_ptr, arg_val
                        ));
                    }
                    return data_ptr;
                }

                let mangled_callee = if actual_callee.contains("::") {
                    actual_callee.replace("::", "_")
                } else {
                    actual_callee.clone()
                };
                self.engine.debug_log(&format!(
                    "After mangling: '{}' -> '{}'",
                    actual_callee, mangled_callee
                ));

                if self.engine.struct_fields.contains_key(&mangled_callee) {
                    self.engine
                        .debug_log(&format!("CallExpr: struct constructor {}", mangled_callee));
                    let struct_name = &mangled_callee;
                    let fields = self.engine.struct_fields[struct_name].clone();
                    if args.len() != fields.len() {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Struct '{}' expects {} fields, got {} arguments.",
                                struct_name,
                                fields.len(),
                                args.len()
                            ))
                            .with_code("VX0272"),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let struct_ty = self
                        .engine
                        .map_type(struct_name, self.target == CodegenTarget::Device);
                    let alloca_reg = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", alloca_reg, struct_ty));
                    for (i, arg) in args.iter().enumerate() {
                        let mut arg_val = self.compile(arg);
                        if let ASTNode::Identifier(name, _) = arg {
                            if let Some((ty_str, alloc_reg, _, _)) =
                                self.engine.variable_symbols.get(name)
                            {
                                let ty_str = ty_str.clone();
                                let alloc_reg = alloc_reg.clone();
                                if ty_str.starts_with('%') {
                                    let loaded = self.engine.next_register();
                                    self.emit(&format!(
                                        "    {} = load {}, {}* {}",
                                        loaded, ty_str, ty_str, alloc_reg
                                    ));
                                    arg_val = loaded;
                                }
                            }
                        }
                        let (_, field_ty) = &fields[i];
                        let llvm_field_ty = self
                            .engine
                            .map_type(field_ty, self.target == CodegenTarget::Device);
                        let gep_reg = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                            gep_reg, struct_ty, struct_ty, alloca_reg, i
                        ));
                        self.emit(&format!(
                            "    store {} {}, {}* {}",
                            llvm_field_ty, arg_val, llvm_field_ty, gep_reg
                        ));
                    }
                    return alloca_reg;
                }

                if mangled_callee == "copy" {
                    self.engine.debug_log("CallExpr: copy");
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error("copy expects one argument").with_code("VX0407"),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    return self.compile(&args[0]);
                }

                if mangled_callee == "push" {
                    self.engine.debug_log("CallExpr: push");
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "push expects exactly 2 arguments: container and value",
                            )
                            .with_code("VX0432")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }

                    let container_expr = &args[0];
                    let container_ty_raw = self.expr_type(container_expr).unwrap_or_default();
                    let container_ty = self.engine.expand_type_aliases(&container_ty_raw);

                    if let Some((base_name, type_args)) = parse_generic_type(&container_ty) {
                        if base_name == "Vec" && type_args.len() == 1 {
                            let elem_vox_ty = &type_args[0];
                            let elem_llvm_ty = self
                                .engine
                                .map_type(elem_vox_ty, self.target == CodegenTarget::Device);
                            let elem_size = self.engine.size_of_type(elem_vox_ty);
                            if elem_size == 0 {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Unknown element size for type '{}'",
                                        elem_vox_ty
                                    ))
                                    .with_code("VX0504")
                                    .with_span(*span),
                                );
                                self.engine.has_error = true;
                                return "0".to_string();
                            }

                            let vec_handle = self.compile(container_expr);
                            let val_reg = self.compile(&args[1]);

                            let tmp = self.engine.next_register();
                            self.emit(&format!("    {} = alloca {}", tmp, elem_llvm_ty));
                            self.emit(&format!(
                                "    store {} {}, {}* {}",
                                elem_llvm_ty, val_reg, elem_llvm_ty, tmp
                            ));
                            let val_ptr_i8 = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = bitcast {}* {} to i8*",
                                val_ptr_i8, elem_llvm_ty, tmp
                            ));

                            self.emit(&format!(
                                "    call void @vox_vec_push(i8* {}, i8* {})",
                                vec_handle, val_ptr_i8
                            ));
                            return "0".to_string();
                        }
                    }

                    let array_expr = &args[0];
                    let array_name = match array_expr {
                        ASTNode::Identifier(name, _) => name,
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error("push argument must be an array variable")
                                    .with_code("VX0440")
                                    .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                    };
                    let elem_vox_type = match self.engine.dynamic_array_elem_type.get(array_name) {
                        Some(ty) => ty.clone(),
                        None => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Variable '{}' is not a dynamic array",
                                    array_name
                                ))
                                .with_code("VX0441")
                                .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                    };
                    let elem_llvm = self
                        .engine
                        .map_type(&elem_vox_type, self.target == CodegenTarget::Device);
                    let elem_size = self.engine.size_of_type(&elem_vox_type);
                    if elem_size == 0 {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Unknown element size for type '{}'",
                                elem_vox_type
                            ))
                            .with_code("VX0442")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let arr_ptr = self.compile(array_expr);
                    let elem_val = self.compile(&args[1]);
                    let elem_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", elem_tmp, elem_llvm));
                    self.emit(&format!(
                        "    store {} {}, {}* {}",
                        elem_llvm, elem_val, elem_llvm, elem_tmp
                    ));
                    let arr_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {{ i8*, i64, i64 }}* {} to i8*",
                        arr_i8, arr_ptr
                    ));
                    let elem_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        elem_i8, elem_llvm, elem_tmp
                    ));
                    self.emit(&format!(
                        "    call void @vox_array_push(i8* {}, i8* {}, i64 {})",
                        arr_i8, elem_i8, elem_size
                    ));
                    return "0".to_string();
                }

                if mangled_callee == "pop" {
                    self.engine.debug_log("CallExpr: pop");
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error("pop expects exactly 1 argument: container")
                                .with_code("VX0434")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }

                    let container_expr = &args[0];
                    let container_ty_raw = self.expr_type(container_expr).unwrap_or_default();
                    let container_ty = self.engine.expand_type_aliases(&container_ty_raw);

                    if let Some((base_name, type_args)) = parse_generic_type(&container_ty) {
                        if base_name == "Vec" && type_args.len() == 1 {
                            let elem_vox_ty = &type_args[0];
                            let elem_llvm_ty = self
                                .engine
                                .map_type(elem_vox_ty, self.target == CodegenTarget::Device);
                            let elem_size = self.engine.size_of_type(elem_vox_ty);
                            if elem_size == 0 {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Unknown element size for type '{}'",
                                        elem_vox_ty
                                    ))
                                    .with_code("VX0445")
                                    .with_span(*span),
                                );
                                self.engine.has_error = true;
                                return "0".to_string();
                            }

                            let vec_handle = self.compile(container_expr);
                            let out_tmp = self.engine.next_register();
                            self.emit(&format!("    {} = alloca {}", out_tmp, elem_llvm_ty));
                            let out_i8 = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = bitcast {}* {} to i8*",
                                out_i8, elem_llvm_ty, out_tmp
                            ));

                            let success = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = call i32 @vox_vec_pop(i8* {}, i8* {})",
                                success, vec_handle, out_i8
                            ));

                            let opt_ty = self.engine.map_type(
                                &format!("Option<{}>", elem_vox_ty),
                                self.target == CodegenTarget::Device,
                            );
                            let some_discriminant = 1;
                            let none_discriminant = 0;
                            let is_some_label = self.engine.next_block();
                            let is_none_label = self.engine.next_block();
                            let merge_label = self.engine.next_block();

                            let success_i1 = self.engine.next_register();
                            self.emit(&format!("    {} = icmp eq i32 {}, 0", success_i1, success));
                            self.emit(&format!(
                                "    br i1 {}, label %{}, label %{}",
                                success_i1, is_none_label, is_some_label
                            ));

                            self.emit(&format!("{}:", is_none_label));
                            let none_enum = self.engine.next_register();
                            self.emit(&format!("    {} = alloca {}", none_enum, opt_ty));
                            let disc_none = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                                disc_none, opt_ty, opt_ty, none_enum
                            ));
                            self.emit(&format!(
                                "    store i32 {}, i32* {}",
                                none_discriminant, disc_none
                            ));
                            let none_val = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = load {}, {}* {}",
                                none_val, opt_ty, opt_ty, none_enum
                            ));
                            self.emit(&format!("    br label %{}", merge_label));

                            self.emit(&format!("{}:", is_some_label));
                            let loaded_val = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = load {}, {}* {}",
                                loaded_val, elem_llvm_ty, elem_llvm_ty, out_tmp
                            ));
                            let some_enum = self.engine.next_register();
                            self.emit(&format!("    {} = alloca {}", some_enum, opt_ty));
                            let disc_some = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                                disc_some, opt_ty, opt_ty, some_enum
                            ));
                            self.emit(&format!(
                                "    store i32 {}, i32* {}",
                                some_discriminant, disc_some
                            ));
                            let payload_field = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 1",
                                payload_field, opt_ty, opt_ty, some_enum
                            ));
                            self.emit(&format!(
                                "    store {} {}, {}* {}",
                                elem_llvm_ty, loaded_val, elem_llvm_ty, payload_field
                            ));
                            let some_val = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = load {}, {}* {}",
                                some_val, opt_ty, opt_ty, some_enum
                            ));
                            self.emit(&format!("    br label %{}", merge_label));

                            self.emit(&format!("{}:", merge_label));
                            let phi = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = phi {} [ {}, %{} ], [ {}, %{} ]",
                                phi, opt_ty, none_val, is_none_label, some_val, is_some_label
                            ));
                            return phi;
                        }
                    }

                    let array_expr = &args[0];
                    let array_name = match array_expr {
                        ASTNode::Identifier(name, _) => name,
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error("pop argument must be an array variable")
                                    .with_code("VX0443")
                                    .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                    };
                    let elem_vox_type = match self.engine.dynamic_array_elem_type.get(array_name) {
                        Some(ty) => ty.clone(),
                        None => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Variable '{}' is not a dynamic array",
                                    array_name
                                ))
                                .with_code("VX0444")
                                .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                    };
                    let elem_llvm = self
                        .engine
                        .map_type(&elem_vox_type, self.target == CodegenTarget::Device);
                    let elem_size = self.engine.size_of_type(&elem_vox_type);
                    if elem_size == 0 {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Unknown element size for type '{}'",
                                elem_vox_type
                            ))
                            .with_code("VX0445")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let arr_ptr = self.compile(array_expr);
                    let out_tmp = self.engine.next_register();
                    self.emit(&format!("    {} = alloca {}", out_tmp, elem_llvm));
                    let arr_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {{ i8*, i64, i64 }}* {} to i8*",
                        arr_i8, arr_ptr
                    ));
                    let out_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        out_i8, elem_llvm, out_tmp
                    ));
                    self.emit(&format!(
                        "    call void @vox_array_pop(i8* {}, i8* {}, i64 {})",
                        arr_i8, out_i8, elem_size
                    ));
                    let result = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = load {}, {}* {}",
                        result, elem_llvm, elem_llvm, out_tmp
                    ));
                    return result;
                }

                let base_callee = if mangled_callee.contains("::") {
                    mangled_callee
                        .split("::")
                        .last()
                        .unwrap_or(&mangled_callee)
                        .to_string()
                } else {
                    mangled_callee.clone()
                };

                let final_callee = if self.engine.kernel_names.contains(&base_callee) {
                    if self.engine.gpu_mode.is_some() && self.target == CodegenTarget::Host {
                        format!("{}_launch", base_callee)
                    } else if self.target == CodegenTarget::Host {
                        format!("{}_cpu", base_callee)
                    } else {
                        base_callee.clone()
                    }
                } else {
                    mangled_callee.clone()
                };

                let ret_type = self
                    .engine
                    .function_return_types
                    .get(&final_callee)
                    .map(|s| {
                        self.engine
                            .map_type(s, self.target == CodegenTarget::Device)
                    })
                    .unwrap_or_else(|| {
                        self.engine.debug_log(&format!(
                            "WARNING: No return type found for callee '{}', defaulting to i32",
                            final_callee
                        ));
                        "i32".to_string()
                    });

                self.engine.debug_log(&format!(
                    "CallExpr: final_callee='{}', resolved_return_type='{}'",
                    final_callee, ret_type
                ));

                let mut compiled_args = Vec::new();
                for arg in args {
                    let arg_reg = self.compile(arg);
                    let (ty, compiled_reg) = match arg {
                        ASTNode::Identifier(name, _) => {
                            if let Some((enum_name, variant_name)) = name.split_once("::") {
                                let base_enum = CodegenEngine::strip_generic_args(enum_name);
                                if let Some(variants) = self.engine.enum_variants.get(&base_enum) {
                                    if variants.contains_key(variant_name) {
                                        let llvm_ty = self.engine.map_type(
                                            &base_enum,
                                            self.target == CodegenTarget::Device,
                                        );
                                        (llvm_ty, arg_reg)
                                    } else {
                                        ("i32".to_string(), arg_reg)
                                    }
                                } else {
                                    ("i32".to_string(), arg_reg)
                                }
                            } else if let Some((ty_str, _alloc_reg, _, _)) =
                                self.engine.variable_symbols.get(name)
                            {
                                let vox_ty = ty_str.clone();
                                let llvm_ty = self
                                    .engine
                                    .map_type(&vox_ty, self.target == CodegenTarget::Device);
                                let should_load = vox_ty.starts_with('%')
                                    || self.engine.enum_variants.contains_key(&vox_ty)
                                    || vox_ty.starts_with('[')
                                    || vox_ty == "String"
                                    || vox_ty.starts_with("[]");
                                if should_load {
                                    let loaded = self.engine.next_register();
                                    let ptr_ty = if self.target == CodegenTarget::Device {
                                        self.engine.device_ptr_type(&llvm_ty)
                                    } else {
                                        format!("{}*", llvm_ty)
                                    };
                                    self.emit(&format!(
                                        "    {} = load {}, {} {}",
                                        loaded, llvm_ty, ptr_ty, arg_reg
                                    ));
                                    (llvm_ty, loaded)
                                } else {
                                    (llvm_ty, arg_reg)
                                }
                            } else {
                                ("i32".to_string(), arg_reg)
                            }
                        }
                        ASTNode::StringLiteral(..) => ("{ i8*, i64 }".to_string(), arg_reg),
                        ASTNode::CastExpr { target_type, .. } => {
                            let llvm_ty = self
                                .engine
                                .map_type(target_type, self.target == CodegenTarget::Device);
                            (llvm_ty, arg_reg)
                        }
                        ASTNode::IntegerLiteral(..) => ("i32".to_string(), arg_reg),
                        ASTNode::FloatLiteral(..) => ("double".to_string(), arg_reg),
                        ASTNode::CharLiteral(..) => ("i32".to_string(), arg_reg),
                        ASTNode::BorrowExpr { expr, .. } => {
                            if let ASTNode::Identifier(name, _) = &**expr {
                                if let Some((ty, _, _, _)) = self.engine.variable_symbols.get(name)
                                {
                                    (format!("{}*", ty), arg_reg)
                                } else {
                                    ("i32*".to_string(), arg_reg)
                                }
                            } else {
                                ("i32*".to_string(), arg_reg)
                            }
                        }
                        ASTNode::DerefExpr(inner, _) => {
                            if let ASTNode::Identifier(name, _) = &**inner {
                                if let Some((ty, _, _, _)) = self.engine.variable_symbols.get(name)
                                {
                                    (ty.trim_end_matches('*').to_string(), arg_reg)
                                } else {
                                    ("i32".to_string(), arg_reg)
                                }
                            } else {
                                ("i32".to_string(), arg_reg)
                            }
                        }
                        _ => {
                            let ty = self.expr_type(arg).unwrap_or_else(|| "i32".to_string());
                            let llvm_ty = self
                                .engine
                                .map_type(&ty, self.target == CodegenTarget::Device);
                            (llvm_ty, arg_reg)
                        }
                    };
                    compiled_args.push((ty, compiled_reg));
                }

                let arg_list: Vec<String> = compiled_args
                    .iter()
                    .map(|(ty, reg)| format!("{} {}", ty, reg))
                    .collect();

                if ret_type == "void" {
                    self.emit(&format!(
                        "    call void @{}({})",
                        final_callee,
                        arg_list.join(", ")
                    ));
                    "0".to_string()
                } else {
                    let result_reg = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call {} @{}({})",
                        result_reg,
                        ret_type,
                        final_callee,
                        arg_list.join(", ")
                    ));
                    result_reg
                }
            }

            ASTNode::UnaryExpr { op, expr, .. } => {
                self.engine
                    .debug_log(&format!("compile UnaryExpr {:?}", op));
                let val = self.compile(expr);
                match op {
                    TokenKind::Not => {
                        let cmp = self.engine.next_register();
                        let result = self.engine.next_register();
                        self.emit(&format!("    {} = icmp eq i32 {}, 0", cmp, val));
                        self.emit(&format!("    {} = zext i1 {} to i32", result, cmp));
                        result
                    }
                    TokenKind::Minus => {
                        let result = self.engine.next_register();
                        self.emit(&format!("    {} = sub i32 0, {}", result, val));
                        result
                    }
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!("Unsupported unary operator {:?}", op))
                                .with_code("VX0426"),
                        );
                        self.engine.has_error = true;
                        "0".to_string()
                    }
                }
            }

            ASTNode::BinaryExpr {
                left, op, right, ..
            } => {
                self.engine
                    .debug_log(&format!("compile BinaryExpr {:?}", op));
                let left_val = self.compile(left);
                let right_val = self.compile(right);
                let left_ty = self.expr_type(left).unwrap_or_else(|| "i32".to_string());
                let right_ty = self.expr_type(right).unwrap_or_else(|| "i32".to_string());
                let is_float = left_ty == "f64" || right_ty == "f64";
                let is_integer = !is_float;

                match op {
                    TokenKind::Equal
                    | TokenKind::NotEqual
                    | TokenKind::LessThan
                    | TokenKind::GreaterThan
                    | TokenKind::LessThanOrEqual
                    | TokenKind::GreaterThanOrEqual
                    | TokenKind::And
                    | TokenKind::Or => {
                        let is_aggregate_eq = matches!(op, TokenKind::Equal | TokenKind::NotEqual);
                        if is_aggregate_eq {
                            let left_vox = self.expr_type(left).unwrap_or_default();
                            let right_vox = self.expr_type(right).unwrap_or_default();
                            self.engine.debug_log(&format!(
                                "BinaryExpr aggregate check: left_vox='{}', right_vox='{}'",
                                left_vox, right_vox
                            ));

                            if left_vox == "&str" && right_vox == "&str" {
                                self.engine
                                    .debug_log("String slice equality, using vox_string_compare");
                                let left_data = self.engine.next_register();
                                self.emit(&format!(
                                    "    {} = extractvalue {{ i8*, i64 }} {}, 0",
                                    left_data, left_val
                                ));
                                let left_len = self.engine.next_register();
                                self.emit(&format!(
                                    "    {} = extractvalue {{ i8*, i64 }} {}, 1",
                                    left_len, left_val
                                ));
                                let right_data = self.engine.next_register();
                                self.emit(&format!(
                                    "    {} = extractvalue {{ i8*, i64 }} {}, 0",
                                    right_data, right_val
                                ));
                                let right_len = self.engine.next_register();
                                self.emit(&format!(
                                    "    {} = extractvalue {{ i8*, i64 }} {}, 1",
                                    right_len, right_val
                                ));
                                let cmp_result = self.engine.next_register();
                                self.emit(&format!(
                                    "    {} = call i32 @vox_string_compare(i8* {}, i64 {}, i8* {}, i64 {})",
                                    cmp_result, left_data, left_len, right_data, right_len
                                ));
                                let cmp_i1 = self.engine.next_register();
                                if *op == TokenKind::Equal {
                                    self.emit(&format!(
                                        "    {} = icmp eq i32 {}, 0",
                                        cmp_i1, cmp_result
                                    ));
                                } else {
                                    self.emit(&format!(
                                        "    {} = icmp ne i32 {}, 0",
                                        cmp_i1, cmp_result
                                    ));
                                }
                                let result_reg = self.engine.next_register();
                                self.emit(&format!(
                                    "    {} = zext i1 {} to i32",
                                    result_reg, cmp_i1
                                ));
                                return result_reg;
                            }

                            let left_stripped = CodegenEngine::strip_references(&left_vox);
                            let right_stripped = CodegenEngine::strip_references(&right_vox);
                            let left_base = CodegenEngine::strip_generic_args(left_stripped);
                            let right_base = CodegenEngine::strip_generic_args(right_stripped);
                            let is_enum = self.engine.enum_variants.contains_key(&left_base)
                                && self.engine.enum_variants.contains_key(&right_base);
                            let is_struct = self.engine.struct_fields.contains_key(&left_base)
                                && self.engine.struct_fields.contains_key(&right_base);
                            self.engine.debug_log(&format!(
                                "BinaryExpr: left_base='{}', right_base='{}', is_enum={}, is_struct={}, types_equal={}",
                                left_base, right_base, is_enum, is_struct, left_vox == right_vox
                            ));
                            if (is_enum || is_struct) && left_vox == right_vox {
                                self.engine.debug_log(
                                    "BinaryExpr: using fieldwise comparison for aggregate",
                                );
                                let llvm_ty = self
                                    .engine
                                    .map_type(&left_vox, self.target == CodegenTarget::Device);
                                let fields = self.get_struct_field_types(&llvm_ty);
                                if !fields.is_empty() {
                                    let mut eq_i1 = None;
                                    for (i, field_ty) in fields.iter().enumerate() {
                                        let left_field = self.engine.next_register();
                                        self.emit(&format!(
                                            "    {} = extractvalue {} {}, {}",
                                            left_field, llvm_ty, left_val, i
                                        ));
                                        let right_field = self.engine.next_register();
                                        self.emit(&format!(
                                            "    {} = extractvalue {} {}, {}",
                                            right_field, llvm_ty, right_val, i
                                        ));
                                        let field_eq = self.engine.next_register();
                                        self.emit(&format!(
                                            "    {} = icmp eq {} {}, {}",
                                            field_eq, field_ty, left_field, right_field
                                        ));
                                        eq_i1 = match eq_i1 {
                                            Some(prev) => {
                                                let combined = self.engine.next_register();
                                                self.emit(&format!(
                                                    "    {} = and i1 {}, {}",
                                                    combined, prev, field_eq
                                                ));
                                                Some(combined)
                                            }
                                            None => Some(field_eq),
                                        };
                                    }
                                    if let Some(eq) = eq_i1 {
                                        let result_reg = self.engine.next_register();
                                        if *op == TokenKind::Equal {
                                            self.emit(&format!(
                                                "    {} = zext i1 {} to i32",
                                                result_reg, eq
                                            ));
                                        } else {
                                            let ne = self.engine.next_register();
                                            self.emit(&format!("    {} = xor i1 {}, 1", ne, eq));
                                            self.emit(&format!(
                                                "    {} = zext i1 {} to i32",
                                                result_reg, ne
                                            ));
                                        }
                                        return result_reg;
                                    }
                                }
                            }
                        }

                        let (cmp_op, cmp_type) = match *op {
                            TokenKind::Equal => ("eq", "icmp"),
                            TokenKind::NotEqual => ("ne", "icmp"),
                            TokenKind::LessThan => ("slt", "icmp"),
                            TokenKind::GreaterThan => ("sgt", "icmp"),
                            TokenKind::LessThanOrEqual => ("sle", "icmp"),
                            TokenKind::GreaterThanOrEqual => ("sge", "icmp"),
                            TokenKind::And => ("ne", "icmp"),
                            TokenKind::Or => ("ne", "icmp"),
                            _ => unreachable!(),
                        };

                        if *op == TokenKind::And {
                            let left_bool = self.engine.next_register();
                            self.emit(&format!("    {} = icmp ne i32 {}, 0", left_bool, left_val));
                            let right_bool = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = icmp ne i32 {}, 0",
                                right_bool, right_val
                            ));
                            let and_i1 = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = and i1 {}, {}",
                                and_i1, left_bool, right_bool
                            ));
                            let result_reg = self.engine.next_register();
                            self.emit(&format!("    {} = zext i1 {} to i32", result_reg, and_i1));
                            return result_reg;
                        }
                        if *op == TokenKind::Or {
                            let left_bool = self.engine.next_register();
                            self.emit(&format!("    {} = icmp ne i32 {}, 0", left_bool, left_val));
                            let right_bool = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = icmp ne i32 {}, 0",
                                right_bool, right_val
                            ));
                            let or_i1 = self.engine.next_register();
                            self.emit(&format!(
                                "    {} = or i1 {}, {}",
                                or_i1, left_bool, right_bool
                            ));
                            let result_reg = self.engine.next_register();
                            self.emit(&format!("    {} = zext i1 {} to i32", result_reg, or_i1));
                            return result_reg;
                        }

                        let cmp_reg = self.engine.next_register();
                        if is_float {
                            let fcmp_pred = match *op {
                                TokenKind::Equal => "oeq",
                                TokenKind::NotEqual => "une",
                                TokenKind::LessThan => "olt",
                                TokenKind::GreaterThan => "ogt",
                                TokenKind::LessThanOrEqual => "ole",
                                TokenKind::GreaterThanOrEqual => "oge",
                                _ => unreachable!(),
                            };
                            self.emit(&format!(
                                "    {} = fcmp {} double {}, {}",
                                cmp_reg, fcmp_pred, left_val, right_val
                            ));
                        } else {
                            self.emit(&format!(
                                "    {} = {} {} i32 {}, {}",
                                cmp_reg, cmp_type, cmp_op, left_val, right_val
                            ));
                        }
                        let result_reg = self.engine.next_register();
                        self.emit(&format!("    {} = zext i1 {} to i32", result_reg, cmp_reg));
                        return result_reg;
                    }
                    _ => {}
                }

                let result_reg = self.engine.next_register();
                if is_float {
                    let line = match op {
                        TokenKind::Plus => {
                            format!(
                                "    {} = fadd double {}, {}",
                                result_reg, left_val, right_val
                            )
                        }
                        TokenKind::Minus => {
                            format!(
                                "    {} = fsub double {}, {}",
                                result_reg, left_val, right_val
                            )
                        }
                        TokenKind::Star => {
                            format!(
                                "    {} = fmul double {}, {}",
                                result_reg, left_val, right_val
                            )
                        }
                        TokenKind::Div => {
                            format!(
                                "    {} = fdiv double {}, {}",
                                result_reg, left_val, right_val
                            )
                        }
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Unsupported binary operator for float: {:?}",
                                    op
                                ))
                                .with_code("VX0422"),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                    };
                    self.emit(&line);
                    result_reg
                } else {
                    let is_i64 = left_ty == "i64" || right_ty == "i64";
                    let (int_ty, native_op) = match op {
                        TokenKind::Plus => (if is_i64 { "i64" } else { "i32" }, "add"),
                        TokenKind::Minus => (if is_i64 { "i64" } else { "i32" }, "sub"),
                        TokenKind::Star => (if is_i64 { "i64" } else { "i32" }, "mul"),
                        TokenKind::Div => (if is_i64 { "i64" } else { "i32" }, "sdiv"),
                        TokenKind::Mod => (if is_i64 { "i64" } else { "i32" }, "srem"),
                        TokenKind::Ampersand => ("i32", "and"),
                        TokenKind::Pipe => ("i32", "or"),
                        TokenKind::Caret => ("i32", "xor"),
                        TokenKind::Shl => ("i32", "shl"),
                        TokenKind::Shr => ("i32", "ashr"),
                        _ => unreachable!(),
                    };

                    if self.target == CodegenTarget::Device {
                        match op {
                            TokenKind::Plus => {
                                self.emit(&format!(
                                    "    {} = {} {} {}, {}",
                                    result_reg, native_op, int_ty, left_val, right_val
                                ));
                            }
                            TokenKind::Minus => {
                                self.emit(&format!(
                                    "    {} = {} {} {}, {}",
                                    result_reg, native_op, int_ty, left_val, right_val
                                ));
                            }
                            TokenKind::Star => {
                                self.emit(&format!(
                                    "    {} = {} {} {}, {}",
                                    result_reg, native_op, int_ty, left_val, right_val
                                ));
                            }
                            TokenKind::Div => {
                                self.emit(&format!(
                                    "    {} = {} {} {}, {}",
                                    result_reg, native_op, int_ty, left_val, right_val
                                ));
                            }
                            TokenKind::Mod => {
                                self.emit(&format!(
                                    "    {} = {} {} {}, {}",
                                    result_reg, native_op, int_ty, left_val, right_val
                                ));
                            }
                            TokenKind::Ampersand
                            | TokenKind::Pipe
                            | TokenKind::Caret
                            | TokenKind::Shl
                            | TokenKind::Shr => {
                                self.emit(&format!(
                                    "    {} = {} {} {}, {}",
                                    result_reg, native_op, int_ty, left_val, right_val
                                ));
                            }
                            _ => unreachable!(),
                        }
                    } else {
                        match op {
                            TokenKind::Plus => {
                                self.emit(&format!(
                                    "    {} = call i32 @vox_add_i32(i32 {}, i32 {})",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Minus => {
                                self.emit(&format!(
                                    "    {} = call i32 @vox_sub_i32(i32 {}, i32 {})",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Star => {
                                self.emit(&format!(
                                    "    {} = call i32 @vox_mul_i32(i32 {}, i32 {})",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Div => {
                                self.emit(&format!(
                                    "    {} = call i32 @vox_div_i32(i32 {}, i32 {})",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Mod => {
                                self.emit(&format!(
                                    "    {} = call i32 @vox_rem_i32(i32 {}, i32 {})",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Ampersand => {
                                self.emit(&format!(
                                    "    {} = and i32 {}, {}",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Pipe => {
                                self.emit(&format!(
                                    "    {} = or i32 {}, {}",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Caret => {
                                self.emit(&format!(
                                    "    {} = xor i32 {}, {}",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Shl => {
                                self.emit(&format!(
                                    "    {} = shl i32 {}, {}",
                                    result_reg, left_val, right_val
                                ));
                            }
                            TokenKind::Shr => {
                                self.emit(&format!(
                                    "    {} = ashr i32 {}, {}",
                                    result_reg, left_val, right_val
                                ));
                            }
                            _ => unreachable!(),
                        }
                    }
                    result_reg
                }
            }

            ASTNode::ComptimeBlock { span, .. } => match ComptimeEvaluator::evaluate(node) {
                Some(constant_node) => self.compile(&constant_node),
                None => {
                    let err_msg = if self.target == CodegenTarget::Device {
                        "Failed to evaluate @comptime block on device. All expressions must be constant at compile time."
                    } else {
                        "Failed to evaluate @comptime block at compile time. All expressions must be constant."
                    };
                    emit_diagnostic(
                        &Diagnostic::error(err_msg)
                            .with_code("VX0420")
                            .with_span(*span),
                    );
                    self.engine.has_error = true;
                    return "0".to_string();
                }
            },

            ASTNode::KernelLaunch {
                kernel,
                grid,
                args,
                span,
            } => {
                self.engine.debug_log("compile KernelLaunch");

                // Resolve kernel name
                let kernel_name = match kernel.as_ref() {
                    ASTNode::Identifier(name, _) => name.clone(),
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error("Kernel launch requires a kernel identifier")
                                .with_code("VX0305")
                                .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                };

                // Retrieve kernel information from the engine
                let (block_x, block_y, block_z) = match self.engine.kernel_attrs.get(&kernel_name) {
                    Some(attr) => (attr.block.0, attr.block.1, attr.block.2),
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Kernel '{}' has no block dimensions defined (missing @kernel attribute?)",
                                kernel_name
                            ))
                            .with_code("VX0310")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                };

                // Look up kernel parameter types (Vox type strings) from the symbol table.
                let param_vox_types = match self.engine.get_kernel_param_types(&kernel_name) {
                    Some(types) => types.clone(),
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot retrieve parameter types for kernel '{}'",
                                kernel_name
                            ))
                            .with_code("VX0311")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                };

                if args.len() != param_vox_types.len() {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Kernel '{}' expects {} arguments, got {}",
                            kernel_name,
                            param_vox_types.len(),
                            args.len()
                        ))
                        .with_code("VX0312")
                        .with_span(*span),
                    );
                    self.engine.has_error = true;
                    return "0".to_string();
                }

                // Compile grid dimensions (must be i32)
                let (gx_expr, gy_expr, gz_expr) = grid;
                let grid_x = self.compile(gx_expr);
                let grid_y = self.compile(gy_expr);
                let grid_z = self.compile(gz_expr);

                // -------------------------------------------------------------
                // 1. Allocate device memory for mutable reference (&mut) arguments
                // -------------------------------------------------------------
                let mut mutable_indices = Vec::new();
                for (i, vt) in param_vox_types.iter().enumerate() {
                    if vt.starts_with("&mut ") {
                        mutable_indices.push(i);
                    }
                }
                let mut device_ptrs = Vec::new(); // (arg_index, device_ptr_reg, size_in_bytes, pointee_vox)
                for &idx in &mutable_indices {
                    let pointee_vox = &param_vox_types[idx][5..]; // after "&mut "
                    let size = self.engine.size_of_type(pointee_vox);
                    if size == 0 {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot determine size of pointee type '{}' for mutable parameter",
                                pointee_vox
                            ))
                            .with_code("VX0313")
                            .with_span(*span),
                        );
                        self.engine.has_error = true;
                        return "0".to_string();
                    }
                    let dev_ptr = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = call i8* @vox_gpu_malloc(i64 {})",
                        dev_ptr, size
                    ));
                    let pointee_llvm = self
                        .engine
                        .map_type(pointee_vox, self.target == CodegenTarget::Device);
                    let cast_ptr = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast i8* {} to {}*",
                        cast_ptr, dev_ptr, pointee_llvm
                    ));
                    device_ptrs.push((idx, cast_ptr, size, pointee_vox.to_string()));
                }

                // -------------------------------------------------------------
                // 2. Build argument pointer array (void**)
                // -------------------------------------------------------------
                let arg_array = self.engine.next_register();
                self.emit(&format!(
                    "    {} = alloca i8*, i32 {}, align 8",
                    arg_array,
                    args.len()
                ));

                for (i, arg) in args.iter().enumerate() {
                    let arg_val = self.compile(arg); // SSA value
                    let arg_vox = self.expr_type(arg).unwrap_or_else(|| "i32".to_string());

                    if mutable_indices.contains(&i) {
                        // Mutable reference argument: &mut T
                        // The argument expression must be a BorrowExpr of a variable.
                        // Retrieve the host variable's alloca pointer.
                        let host_ptr = match arg {
                            ASTNode::BorrowExpr { expr, .. } => match &**expr {
                                ASTNode::Identifier(name, _) => {
                                    if let Some((_, alloc_reg, _, _)) =
                                        self.engine.variable_symbols.get(name)
                                    {
                                        alloc_reg.clone()
                                    } else {
                                        emit_diagnostic(
                                            &Diagnostic::error(&format!("Cannot find host variable '{}' for mutable kernel argument", name))
                                                .with_code("VX0314")
                                                .with_span(*span),
                                        );
                                        self.engine.has_error = true;
                                        return "0".to_string();
                                    }
                                }
                                _ => {
                                    emit_diagnostic(
                                        &Diagnostic::error("Mutable kernel argument must be a borrow of a variable")
                                            .with_code("VX0315")
                                            .with_span(*span),
                                    );
                                    self.engine.has_error = true;
                                    return "0".to_string();
                                }
                            },
                            _ => {
                                emit_diagnostic(
                                    &Diagnostic::error(
                                        "Mutable kernel argument must be a borrow expression",
                                    )
                                    .with_code("VX0316")
                                    .with_span(*span),
                                );
                                self.engine.has_error = true;
                                return "0".to_string();
                            }
                        };
                        // Strip "&mut " from arg_vox to get the pointee Vox type
                        let pointee_vox = arg_vox
                            .strip_prefix("&mut ")
                            .unwrap_or(&arg_vox)
                            .to_string();
                        let pointee_llvm = self.engine.map_type(&pointee_vox, false); // host LLVM type of pointee
                        // Cast host pointer to i8*
                        let host_i8 = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = bitcast {}* {} to i8*",
                            host_i8, pointee_llvm, host_ptr
                        ));
                        // Get device pointer and cast to i8*
                        let dev_ptr_reg = device_ptrs
                            .iter()
                            .find(|(idx, _, _, _)| *idx == i)
                            .unwrap()
                            .1
                            .clone();
                        let dev_i8 = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = bitcast {}* {} to i8*",
                            dev_i8, pointee_llvm, dev_ptr_reg
                        ));
                        // Copy host value to device
                        let size = self.engine.size_of_type(&pointee_vox);
                        self.emit(&format!(
                            "    call void @vox_gpu_memcpy_host_to_device(i8* {}, i8* {}, i64 {})",
                            dev_i8, host_i8, size
                        ));

                        // *** FIX: create a temporary stack variable to hold the device pointer value ***
                        // The argument array must contain a pointer to the argument value.
                        // For a pointer argument (the device pointer itself), the "value" is the pointer.
                        // We create a temporary i8*, store the device pointer in it, then pass the address of that temporary.
                        let temp_ptr = self.engine.next_register();
                        self.emit(&format!("    {} = alloca i8*", temp_ptr));
                        self.emit(&format!("    store i8* {}, i8** {}", dev_i8, temp_ptr));
                        let ptr_to_temp = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = bitcast i8** {} to i8*",
                            ptr_to_temp, temp_ptr
                        ));

                        let gep = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr i8*, i8** {}, i32 {}",
                            gep, arg_array, i
                        ));
                        self.emit(&format!("    store i8* {}, i8** {}", ptr_to_temp, gep));
                    } else {
                        // Non-mutable argument: normal value (i32, float, struct, etc.)
                        // Create a temporary alloca to hold the value and store its address.
                        let arg_llvm = self
                            .engine
                            .map_type(&arg_vox, self.target == CodegenTarget::Device);
                        let tmp = self.engine.next_register();
                        self.emit(&format!("    {} = alloca {}", tmp, arg_llvm));
                        self.emit(&format!(
                            "    store {} {}, {}* {}",
                            arg_llvm, arg_val, arg_llvm, tmp
                        ));
                        let ptr_to_tmp = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = bitcast {}* {} to i8*",
                            ptr_to_tmp, arg_llvm, tmp
                        ));
                        let gep = self.engine.next_register();
                        self.emit(&format!(
                            "    {} = getelementptr i8*, i8** {}, i32 {}",
                            gep, arg_array, i
                        ));
                        self.emit(&format!("    store i8* {}, i8** {}", ptr_to_tmp, gep));
                    }
                }

                // -----------------------------------------------------------------
                // 3. Launch the kernel
                // -----------------------------------------------------------------
                let (kernel_name_ptr, ptr_inst) = self.engine.get_string_ptr(&kernel_name);
                self.emit(&ptr_inst);
                let launch_ret = self.engine.next_register();
                self.emit(&format!(
                    "    {} = call i32 @vox_launch_kernel_3d(i8* {}, i32 {}, i32 {}, i32 {}, i32 {}, i32 {}, i32 {}, i8** {}, i32 {})",
                    launch_ret,
                    kernel_name_ptr,
                    grid_x, grid_y, grid_z,
                    block_x, block_y, block_z,
                    arg_array,
                    args.len()
                ));

                let success_i1 = self.engine.next_register();
                self.emit(&format!(
                    "    {} = icmp eq i32 {}, 0",
                    success_i1, launch_ret
                ));
                let fail_label = self.engine.next_block();
                let cont_label = self.engine.next_block();
                self.emit(&format!(
                    "    br i1 {}, label %{}, label %{}",
                    success_i1, cont_label, fail_label
                ));
                self.emit(&format!("{}:", fail_label));
                self.emit(&format!("    call void @vox_panic()"));
                self.emit(&format!("    unreachable"));
                self.emit(&format!("{}:", cont_label));

                // -------------------------------------------------------------
                // 4. Copy back results for &mut parameters (device → host)
                // -------------------------------------------------------------
                for (idx, dev_ptr, size, pointee_vox) in &device_ptrs {
                    let host_ptr = match &args[*idx] {
                        ASTNode::BorrowExpr { expr, .. } => match &**expr {
                            ASTNode::Identifier(name, _) => {
                                if let Some((_, alloc_reg, _, _)) =
                                    self.engine.variable_symbols.get(name)
                                {
                                    alloc_reg.clone()
                                } else {
                                    emit_diagnostic(
                                        &Diagnostic::error(&format!(
                                            "Cannot find host variable '{}' for mutable kernel argument",
                                            name
                                        ))
                                        .with_code("VX0314")
                                        .with_span(*span),
                                    );
                                    self.engine.has_error = true;
                                    return "0".to_string();
                                }
                            }
                            _ => {
                                emit_diagnostic(
                                    &Diagnostic::error("Mutable kernel argument must be a simple borrow of a variable")
                                        .with_code("VX0315")
                                        .with_span(*span),
                                );
                                self.engine.has_error = true;
                                return "0".to_string();
                            }
                        },
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(
                                    "Mutable kernel argument must be a borrow expression",
                                )
                                .with_code("VX0316")
                                .with_span(*span),
                            );
                            self.engine.has_error = true;
                            return "0".to_string();
                        }
                    };
                    let pointee_llvm = self
                        .engine
                        .map_type(pointee_vox, self.target == CodegenTarget::Device);
                    let dev_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        dev_i8, pointee_llvm, dev_ptr
                    ));
                    self.emit(&format!(
                        "    call void @vox_gpu_memcpy_device_to_host(i8* {}, i8* {}, i64 {})",
                        host_ptr, dev_i8, size
                    ));
                }

                // -------------------------------------------------------------
                // 5. Free device memory
                // -------------------------------------------------------------
                for (_, dev_ptr, _, pointee_vox) in &device_ptrs {
                    let pointee_llvm = self
                        .engine
                        .map_type(pointee_vox, self.target == CodegenTarget::Device);
                    let dev_i8 = self.engine.next_register();
                    self.emit(&format!(
                        "    {} = bitcast {}* {} to i8*",
                        dev_i8, pointee_llvm, dev_ptr
                    ));
                    self.emit(&format!("    call void @vox_gpu_free(i8* {})", dev_i8));
                }

                "0".to_string()
            }

            _ => {
                self.engine
                    .debug_log(&format!("unhandled expression: {:?}", node));
                "0".to_string()
            }
        }
    }
}
