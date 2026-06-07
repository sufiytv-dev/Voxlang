// parallel.rs - Parallel loop worker generation for Voxlang
use crate::codegen::CodegenEngine;
use crate::parser::ASTNode;
use std::mem;

impl CodegenEngine {
    /// Collects all variable names that are captured from the outer scope inside the given statements.
    pub(crate) fn collect_captured_vars(&self, stmts: &[&ASTNode], out: &mut Vec<String>) {
        for stmt in stmts {
            match stmt {
                ASTNode::Identifier(name, _) => {
                    if self.variable_symbols.contains_key(name) {
                        out.push(name.clone());
                    }
                }
                ASTNode::VariableDecl { value, .. } => {
                    self.collect_captured_vars(&[value.as_ref()], out);
                }
                ASTNode::Assignment { lhs, value, .. } => {
                    self.collect_captured_vars(&[lhs.as_ref()], out);
                    self.collect_captured_vars(&[value.as_ref()], out);
                }
                ASTNode::IfStatement {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    self.collect_captured_vars(&[condition.as_ref()], out);
                    self.collect_captured_vars(&then_branch.iter().collect::<Vec<_>>(), out);
                    if let Some(b) = else_branch {
                        self.collect_captured_vars(&b.iter().collect::<Vec<_>>(), out);
                    }
                }
                ASTNode::WhileStatement {
                    condition, body, ..
                } => {
                    self.collect_captured_vars(&[condition.as_ref()], out);
                    self.collect_captured_vars(&body.iter().collect::<Vec<_>>(), out);
                }
                ASTNode::ReturnStatement(Some(expr), _) => {
                    self.collect_captured_vars(&[expr.as_ref()], out);
                }
                ASTNode::CastExpr { expr, .. } => {
                    self.collect_captured_vars(&[expr.as_ref()], out);
                }
                ASTNode::CallExpr { args, .. } => {
                    let arg_refs: Vec<&ASTNode> = args.iter().map(|a| a as &ASTNode).collect();
                    self.collect_captured_vars(&arg_refs, out);
                }
                ASTNode::BorrowExpr { expr, .. } => {
                    self.collect_captured_vars(&[expr.as_ref()], out);
                }
                ASTNode::DerefExpr(expr, _) => {
                    self.collect_captured_vars(&[expr.as_ref()], out);
                }
                ASTNode::ComptimeBlock { body, .. } => {
                    for stmt in body {
                        self.collect_captured_vars(&[stmt], out);
                    }
                }
                _ => {}
            }
        }
    }

    /// Generates a worker function for a parallel loop.
    /// The worker receives an index and a context pointer, executes the loop body for that index,
    /// and then atomically adds the delta of any captured variables back to the context.
    pub(crate) fn generate_worker_function(
        &mut self,
        name: &str,
        iter_var: &str,
        _captured: &[String],
        body: &[ASTNode],
        ctx_type: &str,
        ctx_fields: &[(String, String)],
    ) {
        self.debug_log(&format!("generating worker function '{}'", name));

        // Save outer engine state (IR buffer and symbol table)
        let original_ir = mem::take(&mut self.ir);
        let original_symbols = mem::take(&mut self.variable_symbols);
        let saved_register_counter = self.register_counter;
        let saved_block_counter = self.block_counter;

        // Reset counters for the worker function (register/block names)
        self.reset_for_new_function();

        let mut worker_ir = String::new();
        worker_ir.push_str(&format!(
            "define internal void @{}(i64 %index, i8* %context) {{\n",
            name
        ));
        worker_ir.push_str("entry:\n");

        let idx_i32 = self.next_register();
        worker_ir.push_str(&format!("    {} = trunc i64 %index to i32\n", idx_i32));

        let iter_alloc = self.fresh_alloca_name(iter_var);
        worker_ir.push_str(&format!("    {} = alloca i32\n", iter_alloc));
        worker_ir.push_str(&format!("    store i32 {}, i32* {}\n", idx_i32, iter_alloc));
        self.variable_symbols.insert(
            iter_var.to_string(),
            ("i32".to_string(), iter_alloc, false, false),
        );

        let ctx_ptr = self.next_register();
        worker_ir.push_str(&format!(
            "    {} = bitcast i8* %context to {}*\n",
            ctx_ptr, ctx_type
        ));

        // Store initial values for delta calculation
        let mut init_regs = Vec::new();

        for (i, (name, ty)) in ctx_fields.iter().enumerate() {
            let field_ptr = self.next_register();
            worker_ir.push_str(&format!(
                "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}\n",
                field_ptr, ctx_type, ctx_type, ctx_ptr, i
            ));
            let init_val = self.next_register();
            init_regs.push(init_val.clone());
            worker_ir.push_str(&format!(
                "    {} = load {}, {}* {}\n",
                init_val, ty, ty, field_ptr
            ));
            let alloc_reg = self.fresh_alloca_name(name);
            worker_ir.push_str(&format!("    {} = alloca {}\n", alloc_reg, ty));
            worker_ir.push_str(&format!(
                "    store {} {}, {}* {}\n",
                ty, init_val, ty, alloc_reg
            ));
            self.variable_symbols
                .insert(name.clone(), (ty.clone(), alloc_reg, false, false));
        }

        // Compile loop body into the worker IR buffer
        self.ir = worker_ir;
        for stmt in body {
            self.compile_statement(stmt);
            if self.has_error {
                break;
            }
        }
        worker_ir = mem::take(&mut self.ir);

        // Write back deltas using atomic addition
        for (i, (name, ty)) in ctx_fields.iter().enumerate() {
            let field_ptr = self.next_register();
            worker_ir.push_str(&format!(
                "    {} = getelementptr inbounds {}, {}* {}, i32 0, i32 {}\n",
                field_ptr, ctx_type, ctx_type, ctx_ptr, i
            ));
            let local_alloc = self.variable_symbols.get(name).unwrap().1.clone();
            let final_val = self.next_register();
            worker_ir.push_str(&format!(
                "    {} = load {}, {}* {}\n",
                final_val, ty, ty, local_alloc
            ));
            let delta = self.next_register();
            worker_ir.push_str(&format!(
                "    {} = sub {} {}, {}\n",
                delta, ty, final_val, init_regs[i]
            ));
            worker_ir.push_str(&format!(
                "    atomicrmw add {}* {}, {} {} seq_cst\n",
                ty, field_ptr, ty, delta
            ));
        }

        worker_ir.push_str("    ret void\n");
        worker_ir.push_str("}\n\n");

        // Restore outer engine state
        self.ir = original_ir;
        self.variable_symbols = original_symbols;
        self.register_counter = saved_register_counter;
        self.block_counter = saved_block_counter;

        self.pending_workers.push(worker_ir);
    }
}
