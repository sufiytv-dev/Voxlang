// stmt.rs - Statement compilation for Voxlang (LLVM IR generation)
// Uses string‑based IR emission and ExprEmitter for expressions.

use crate::codegen::CodegenEngine;
use crate::comptime::ComptimeEvaluator;
use crate::diagnostic::{Diagnostic, emit_diagnostic};
use crate::parser::{ASTNode, MatchArm, MatchPattern};

// --------------------------------------------------------------------------
// Permanent debug logging (always enabled)
// --------------------------------------------------------------------------
#[inline(always)]
fn log_stmt(msg: &str) {
    if crate::diagnostic::global_debug() {
        crate::diagnostic::debug_log(format!("[CODEGEN:STMT] {}", msg));
    }
}

impl CodegenEngine {
    pub fn dbg(&self, msg: &str) {
        if self.debug {
            crate::diagnostic::debug_log(format!("[CODEGEN] {}", msg));
        }
    }

    pub(crate) fn compile_statement(&mut self, node: &ASTNode) {
        if self.has_error {
            return;
        }
        log_stmt(&format!("compile_statement: {:?}", node));

        match node {
            ASTNode::StructDef {
                name,
                generic_params,
                fields,
                span: _,
            } => {
                log_stmt(&format!("compiling struct definition '{}'", name));
                // Store field information and generic parameters for later use.
                self.struct_fields.insert(
                    name.to_string(),
                    fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                );
                self.struct_generic_params
                    .insert(name.clone(), generic_params.clone());

                // Emit a type definition only for non‑generic structs.
                // Generic structs will have concrete instantiations generated on demand
                // inside map_type (see utils.rs).
                if generic_params.is_empty() {
                    let field_types: Vec<String> =
                        fields.iter().map(|f| self.map_type(&f.ty, false)).collect();
                    let struct_body = field_types.join(", ");
                    let struct_def = format!("%{} = type {{ {} }}", name, struct_body);
                    self.debug_emit(&struct_def);
                }
            }

            ASTNode::EnumDef {
                name,
                variants,
                span: _,
                params: _,
            } => {
                log_stmt(&format!("compiling enum definition '{}'", name));
                let mut variant_map = std::collections::HashMap::new();
                for (idx, variant) in variants.iter().enumerate() {
                    variant_map.insert(variant.name.clone(), idx as u32);
                }
                self.enum_variants.insert(name.clone(), variant_map);
            }

            ASTNode::KernelFn {
                name,
                params,
                body,
                device_triple,
                span,
            } => {
                log_stmt(&format!("processing kernel '{}'", name));
                self.kernel_names.insert(name.clone());

                if self.gpu_mode.is_some() {
                    let effective_triple =
                        if let Some(ref override_triple) = self.device_triple_override {
                            override_triple.clone()
                        } else {
                            device_triple.clone()
                        };
                    log_stmt(&format!(
                        "compiling kernel '{}' for GPU ({})",
                        name, effective_triple
                    ));
                    if self.device_triple.is_none() {
                        self.device_triple = Some(effective_triple.clone());
                        self.emit_global_device_header(&effective_triple);
                    }

                    let saved_kernel_name = self.current_kernel_name.take();
                    self.current_kernel_name = Some(name.clone());

                    self.compile_device_function(name, params, body);
                    self.pending_kernel_stubs
                        .push((name.clone(), params.clone()));

                    self.current_kernel_name = saved_kernel_name;
                } else {
                    log_stmt(&format!(
                        "compiling kernel '{}' as CPU fallback function '{}_cpu'",
                        name, name
                    ));
                    // 1. Compile the kernel body as a normal CPU function
                    let cpu_func_name = format!("{}_cpu", name);
                    let cpu_func = ASTNode::FunctionDef {
                        name: cpu_func_name.clone(),
                        params: params.clone(),
                        return_type: "void".to_string(),
                        return_refinement: None,
                        body: body.to_vec(),
                        span: *span,
                        generic_params: Vec::new(),
                    };
                    self.compile_statement(&cpu_func);

                    // 2. Emit a launch stub "{}_launch" that calls the CPU version
                    let mut stub_ir = String::new();
                    stub_ir.push_str(&format!("define void @{}_launch(", name));
                    let param_strings: Vec<String> = params
                        .iter()
                        .map(|p| format!("{} %{}", self.map_type(&p.ty, false), p.name))
                        .collect();
                    stub_ir.push_str(&param_strings.join(", "));
                    stub_ir.push_str(") {\n");
                    stub_ir.push_str("entry:\n");
                    let call_args: Vec<String> = params
                        .iter()
                        .map(|p| format!("{} %{}", self.map_type(&p.ty, false), p.name))
                        .collect();
                    stub_ir.push_str(&format!(
                        "    call void @{}({})\n",
                        cpu_func_name,
                        call_args.join(", ")
                    ));
                    stub_ir.push_str("    ret void\n");
                    stub_ir.push_str("}\n\n");
                    self.ir.push_str(&stub_ir);
                }
            }

            ASTNode::DeviceVarDecl {
                name, ty, value, ..
            } => {
                let ty_str = ty.as_deref().unwrap_or("i32");
                log_stmt(&format!(
                    "compiling device variable decl '{} : {:?}' (GPU stub)",
                    name, ty
                ));
                if self.gpu_mode.is_some() && self.device_triple.is_some() {
                    let elem_ty = self.map_type(ty_str, true);
                    let ptr_ty = self.alloca_pointer_type();
                    let alloc_reg = self.fresh_alloca_name(name);
                    let alloc_line = format!(
                        "    {} = alloca {}{}",
                        alloc_reg,
                        elem_ty,
                        self.alloca_addrspace_suffix()
                    );
                    self.debug_emit_device(&alloc_line);

                    let val_reg = self.compile_expression_device(value);
                    let store_line = format!(
                        "    store {} {}, {} {}",
                        elem_ty, val_reg, ptr_ty, alloc_reg
                    );
                    self.debug_emit_device(&store_line);

                    // Store both LLVM type and Vox type
                    self.var_vox_types.insert(name.clone(), ty_str.to_string());
                    self.variable_symbols
                        .insert(name.clone(), (elem_ty, alloc_reg, true, false));
                } else {
                    self.debug_log("Skipping host emission of @device variable (CPU mode)");
                }
            }

            // -------------------------------------------------------------
            // MODIFIED: FunctionDef with module prefix support, reliable closing brace,
            //           and function stack push/pop for debugging.
            // -------------------------------------------------------------
            ASTNode::FunctionDef {
                name,
                generic_params,
                params,
                return_type,
                body,
                ..
            } => {
                // Skip generic function templates – only concrete monomorphised versions are emitted.
                if !generic_params.is_empty() {
                    log_stmt(&format!(
                        "skipping generic function '{}' (will be monomorphised)",
                        name
                    ));
                    let param_types: Vec<String> = params.iter().map(|p| p.ty.clone()).collect();
                    self.register_generic_function(
                        name,
                        generic_params.clone(),
                        param_types,
                        return_type.clone(),
                    );
                    return;
                }

                let actual_name = if let Some(prefix) = &self.module_prefix {
                    format!("{}_{}", prefix, name)
                } else {
                    name.clone()
                };

                log_stmt(&format!(
                    "compiling function '{}' (actual name '{}')",
                    name, actual_name
                ));
                self.function_return_types
                    .insert(actual_name.clone(), return_type.clone());

                // NEW: Push function name onto the stack for brace emission logging
                self.current_function_stack.push(actual_name.clone());
                self.debug_log(&format!(
                    "Pushed function '{}' onto stack (depth: {})",
                    actual_name,
                    self.current_function_stack.len()
                ));

                let old_in_function = self.in_function;
                self.in_function = true;

                let saved_counter = self.register_counter;
                self.reset_for_new_function();

                let old_func_name = self.current_function_name.take();
                self.current_function_name = Some(actual_name.clone());

                self.variable_symbols.clear();
                self.var_vox_types.clear();
                let old_return_type = self.current_return_type.take();
                self.current_return_type = Some(return_type.clone());

                let effective_return_type = if actual_name == "main" && return_type == "void" {
                    "i32".to_string()
                } else {
                    return_type.clone()
                };

                let param_strings: Vec<String> = params
                    .iter()
                    .map(|p| format!("{} %{}", self.map_type(&p.ty, false), p.name))
                    .collect();

                let mapped_return_type = self.map_type(&effective_return_type, false);
                let func_sig = format!(
                    "define {} @{}({}) {{",
                    mapped_return_type,
                    actual_name,
                    param_strings.join(", ")
                );
                self.debug_emit(&func_sig);
                self.debug_emit("entry:");

                for param in params {
                    let llvm_ty = self.map_type(&param.ty, false);
                    let alloc_reg = self.fresh_alloca_name(&param.name);
                    let alloc_line = format!("    {} = alloca {}", alloc_reg, llvm_ty);
                    self.debug_emit(&alloc_line);
                    let store_line = format!(
                        "    store {} %{}, {}* {}",
                        llvm_ty, param.name, llvm_ty, alloc_reg
                    );
                    self.debug_emit(&store_line);
                    self.var_vox_types
                        .insert(param.name.clone(), param.ty.clone());
                    self.variable_symbols
                        .insert(param.name.clone(), (llvm_ty, alloc_reg, false, false));
                }

                for stmt in body {
                    if self.block_terminated {
                        log_stmt("Function body terminated, skipping remaining statements");
                        break;
                    }
                    self.compile_statement(stmt);
                    if self.has_error {
                        break;
                    }
                }

                // No default return – rely on explicit returns.
                // If the block is unterminated (should not happen for well‑formed code),
                // emit an unreachable to keep LLVM happy.
                if !self.is_current_block_terminated() && !self.has_error {
                    log_stmt("Function ended without terminator, emitting unreachable");
                    self.debug_emit("    unreachable");
                    self.block_terminated = true;
                }

                // -----------------------------------------------------------------
                // CRITICAL FIX: Always emit the closing brace, and do it via debug_emit
                // so that block_terminated is reset automatically.
                // -----------------------------------------------------------------
                log_stmt(&format!(
                    "Adding closing brace for function: {}",
                    actual_name
                ));
                self.debug_emit("}"); // closes the function
                self.debug_emit(""); // add a blank line for readability
                if self.debug {
                    crate::diagnostic::debug_log(format!(
                        "=== Finished emitting function '{}', current IR length: {} characters ===",
                        actual_name,
                        self.ir.len()
                    ));
                }

                // NEW: Pop function name from stack after closing brace
                let popped = self.current_function_stack.pop();
                self.debug_log(&format!(
                    "Popped function '{:?}' from stack (remaining depth: {})",
                    popped,
                    self.current_function_stack.len()
                ));

                // Restore state
                self.current_function_name = old_func_name;
                self.current_return_type = old_return_type;
                self.register_counter = saved_counter;
                self.in_function = old_in_function;
            }

            ASTNode::VariableDecl {
                name,
                ty,
                value,
                mutable,
                ..
            } => {
                log_stmt(&format!(
                    "compiling variable decl '{} : {:?}' mutable={}",
                    name, ty, mutable
                ));

                // Global variable (module scope)
                if !self.in_function {
                    if let Some(ty_str) = ty {
                        if ty_str.starts_with("[]") {
                            emit_diagnostic(
                                &Diagnostic::error("Global dynamic arrays are not yet supported")
                                    .with_code("VX0430")
                                    .with_span(value.span()),
                            );
                            self.has_error = true;
                            return;
                        }
                    } else {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "Global variables must have an explicit type annotation",
                            )
                            .with_code("VX0429")
                            .with_span(value.span()),
                        );
                        self.has_error = true;
                        return;
                    }

                    let const_val = match self.const_fold_expr(value) {
                        Some(v) => v,
                        None => {
                            emit_diagnostic(
                                &Diagnostic::error("Global initializer must be constant")
                                    .with_code("VX0429")
                                    .with_span(value.span()),
                            );
                            self.has_error = true;
                            return;
                        }
                    };
                    let global_name = format!("@{}", name);
                    let const_kw = if *mutable { "global" } else { "constant" };
                    let ty_str = self.map_type(ty.as_deref().unwrap(), false);
                    self.ir.push_str(&format!(
                        "{} = dso_local {} {} {}\n",
                        global_name, const_kw, ty_str, const_val
                    ));
                    self.global_variables
                        .insert(name.clone(), (ty_str, *mutable));
                    return;
                }

                // ==========================================================
                // FIX: Use qualified resolved type lookup
                // ==========================================================
                let vox_type_str = if let Some(t) = ty {
                    t.clone()
                } else if let Some(resolved) =
                    self.get_resolved_type_qualified(self.current_function_name.as_deref(), name)
                {
                    log_stmt(&format!(
                        "using resolved type '{}' for '{}' (qualified)",
                        resolved, name
                    ));
                    resolved.clone()
                } else {
                    log_stmt(&format!(
                        "fallback: inferring type for '{}' from value",
                        name
                    ));
                    self.infer_vox_type(value)
                };

                // ---------- Dynamic array handling ----------
                if vox_type_str.starts_with("[]") {
                    let elements = match &**value {
                        ASTNode::ArrayLiteral { elements, .. } => elements,
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(
                                    "Dynamic array initialization currently only supports array literals",
                                )
                                .with_code("VX0431")
                                .with_span(value.span()),
                            );
                            self.has_error = true;
                            return;
                        }
                    };

