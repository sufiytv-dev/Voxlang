// gpu.rs - GPU kernel compilation for Voxlang
//
// UPDATED for PTX integration:
// - Stores kernel block attributes in CodegenEngine (kernel_attrs map)
// - No longer emits launch stubs – launch is handled directly via ASTNode::KernelLaunch
// - FIXED: replaced opaque `ptr` with explicit pointer types using `device_ptr_type`
// - FIXED: Proper pointee type derivation for dereference assignment
// - NEW: Stores kernel parameter types for later use during launch code generation.

use crate::codegen::CodegenEngine;
use crate::diagnostic::{Diagnostic, emit_diagnostic};
use crate::parser::{ASTNode, KernelAttr};
use std::collections::HashMap;

impl CodegenEngine {
    // ------------------------------------------------------------------------
    // Device function (kernel) compilation – stores block attribute and param types
    // ------------------------------------------------------------------------
    pub(crate) fn compile_device_function(
        &mut self,
        name: &str,
        params: &[crate::parser::Param],
        body: &[ASTNode],
        attr: &KernelAttr,
    ) {
        self.debug_log(&format!("compiling device function '{}'", name));

        // Store the kernel attribute for later use during launch code generation
        self.kernel_attrs.insert(name.to_string(), attr.clone());
        // Record this kernel name for NVVM metadata
        self.kernel_names.insert(name.to_string());

        // Store the parameter types (Vox types) for this kernel – used by launch code
        let param_types: Vec<String> = params.iter().map(|p| p.ty.clone()).collect();
        self.kernel_param_types
            .insert(name.to_string(), param_types);

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
            // Use explicit pointer type for device IR (no opaque `ptr`)
            let ptr_ty = self.device_ptr_type(&elem_ty);
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
                // Use explicit pointer type for device IR
                let ptr_ty = self.device_ptr_type(&elem_ty);
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
                        // Get the LLVM pointer type for the store instruction.
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

                        // Determine the pointee LLVM type from the Vox type of the inner variable.
                        // This avoids relying on the string representation of the pointer type.
                        let pointee_ty = if let ASTNode::Identifier(ref_name, _) = &**inner {
                            if let Some(vox_ty) = self.var_vox_types.get(ref_name) {
                                // Strip the reference (&mut or &) to get the pointee Vox type.
                                let inner_vox = if let Some(stripped) = vox_ty.strip_prefix("&mut ")
                                {
                                    stripped
                                } else if let Some(stripped) = vox_ty.strip_prefix("& ") {
                                    stripped
                                } else {
                                    vox_ty.as_str()
                                };
                                self.map_type(inner_vox, true) // true = device target
                            } else {
                                // Fallback: try to infer from the variable's LLVM type (if it's a pointer type)
                                if ptr_ty.ends_with('*') {
                                    let base = ptr_ty.trim_end_matches('*');
                                    if base == "ptr" {
                                        "i32".to_string() // best guess
                                    } else {
                                        base.to_string()
                                    }
                                } else {
                                    "i32".to_string()
                                }
                            }
                        } else {
                            // Fallback for non-identifier dereference (e.g., from borrow)
                            "i32".to_string()
                        };

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
}
