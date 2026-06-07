// gpu.rs - GPU kernel compilation and launch stub emission for Voxlang
use crate::codegen::CodegenEngine;
use crate::diagnostic::{Diagnostic, emit_diagnostic};
use crate::parser::ASTNode;

impl CodegenEngine {
    // ------------------------------------------------------------------------
    // Device function (kernel) compilation
    // ------------------------------------------------------------------------
    pub(crate) fn compile_device_function(
        &mut self,
        name: &str,
        params: &[crate::parser::Param],
        body: &[ASTNode],
    ) {
        self.debug_log(&format!("compiling device function '{}'", name));

        let param_decls: Vec<String> = params
            .iter()
            .map(|p| format!("{} %{}", self.map_type(&p.ty, true), p.name))
            .collect();

        let kernel_prefix = if let Some(triple) = &self.device_triple {
            if triple.contains("amdgcn") {
                "amdgpu_kernel "
            } else {
                ""
            }
        } else {
            ""
        };
        self.device_ir.push_str(&format!(
            "define {prefix}void @{name}({params}) {{\n",
            prefix = kernel_prefix,
            name = name,
            params = param_decls.join(", ")
        ));
        self.device_ir.push_str("entry:\n");

        self.reset_for_new_function();
        self.variable_symbols.clear();

        for param in params {
            let elem_ty = self.map_type(&param.ty, true);
            let ptr_ty = self.alloca_pointer_type();
            let alloc_reg = self.fresh_alloca_name(&param.name);
            self.device_ir.push_str(&format!(
                "    {} = alloca {}{}\n",
                alloc_reg,
                elem_ty,
                self.alloca_addrspace_suffix()
            ));
            self.device_ir.push_str(&format!(
                "    store {} %{}, {} {}\n",
                elem_ty, param.name, ptr_ty, alloc_reg
            ));
            self.variable_symbols
                .insert(param.name.clone(), (elem_ty, alloc_reg, true, false));
        }

        for stmt in body {
            self.compile_statement_device(stmt);
            if self.has_error {
                break;
            }
        }

        // Device functions should always return void (kernel), but if the last block is not terminated, add a ret void.
        if !self.is_device_block_terminated() {
            self.device_ir.push_str("    ret void\n");
        }
        self.device_ir.push_str("}\n\n");
    }