                    let struct_ty = self.map_type(&vox_type_str, false);
                    let arr_ptr = self.next_register();
                    self.debug_emit(&format!("    {} = alloca {}", arr_ptr, struct_ty));

                    let elem_type_raw = vox_type_str.trim_start_matches("[]").to_string();
                    self.dynamic_array_elem_type
                        .insert(name.clone(), elem_type_raw.clone());

                    let elem_llvm_ty = self.map_type(&elem_type_raw, false);
                    let n_elements = elements.len() as u64;

                    if n_elements == 0 {
                        let null_data = self.next_register();
                        self.debug_emit(&format!("    {} = inttoptr i64 0 to i8*", null_data));

                        let data_field = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                            data_field, struct_ty, struct_ty, arr_ptr
                        ));
                        self.debug_emit(&format!(
                            "    store i8* {}, i8** {}",
                            null_data, data_field
                        ));

                        let len_field = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 1",
                            len_field, struct_ty, struct_ty, arr_ptr
                        ));
                        self.debug_emit(&format!("    store i64 0, i64* {}", len_field));

                        let cap_field = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 2",
                            cap_field, struct_ty, struct_ty, arr_ptr
                        ));
                        self.debug_emit(&format!("    store i64 0, i64* {}", cap_field));
                    } else {
                        let null_ptr = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = inttoptr i64 0 to {}*",
                            null_ptr, elem_llvm_ty
                        ));
                        let next_ptr = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr {}, {}* {}, i32 1",
                            next_ptr, elem_llvm_ty, elem_llvm_ty, null_ptr
                        ));
                        let elem_size = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = ptrtoint {}* {} to i64",
                            elem_size, elem_llvm_ty, next_ptr
                        ));

                        let data_ptr = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = call i8* @vox_array_alloc(i64 {}, i64 {})",
                            data_ptr, elem_size, n_elements
                        ));

                        let data_field = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 0",
                            data_field, struct_ty, struct_ty, arr_ptr
                        ));
                        self.debug_emit(&format!(
                            "    store i8* {}, i8** {}",
                            data_ptr, data_field
                        ));

                        let len_field = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 1",
                            len_field, struct_ty, struct_ty, arr_ptr
                        ));
                        self.debug_emit(&format!(
                            "    store i64 {}, i64* {}",
                            n_elements, len_field
                        ));

                        let cap_field = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 2",
                            cap_field, struct_ty, struct_ty, arr_ptr
                        ));
                        self.debug_emit(&format!(
                            "    store i64 {}, i64* {}",
                            n_elements, cap_field
                        ));

                        let elem_ptr_base = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = bitcast i8* {} to {}*",
                            elem_ptr_base, data_ptr, elem_llvm_ty
                        ));
                        for (i, elem) in elements.iter().enumerate() {
                            let val_reg = self.compile_expression(elem, None);
                            let elem_addr = self.next_register();
                            self.debug_emit(&format!(
                                "    {} = getelementptr inbounds {}, {}* {}, i32 {}",
                                elem_addr, elem_llvm_ty, elem_llvm_ty, elem_ptr_base, i
                            ));
                            self.debug_emit(&format!(
                                "    store {} {}, {}* {}",
                                elem_llvm_ty, val_reg, elem_llvm_ty, elem_addr
                            ));
                        }
                    }

                    // Store Vox type and LLVM info
                    self.var_vox_types.insert(name.clone(), vox_type_str);
                    self.variable_symbols
                        .insert(name.clone(), (struct_ty, arr_ptr, false, *mutable));
                    return;
                }

                // ---------- Fixed‑size array or primitive / struct ----------
                let llvm_ty = self.map_type(&vox_type_str, false);
                let alloc_reg = self.fresh_alloca_name(name);
                let alloc_line = format!("    {} = alloca {}", alloc_reg, llvm_ty);
                self.debug_emit(&alloc_line);
                self.var_vox_types.insert(name.clone(), vox_type_str);
                self.variable_symbols.insert(
                    name.clone(),
                    (llvm_ty.clone(), alloc_reg.clone(), false, *mutable),
                );

                if let ASTNode::ArrayLiteral { elements, .. } = &**value {
                    for (i, elem) in elements.iter().enumerate() {
                        let val_reg = self.compile_expression(elem, None);
                        let idx = i as u64;
                        let elem_ptr_reg = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                            elem_ptr_reg, llvm_ty, llvm_ty, alloc_reg, idx
                        ));
                        let elem_ty = if let Some(inner) = llvm_ty.strip_prefix('[') {
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
                        };
                        self.debug_emit(&format!(
                            "    store {} {}, {}* {}",
                            elem_ty, val_reg, elem_ty, elem_ptr_reg
                        ));
                    }
                } else {
                    // Determine expected type: explicit annotation, or resolved type from inference (qualified)
                    let expected_owned = if let Some(ty_str) = ty.as_deref() {
                        Some(ty_str.to_string())
                    } else {
                        self.get_resolved_type_qualified(
                            self.current_function_name.as_deref(),
                            name,
                        )
                    };
                    let expected = expected_owned.as_deref();
                    let val_reg = self.compile_expression(value, expected);
                    let store_line = format!(
                        "    store {} {}, {}* {}",
                        llvm_ty, val_reg, llvm_ty, alloc_reg
                    );
                    self.debug_emit(&store_line);
                }
            }

            ASTNode::Assignment { lhs, value, .. } => {
                log_stmt("compiling assignment");
                let val_reg = self.compile_expression(value, None);
                match &**lhs {
                    ASTNode::Identifier(name, _) => {
                        let (ty_str, alloc_reg, _, _) =
                            self.variable_symbols.get(name).cloned().unwrap_or_else(|| {
                                // Fallback – generate a unique name to avoid collisions
                                let fallback_alloc = self.fresh_alloca_name(name);
                                ("i32".to_string(), fallback_alloc, false, false)
                            });
                        let store_line = format!(
                            "    store {} {}, {}* {}",
                            ty_str, val_reg, ty_str, alloc_reg
                        );
                        self.debug_emit(&store_line);
                    }
                    ASTNode::DerefExpr(inner, _) => {
                        let ptr_reg = self.compile_expression(inner, None);
                        let pointee_ty = if let ASTNode::Identifier(ref_name, _) = &**inner {
                            if let Some((ty, _, _, _)) = self.variable_symbols.get(ref_name) {
                                ty.trim_end_matches('*').to_string()
                            } else {
                                "i32".to_string()
                            }
                        } else {
                            "i32".to_string()
                        };
                        let store_line = format!(
                            "    store {} {}, {}* {}",
                            pointee_ty, val_reg, pointee_ty, ptr_reg
                        );
                        self.debug_emit(&store_line);
                    }
                    ASTNode::FieldAccess { expr, field, .. } => {
                        // Get Vox type of the base (using our new map)
                        let base_vox_ty = self.infer_vox_type(expr);
                        let mut ty = base_vox_ty.as_str();
                        let mut ptr_reg = self.compile_expression(expr, None);

                        // Auto-dereference to get actual struct pointer
                        while ty.starts_with('&') {
                            let loaded = self.next_register();
                            let ptr_ty = self.map_type(ty, false);
                            self.debug_emit(&format!(
                                "    {} = load {}, {}* {}",
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
                        }

                        // Now `ty` is the concrete Vox type (e.g., "Pair<i32,&str>").
                        // Strip generic arguments to get base struct name for field lookup.
                        let base_name = Self::strip_generic_args(ty);
                        let fields = match self.struct_fields.get(&base_name) {
                            Some(f) => f.clone(),
                            None => {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Struct '{}' not found for field assignment",
                                        base_name
                                    ))
                                    .with_code("VX0453"),
                                );
                                self.has_error = true;
                                return;
                            }
                        };
                        let idx = match fields.iter().position(|(fname, _)| fname == field) {
                            Some(i) => i,
                            None => {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Struct '{}' has no field '{}'",
                                        base_name, field
                                    ))
                                    .with_code("VX0454"),
                                );
                                self.has_error = true;
                                return;
                            }
                        };
                        // Determine concrete LLVM field type using the new helper
                        let llvm_field_ty = if let Some(fty) = self.get_concrete_field_llvm_type(
                            &base_name, ty, field, false, // host context
                        ) {
                            fty
                        } else {
                            // Fallback to generic field type (might be a placeholder)
                            let field_ty = &fields[idx].1;
                            self.map_type(field_ty, false)
                        };
                        // Use the full concrete type (ty) to get the LLVM struct type.
                        let struct_ty = self.map_type(ty, false);
                        let gep_reg = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                            gep_reg, struct_ty, struct_ty, ptr_reg, idx
                        ));
                        self.debug_emit(&format!(
                            "    store {} {}, {}* {}",
                            llvm_field_ty, val_reg, llvm_field_ty, gep_reg
                        ));
                    }
                    ASTNode::ArrayIndex { .. } => {
                        use crate::codegen::expr::{CodegenTarget, ExprEmitter};
                        let mut emitter = ExprEmitter {
                            engine: self,
                            target: CodegenTarget::Host,
                            lvalue: true,
                            expected_type: None,
                        };
                        let lhs_ptr = emitter.compile(lhs);
                        if self.has_error {
                            return;
                        }
                        let val_type = if let ASTNode::ArrayIndex { array, .. } = &**lhs {
                            let array_ty = self.infer_vox_type(array);
                            if array_ty.starts_with('[') {
                                if let Some(inner) = array_ty.strip_prefix('[') {
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
                        } else {
                            "i32".to_string()
                        };
                        let llvm_val_type = self.map_type(&val_type, false);
                        self.debug_emit(&format!(
                            "    store {} {}, {}* {}",
                            llvm_val_type, val_reg, llvm_val_type, lhs_ptr
                        ));
                    }
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error("Unsupported left-hand side in assignment")
                                .with_code("VX0401"),
                        );
                        self.has_error = true;
                    }
                }
            }

            ASTNode::IfStatement {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                log_stmt("compiling if statement");

                // Flatten the then_branch if it contains exactly one Block node.
                let then_stmts = if then_branch.len() == 1 {
                    if let ASTNode::Block { statements, .. } = &then_branch[0] {
                        log_stmt(&format!(
                            "Flattened then_branch: Block with {} statements",
                            statements.len()
                        ));
                        statements.as_slice()
                    } else {
                        then_branch.as_slice()
                    }
                } else {
                    then_branch.as_slice()
                };

                // Flatten the else_branch similarly.
                let else_stmts = if let Some(branch) = else_branch {
                    if branch.len() == 1 {
                        if let ASTNode::Block { statements, .. } = &branch[0] {
                            log_stmt(&format!(
                                "Flattened else_branch: Block with {} statements",
                                statements.len()
                            ));
                            Some(statements.as_slice())
                        } else {
                            Some(branch.as_slice())
                        }
                    } else {
                        Some(branch.as_slice())
                    }
                } else {
                    None
                };

                let cond_val = self.compile_expression(condition, None);
                let cond_i1 = self.next_register();
                self.debug_emit(&format!("    {} = icmp ne i32 {}, 0", cond_i1, cond_val));
                let then_label = self.next_block();
                let else_label = self.next_block();
                let merge_label = self.next_block();

                self.debug_emit(&format!(
                    "    br i1 {}, label %{}, label %{}",
                    cond_i1, then_label, else_label
                ));
                self.block_terminated = true;

                // Then block
                self.debug_emit(&format!("{}:", then_label));
                let then_terminated = {
                    for stmt in then_stmts {
                        if self.block_terminated {
                            break;
                        }
                        self.compile_statement(stmt);
                        if self.has_error {
                            break;
                        }
                    }
                    self.is_current_block_terminated()
                };
                if !then_terminated {
                    self.debug_emit(&format!("    br label %{}", merge_label));
                    self.block_terminated = true;
                }

                // Else block
                let else_terminated = if let Some(stmts) = else_stmts {
                    self.debug_emit(&format!("{}:", else_label));
                    for stmt in stmts {
                        if self.block_terminated {
                            break;
                        }
                        self.compile_statement(stmt);
                        if self.has_error {
                            break;
                        }
                    }
                    self.is_current_block_terminated()
                } else {
                    self.debug_emit(&format!("{}:", else_label));
                    false
                };
                if !else_terminated {
                    self.debug_emit(&format!("    br label %{}", merge_label));
                    self.block_terminated = true;
                }

                // Only emit the merge block if at least one branch did not terminate
                if !then_terminated || !else_terminated {
                    self.debug_emit(&format!("{}:", merge_label));
                }
            }

            ASTNode::WhileStatement {
                condition, body, ..
            } => {
                log_stmt("compiling while statement");

                // Flatten the loop body if it contains exactly one Block node.
                let body_stmts = if body.len() == 1 {
                    if let ASTNode::Block { statements, .. } = &body[0] {
                        log_stmt(&format!(
                            "Flattened loop body: Block with {} statements",
                            statements.len()
                        ));
                        statements.as_slice()
                    } else {
                        body.as_slice()
                    }
                } else {
                    body.as_slice()
                };

                let cond_label = self.next_block();
                let body_label = self.next_block();
                let exit_label = self.next_block();

                self.debug_emit(&format!("    br label %{}", cond_label));
                self.block_terminated = true;

                self.debug_emit(&format!("{}:", cond_label));
                let cond_val = self.compile_expression(condition, None);
                let cond_i1 = self.next_register();
                self.debug_emit(&format!("    {} = icmp ne i32 {}, 0", cond_i1, cond_val));
                self.debug_emit(&format!(
                    "    br i1 {}, label %{}, label %{}",
                    cond_i1, body_label, exit_label
                ));
                self.block_terminated = true;

                self.debug_emit(&format!("{}:", body_label));
                for stmt in body_stmts {
                    if self.block_terminated {
                        break;
                    }
                    self.compile_statement(stmt);
                }
                if !self.is_current_block_terminated() {
                    self.debug_emit(&format!("    br label %{}", cond_label));
                    self.block_terminated = true;
                }
                self.debug_emit(&format!("{}:", exit_label));
            }

            ASTNode::ParallelLoop {
                iter_var,
                start,
                end,
                body,
                ..
            } => {
                log_stmt("compiling parallel loop");

                // Flatten the parallel loop body if it contains exactly one Block node.
                let body_stmts = if body.len() == 1 {
                    if let ASTNode::Block { statements, .. } = &body[0] {
                        log_stmt(&format!(
                            "Flattened parallel loop body: Block with {} statements",
                            statements.len()
                        ));
                        statements.as_slice()
                    } else {
                        body.as_slice()
                    }
                } else {
                    body.as_slice()
                };

                let start_val = self.compile_expression(start, None);
                let end_val = self.compile_expression(end, None);

                let mut captured_vars = Vec::new();
                let body_refs: Vec<&ASTNode> = body_stmts.iter().collect();
                self.collect_captured_vars(&body_refs, &mut captured_vars);
                captured_vars.sort();
                captured_vars.dedup();

                let mut ctx_fields = Vec::new();
                for var in &captured_vars {
                    if let Some((ty, _, _, _)) = self.variable_symbols.get(var) {
                        ctx_fields.push((var.clone(), ty.clone()));
                    } else {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Captured variable '{}' not found in symbol table",
                                var
                            ))
                            .with_code("VX0402"),
                        );
                        self.has_error = true;
                        return;
                    }
                }
                let ctx_type = if ctx_fields.is_empty() {
                    "i8".to_string()
                } else {
                    let mut ty_str = "{".to_string();
                    for (_, ty) in &ctx_fields {
                        ty_str.push_str(&format!("{}, ", ty));
                    }
                    ty_str.pop();
                    ty_str.pop();
                    ty_str.push('}');
                    ty_str
                };

                let ctx_ptr = self.next_register();
                if ctx_fields.is_empty() {
                    self.debug_emit(&format!("    {} = alloca i8, align 8", ctx_ptr));
                    self.debug_emit(&format!("    store i8 0, i8* {}", ctx_ptr));
                } else {
                    self.debug_emit(&format!("    {} = alloca {}", ctx_ptr, ctx_type));
                    for (i, (name, _)) in ctx_fields.iter().enumerate() {
                        let field_ptr = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                            field_ptr, ctx_type, ctx_type, ctx_ptr, i
                        ));
                        let (_, alloc_reg, _, _) = self.variable_symbols.get(name).unwrap().clone();
                        let loaded = self.next_register();
                        self.debug_emit(&format!(
                            "    {} = load {}, {}* {}",
                            loaded, ctx_fields[i].1, ctx_fields[i].1, alloc_reg
                        ));
                        self.debug_emit(&format!(
                            "    store {} {}, {}* {}",
                            ctx_fields[i].1, loaded, ctx_fields[i].1, field_ptr
                        ));
                    }
                }

                let worker_name = self.next_worker_name();
                self.generate_worker_function(
                    &worker_name,
                    &iter_var,
                    &captured_vars,
                    body_stmts,
                    &ctx_type,
                    &ctx_fields,
                );
                if self.has_error {
                    return;
                }

                let ctx_i8 = self.next_register();
                self.debug_emit(&format!(
                    "    {} = bitcast {}* {} to i8*",
                    ctx_i8,
                    if ctx_fields.is_empty() {
                        "i8"
                    } else {
                        &ctx_type
                    },
                    ctx_ptr
                ));

                let start_i64 = self.next_register();
                let end_i64 = self.next_register();
                self.debug_emit(&format!(
                    "    {} = sext i32 {} to i64",
                    start_i64, start_val
                ));
                self.debug_emit(&format!("    {} = sext i32 {} to i64", end_i64, end_val));
                self.debug_emit(&format!("    call void @vox_dispatch_parallel(i8* bitcast (void (i64, i8*)* @{} to i8*), i8* {}, i64 {}, i64 {})", worker_name, ctx_i8, start_i64, end_i64));

                for (i, (name, ty)) in ctx_fields.iter().enumerate() {
                    let field_ptr = self.next_register();
                    self.debug_emit(&format!(
                        "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}",
                        field_ptr, ctx_type, ctx_type, ctx_ptr, i
                    ));
                    let loaded = self.next_register();
                    self.debug_emit(&format!(
                        "    {} = load {}, {}* {}",
                        loaded, ty, ty, field_ptr
                    ));
                    let (_, orig_alloc, _, _) = self.variable_symbols.get(name).unwrap();
                    self.debug_emit(&format!(
                        "    store {} {}, {}* {}",
                        ty, loaded, ty, orig_alloc
                    ));
                }
            }

            ASTNode::ComptimeBlock { body, .. } => {
                log_stmt("compiling comptime block");
                if let Some(evaluated) = ComptimeEvaluator::evaluate(node) {
                    self.compile_statement(&evaluated);
                } else {
                    for stmt in body {
                        if self.block_terminated {
                            break;
                        }
                        self.compile_statement(stmt);
                    }
                }
            }

            ASTNode::ReturnStatement(expr_opt, span) => {
                log_stmt("compiling return statement");
                let ret_type = self
                    .current_return_type
                    .clone()
                    .unwrap_or_else(|| "void".to_string());
                match expr_opt {
                    Some(expr) => {
                        // FIX: pass the expected return type to the expression compiler
                        let val_reg = self.compile_expression(expr, Some(&ret_type));
                        let llvm_ret_type = self.map_type(&ret_type, false);

                        let is_aggregate =
                            llvm_ret_type.starts_with('{') || llvm_ret_type.starts_with('%');
                        let is_integer_constant =
                            !val_reg.starts_with('%') && !val_reg.starts_with('@');
                        if is_aggregate && is_integer_constant {
                            if let Ok(_) = val_reg.parse::<i64>() {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Cannot return integer constant {} as aggregate type {}",
                                        val_reg, llvm_ret_type
                                    ))
                                    .with_code("VX0450")
                                    .with_span(*span),
                                );
                                self.has_error = true;
                                return;
                            }
                        }

                        // If the value is a pointer and the return type is not a pointer, load the value.
                        let final_val = if val_reg.ends_with('*') && !llvm_ret_type.ends_with('*') {
                            let loaded = self.next_register();
                            self.debug_emit(&format!(
                                "    {} = load {}, {}* {}",
                                loaded, llvm_ret_type, llvm_ret_type, val_reg
                            ));
                            loaded
                        } else {
                            val_reg
                        };
                        self.debug_emit(&format!("    ret {} {}", llvm_ret_type, final_val));
                        self.block_terminated = true;
                    }
                    None => {
                        if ret_type != "void" {
                            self.debug_emit(&format!(
                                "    ret {} 0",
                                self.map_type(&ret_type, false)
                            ));
                            self.block_terminated = true;
                        } else {
                            self.debug_emit("    ret void");
                            self.block_terminated = true;
                        }
                    }
                }
            }

            ASTNode::CallExpr { .. } => {
                log_stmt("compiling call expression (as statement)");
                let _ = self.compile_expression(node, None);
            }

            // -----------------------------------------------------------------
            // NEW: Handle Block statements – expand the statements inside.
            // -----------------------------------------------------------------
            ASTNode::Block {
                statements,
                span: _,
            } => {
                log_stmt(&format!(
                    "Entering Block with {} statements",
                    statements.len()
                ));
                for stmt in statements {
                    if self.block_terminated {
                        log_stmt("Block terminated, skipping remaining statements");
                        break;
                    }
                    log_stmt(&format!("  Block child: {:?}", stmt));
                    self.compile_statement(stmt);
                    if self.has_error {
                        break;
                    }
                }
                log_stmt("Exiting Block");
            }

            _ => {
                log_stmt(&format!("default: compiling as expression: {:?}", node));
                let _ = self.compile_expression(node, None);
            }
        }
    }
}