    // ------------------------------------------------------------------------
    // Device statement compilation
    // ------------------------------------------------------------------------
    pub(crate) fn compile_statement_device(&mut self, node: &ASTNode) {
        if self.has_error {
            return;
        }
        match node {
            ASTNode::VariableDecl {
                name,
                ty,
                value,
                mutable,
                ..
            } => {
                // Fix: Convert Option<String> to &str, fallback to "i32"
                let ty_str = ty.as_deref().unwrap_or("i32");
                let elem_ty = self.map_type(ty_str, true);
                let ptr_ty = self.alloca_pointer_type();
                let alloc_reg = self.fresh_alloca_name(name);
                self.device_ir.push_str(&format!(
                    "    {} = alloca {}{}\n",
                    alloc_reg,
                    elem_ty,
                    self.alloca_addrspace_suffix()
                ));
                self.variable_symbols.insert(
                    name.clone(),
                    (elem_ty.clone(), alloc_reg.clone(), true, *mutable),
                );
                let val_reg = self.compile_expression_device(value);
                self.device_ir.push_str(&format!(
                    "    store {} {}, {} {}\n",
                    elem_ty, val_reg, ptr_ty, alloc_reg
                ));
            }
            ASTNode::Assignment { lhs, value, .. } => {
                let val_reg = self.compile_expression_device(value);
                match &**lhs {
                    ASTNode::Identifier(name, _) => {
                        let (elem_ty, alloc_reg, _, _) =
                            self.variable_symbols.get(name).cloned().unwrap();
                        let ptr_ty = self.alloca_pointer_type();
                        self.device_ir.push_str(&format!(
                            "    store {} {}, {} {}\n",
                            elem_ty, val_reg, ptr_ty, alloc_reg
                        ));
                    }
                    ASTNode::DerefExpr(inner, _) => {
                        let ptr_reg = self.compile_expression_device(inner);
                        let ptr_ty = if let ASTNode::Identifier(ref_name, _) = &**inner {
                            if let Some((ty, _, _, _)) = self.variable_symbols.get(ref_name) {
                                ty.clone()
                            } else {
                                "i32*".to_string()
                            }
                        } else if let ASTNode::BorrowExpr {
                            expr: inner_expr, ..
                        } = &**inner
                        {
                            if let ASTNode::Identifier(name, _) = &**inner_expr {
                                if let Some((ty, _, _, _)) = self.variable_symbols.get(name) {
                                    ty.clone()
                                } else {
                                    "i32*".to_string()
                                }
                            } else {
                                "i32*".to_string()
                            }
                        } else {
                            "i32*".to_string()
                        };
                        let pointee_ty = Self::strip_pointer_and_addrspace(&ptr_ty);
                        self.device_ir.push_str(&format!(
                            "    store {} {}, {} {}\n",
                            pointee_ty, val_reg, ptr_ty, ptr_reg
                        ));
                    }
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error("Unsupported LHS in device assignment")
                                .with_code("VX0403"),
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
                let cond_val = self.compile_expression_device(condition);
                let cond_i1 = self.next_register();
                self.device_ir
                    .push_str(&format!("    {} = icmp ne i32 {}, 0\n", cond_i1, cond_val));
                let then_label = self.next_block();
                let else_label = self.next_block();
                let merge_label = self.next_block();
                self.device_ir.push_str(&format!(
                    "    br i1 {}, label %{}, label %{}\n",
                    cond_i1, then_label, else_label
                ));
                self.device_ir.push_str(&format!("{}:\n", then_label));
                for stmt in then_branch {
                    self.compile_statement_device(stmt);
                }
                if !self.is_device_block_terminated() {
                    self.device_ir
                        .push_str(&format!("    br label %{}\n", merge_label));
                }
                if let Some(b) = else_branch {
                    self.device_ir.push_str(&format!("{}:\n", else_label));
                    for stmt in b {
                        self.compile_statement_device(stmt);
                    }
                    if !self.is_device_block_terminated() {
                        self.device_ir
                            .push_str(&format!("    br label %{}\n", merge_label));
                    }
                } else {
                    self.device_ir.push_str(&format!("{}:\n", else_label));
                    if !self.is_device_block_terminated() {
                        self.device_ir
                            .push_str(&format!("    br label %{}\n", merge_label));
                    }
                }
                self.device_ir.push_str(&format!("{}:\n", merge_label));
            }
            ASTNode::WhileStatement {
                condition, body, ..
            } => {
                let cond_label = self.next_block();
                let body_label = self.next_block();
                let exit_label = self.next_block();
                self.device_ir
                    .push_str(&format!("    br label %{}\n", cond_label));
                self.device_ir.push_str(&format!("{}:\n", cond_label));
                let cond_val = self.compile_expression_device(condition);
                let cond_i1 = self.next_register();
                self.device_ir
                    .push_str(&format!("    {} = icmp ne i32 {}, 0\n", cond_i1, cond_val));
                self.device_ir.push_str(&format!(
                    "    br i1 {}, label %{}, label %{}\n",
                    cond_i1, body_label, exit_label
                ));
                self.device_ir.push_str(&format!("{}:\n", body_label));
                for stmt in body {
                    self.compile_statement_device(stmt);
                }
                if !self.is_device_block_terminated() {
                    self.device_ir
                        .push_str(&format!("    br label %{}\n", cond_label));
                }
                self.device_ir.push_str(&format!("{}:\n", exit_label));
            }
            ASTNode::CallExpr { .. } => {
                let _ = self.compile_expression_device(node);
            }
            _ => {
                let _ = self.compile_expression_device(node);
            }
        }
    }

    // ------------------------------------------------------------------------
    // Kernel launch stub (host side) for GPU
    // ------------------------------------------------------------------------
    pub(crate) fn emit_kernel_launch_stub(&mut self, name: &str, params: &[crate::parser::Param]) {
        self.debug_log(&format!(
            "emitting real GPU launch stub for kernel '{}'",
            name
        ));

        self.reset_for_new_function();

        let mut mutable_indices = Vec::new();
        for (i, p) in params.iter().enumerate() {
            if p.ty.trim().starts_with("&mut") {
                mutable_indices.push(i);
            }
        }

        let param_llvm_types: Vec<String> =
            params.iter().map(|p| self.map_type(&p.ty, false)).collect();

        self.debug_emit(&format!(
            "define void @{}_launch({}) {{",
            name,
            params
                .iter()
                .map(|p| format!("{} %{}", self.map_type(&p.ty, false), p.name))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        self.debug_emit("entry:");

        // 1. Allocate device memory for each &mut parameter
        let mut device_ptrs = Vec::new(); // (index, device_ptr_reg, size)
        for &idx in &mutable_indices {
            let size = 4; // TODO: derive from type
            let dev_ptr = self.next_register();
            self.debug_emit(&format!(
                "    {} = call i8* @vox_gpu_malloc(i64 {})",
                dev_ptr, size
            ));
            let cast_ptr = self.next_register();
            let base_ty = param_llvm_types[idx].trim_end_matches('*');
            self.debug_emit(&format!(
                "    {} = bitcast i8* {} to {}*",
                cast_ptr, dev_ptr, base_ty
            ));
            device_ptrs.push((idx, cast_ptr, size));
        }

        // 2. Build an array of argument pointers (void**) – STANDARD HIP METHOD
        let arg_array = self.next_register();
        self.debug_emit(&format!(
            "    {} = alloca i8*, i32 {}, align 8",
            arg_array,
            params.len()
        ));

        for (i, p) in params.iter().enumerate() {
            let gep = self.next_register();
            self.debug_emit(&format!(
                "    {} = getelementptr i8*, i8** {}, i32 {}",
                gep, arg_array, i
            ));
            if mutable_indices.contains(&i) {
                let dev_ptr = device_ptrs
                    .iter()
                    .find(|(idx, _, _)| *idx == i)
                    .unwrap()
                    .1
                    .clone();
                self.debug_emit(&format!("    store i8* {}, i8** {}", dev_ptr, gep));
            } else {
                let param_reg = format!("%{}", p.name);
                let tmp = self.next_register();
                self.debug_emit(&format!("    {} = alloca i32", tmp));
                self.debug_emit(&format!("    store i32 {}, i32* {}", param_reg, tmp));
                let ptr_to_tmp = self.next_register();
                self.debug_emit(&format!("    {} = bitcast i32* {} to i8*", ptr_to_tmp, tmp));
                self.debug_emit(&format!("    store i8* {}, i8** {}", ptr_to_tmp, gep));
            }
        }

        // 3. Load device module (real binary if available, otherwise placeholder)
        let binary_const_name = self.kernel_binary_const.clone();
        let (blob_ptr, blob_size_literal) = if let Some(const_name) = binary_const_name {
            let len = *self.string_len.get(&const_name).unwrap();
            let ptr = self.get_binary_ptr(&const_name);
            (ptr, len.to_string())
        } else {
            let placeholder_name = self.add_string_constant("// Placeholder device binary");
            let len = *self.string_len.get(&placeholder_name).unwrap();
            let ptr = self.get_string_ptr("// Placeholder device binary");
            (ptr, len.to_string())
        };
        self.debug_emit(&format!(
            "    call void @vox_load_device_module(i8* {}, i64 {})",
            blob_ptr, blob_size_literal
        ));

        // 4. Launch kernel with void** arguments
        let kernel_name_ptr = self.get_string_ptr(name);
        let launch_ret_reg = self.next_register();
        self.debug_emit(&format!(
            "    {} = call i32 @vox_launch_kernel_1d(i8* {}, i8** {}, i32 {}, i32 1, i32 32)",
            launch_ret_reg,
            kernel_name_ptr,
            arg_array,
            params.len()
        ));

        // 5. Copy back results
        for (idx, dev_ptr, size) in &device_ptrs {
            let host_ptr = format!("%{}", params[*idx].name);
            let dev_i8 = self.next_register();
            self.debug_emit(&format!(
                "    {} = bitcast {}* {} to i8*",
                dev_i8,
                param_llvm_types[*idx].trim_end_matches('*'),
                dev_ptr
            ));
            self.debug_emit(&format!(
                "    call void @vox_gpu_memcpy_device_to_host(i8* {}, i8* {}, i64 {})",
                host_ptr, dev_i8, size
            ));
        }

        // 6. Free device memory
        for (idx, dev_ptr, _) in &device_ptrs {
            let dev_i8 = self.next_register();
            self.debug_emit(&format!(
                "    {} = bitcast {}* {} to i8*",
                dev_i8,
                param_llvm_types[*idx].trim_end_matches('*'),
                dev_ptr
            ));
            self.debug_emit(&format!("    call void @vox_gpu_free(i8* {})", dev_i8));
        }

        self.debug_emit("    ret void");
        self.debug_emit("}\n");
    }
}
