//! High‑speed LLVM IR generator for Voxlang.
//! Delegates to specialised submodules.

mod device;
mod expr;
mod generic;
mod gpu;
mod helpers;
mod infer;
mod ir_builder;
mod parallel;
mod runtime;
mod stmt;
mod string_const;
mod type_map;

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};
use crate::parser::ASTNode;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

// -----------------------------------------------------------------------------
// CodegenEngine – holds all state for IR generation (string‑based)
// -----------------------------------------------------------------------------
pub struct CodegenEngine {
    pub target_triple: String,
    pub ir: String,
    pub device_ir: String,
    pub device_triple: Option<String>,
    pub device_triple_override: Option<String>,
    pub current_kernel_name: Option<String>,
    pub var_vox_types: HashMap<String, String>,
    pub generic_functions: HashMap<String, (Vec<String>, Vec<String>, String)>,
    pub generic_function_asts: HashMap<String, ASTNode>,
    pub pending_kernel_stubs: Vec<(String, Vec<crate::parser::Param>)>,
    pub kernel_binary_const: Option<String>,
    pub register_counter: usize,
    pub variable_symbols: HashMap<String, (String, String, bool, bool)>,
    pub string_counter: usize,
    pub string_map: HashMap<String, String>,
    pub string_len: HashMap<String, usize>,
    pub pending_strings: Vec<(String, String, usize)>,
    pub pending_workers: Vec<String>,
    pub debug: bool,
    pub block_counter: usize,
    pub worker_counter: usize,
    pub function_return_types: HashMap<String, String>,
    pub current_return_type: Option<String>,
    pub concrete_struct_defs: RefCell<HashMap<String, String>>,
    pub pending_concrete_struct_defs: RefCell<Vec<String>>,
    pub has_error: bool,
    pub has_kernel: bool,
    pub gpu_decls_emitted: bool,
    pub gpu_mode: Option<String>,
    pub kernel_names: HashSet<String>,
    pub struct_generic_params: HashMap<String, Vec<String>>,
    pub llc_path: PathBuf,
    pub clang_path: PathBuf,
    pub lld_path: Option<PathBuf>,
    pub struct_fields: HashMap<String, Vec<(String, String)>>,
    pub in_function: bool,
    pub enum_variants: HashMap<String, HashMap<String, u32>>,
    pub global_variables: HashMap<String, (String, bool)>,
    pub dynamic_array_elem_type: HashMap<String, String>,
    pub string_literal_fat: HashMap<String, String>,
    pub pending_monomorphised_functions: Vec<ASTNode>,
    pub resolved_types: HashMap<String, String>,
    pub imported_modules: Vec<(String, ASTNode)>,
    pub module_prefix: Option<String>,
    pub current_function_name: Option<String>,
    pub block_terminated: bool,

    // Debugging fields to track brace emissions and function nesting
    pub brace_emission_log: Vec<String>,
    pub current_function_stack: Vec<String>,

    // NEW: type aliases from semantic analysis
    pub type_aliases: HashMap<String, String>,
}

impl CodegenEngine {
    pub fn new(target_triple: &str) -> Self {
        let mut engine = Self {
            target_triple: target_triple.to_string(),
            ir: String::new(),
            device_ir: String::new(),
            device_triple: None,
            device_triple_override: None,
            current_kernel_name: None,
            struct_generic_params: HashMap::new(),
            pending_kernel_stubs: Vec::new(),
            kernel_binary_const: None,
            generic_functions: HashMap::new(),
            generic_function_asts: HashMap::new(),
            concrete_struct_defs: RefCell::new(HashMap::new()),
            pending_concrete_struct_defs: RefCell::new(Vec::new()),
            var_vox_types: HashMap::new(),
            register_counter: 1,
            variable_symbols: HashMap::new(),
            string_counter: 0,
            string_map: HashMap::new(),
            string_len: HashMap::new(),
            pending_strings: Vec::new(),
            pending_workers: Vec::new(),
            debug: false,
            block_counter: 0,
            worker_counter: 0,
            function_return_types: HashMap::new(),
            current_return_type: None,
            has_error: false,
            has_kernel: false,
            gpu_decls_emitted: false,
            gpu_mode: None,
            enum_variants: HashMap::new(),
            kernel_names: HashSet::new(),
            llc_path: PathBuf::from("llc"),
            clang_path: PathBuf::from("clang"),
            lld_path: Some(PathBuf::from("ld.lld")),
            struct_fields: HashMap::new(),
            in_function: false,
            global_variables: HashMap::new(),
            dynamic_array_elem_type: HashMap::new(),
            string_literal_fat: HashMap::new(),
            pending_monomorphised_functions: Vec::new(),
            resolved_types: HashMap::new(),
            imported_modules: Vec::new(),
            module_prefix: None,
            current_function_name: None,
            block_terminated: false,
            brace_emission_log: Vec::new(),
            current_function_stack: Vec::new(),
            type_aliases: HashMap::new(),
        };

        let mut option_map = HashMap::new();
        option_map.insert("None".to_string(), 0);
        option_map.insert("Some".to_string(), 1);
        engine
            .enum_variants
            .insert("Option".to_string(), option_map);

        let mut result_map = HashMap::new();
        result_map.insert("Ok".to_string(), 0);
        result_map.insert("Err".to_string(), 1);
        engine
            .enum_variants
            .insert("Result".to_string(), result_map);

        engine
    }

    pub fn set_llvm_paths(&mut self, clang: PathBuf, llc: PathBuf, lld: Option<PathBuf>) {
        self.clang_path = clang;
        self.llc_path = llc;
        self.lld_path = lld;
    }

    pub fn set_debug(&mut self, enabled: bool) {
        self.debug = enabled;
    }

    pub fn set_device_triple_override(&mut self, triple: String) {
        self.device_triple_override = Some(triple);
    }

    pub fn set_gpu_mode(&mut self, mode: Option<&str>) {
        self.gpu_mode = mode.map(|s| s.to_string());
    }

    /// NEW: set the type alias map from the semantic analyser.
    pub fn set_type_aliases(&mut self, aliases: HashMap<String, String>) {
        self.type_aliases = aliases;
    }

    pub fn add_imported_module_ast(&mut self, alias: String, ast: ASTNode) {
        self.debug_log(&format!(
            "adding imported module '{}' for code generation",
            alias
        ));
        self.imported_modules.push((alias, ast));
    }

    pub fn emit_pending_concrete_structs(&mut self) {
        let defs = self.pending_concrete_struct_defs.replace(Vec::new());
        for def in defs {
            self.debug_emit(&def);
        }
    }

    pub fn compile_expression(&mut self, node: &ASTNode, expected_type: Option<&str>) -> String {
        use crate::codegen::expr::{CodegenTarget, ExprEmitter};
        let mut emitter = ExprEmitter {
            engine: self,
            target: CodegenTarget::Host,
            lvalue: false,
            expected_type: expected_type.map(|s| s.to_string()),
        };
        emitter.compile(node)
    }

    pub fn register_generic_function(
        &mut self,
        name: &str,
        generic_params: Vec<String>,
        param_types: Vec<String>,
        return_type: String,
    ) {
        self.generic_functions
            .insert(name.to_string(), (generic_params, param_types, return_type));
    }

    pub fn store_generic_function_ast(&mut self, name: &str, node: ASTNode) {
        self.generic_function_asts.insert(name.to_string(), node);
    }

    pub fn compile_expression_device(&mut self, node: &ASTNode) -> String {
        use crate::codegen::expr::{CodegenTarget, ExprEmitter};
        let mut emitter = ExprEmitter {
            engine: self,
            target: CodegenTarget::Device,
            lvalue: false,
            expected_type: None,
        };
        emitter.compile(node)
    }

    // -------------------------------------------------------------
    // generate() with extreme debugging
    // -------------------------------------------------------------
    pub fn generate(&mut self, ast: &ASTNode) -> String {
        self.debug_log("========== [CODEGEN] generate() START ==========");
        // Helper to print the Program node details
        fn print_program(engine: &mut CodegenEngine, ast: &ASTNode, label: &str) {
            if let ASTNode::Program(stmts, _) = ast {
                engine.debug_log(&format!(
                    "[CODEGEN][{}] Program has {} statements",
                    label,
                    stmts.len()
                ));
                for (i, stmt) in stmts.iter().enumerate() {
                    if let ASTNode::FunctionDef { name, .. } = stmt {
                        engine.debug_log(&format!("  [{}] FunctionDef: {}", i, name));
                    } else {
                        engine.debug_log(&format!("  [{}] {:?}", i, stmt));
                    }
                }
            } else {
                engine.debug_log(&format!("[CODEGEN][{}] AST is NOT a Program!", label));
            }
        }

        print_program(self, ast, "ENTRY");

        self.debug_log("starting IR generation");
        self.collect_strings(ast);
        print_program(self, ast, "after collect_strings");

        self.ir.clear();
        self.emit_module_header();
        if self.has_error {
            self.debug_log("[CODEGEN] has_error after emit_module_header, aborting");
            return String::new();
        }

        self.has_kernel = self.contains_kernel(ast);
        if self.has_kernel && self.gpu_mode.is_some() {
            self.emit_gpu_runtime_declarations();
        }

        // Phase 0: store generic function ASTs
        self.debug_log("Phase 0: storing generic function ASTs");
        if let ASTNode::Program(statements, _) = ast {
            self.debug_log(&format!(
                "[CODEGEN][Phase0] scanning {} statements",
                statements.len()
            ));
            for stmt in statements {
                if let ASTNode::FunctionDef {
                    name,
                    generic_params,
                    ..
                } = stmt
                {
                    if !generic_params.is_empty() {
                        self.store_generic_function_ast(name, stmt.clone());
                        self.debug_log(&format!("Pre‑stored generic function '{}'", name));
                    }
                }
            }
        }
        print_program(self, ast, "after Phase0");

        // Phase 1: emit struct/enum definitions
        self.debug_log("Phase 1: emitting struct/enum definitions");
        if let ASTNode::Program(statements, _) = ast {
            self.debug_log(&format!(
                "[CODEGEN][Phase1] scanning {} statements",
                statements.len()
            ));
            for stmt in statements {
                match stmt {
                    ASTNode::StructDef { .. } | ASTNode::EnumDef { .. } => {
                        self.compile_statement(stmt);
                        if self.has_error {
                            return String::new();
                        }
                    }
                    _ => {}
                }
            }
        }
        print_program(self, ast, "after Phase1");

        // Phase 1.5: pre‑generate concrete struct definitions
        self.debug_log("Phase 1.5: pre‑generating concrete struct definitions");
        for ty in self.resolved_types.values() {
            self.map_type(ty, false);
        }
        self.emit_pending_concrete_structs();
        print_program(self, ast, "after Phase1.5");

        // Phase 2: compile imported modules
        self.debug_log("Phase 2: compiling imported modules");
        let saved_prefix = self.module_prefix.take();
        let imported = std::mem::take(&mut self.imported_modules);
        self.debug_log(&format!(
            "[CODEGEN][Phase2] processing {} imported modules",
            imported.len()
        ));
        for (alias, module_ast) in &imported {
            self.module_prefix = Some(alias.clone());
            self.debug_log(&format!("[CODEGEN][Phase2] compiling module '{}'", alias));
            if let ASTNode::Program(stmts, _) = module_ast {
                self.debug_log(&format!(
                    "  module {} has {} statements",
                    alias,
                    stmts.len()
                ));
                for stmt in stmts {
                    if matches!(stmt, ASTNode::Import { .. }) {
                        continue;
                    }
                    self.compile_statement(stmt);
                    if self.has_error {
                        self.imported_modules = imported;
                        return String::new();
                    }
                }
            }
        }
        self.module_prefix = saved_prefix;
        self.imported_modules = imported;
        print_program(self, ast, "after Phase2");

        // Phase 3: compile original program (skip types already emitted)
        self.debug_log(
            "Phase 3: compiling original program statements (excluding type definitions)",
        );
        match ast {
            ASTNode::Program(statements, _) => {
                self.debug_log(&format!(
                    "[CODEGEN][Phase3] original program has {} statements",
                    statements.len()
                ));
                for (i, stmt) in statements.iter().enumerate() {
                    self.debug_log(&format!(
                        "[CODEGEN][Phase3] processing statement {}: {:?}",
                        i, stmt
                    ));
                    if matches!(stmt, ASTNode::StructDef { .. } | ASTNode::EnumDef { .. }) {
                        continue;
                    }
                    if self.has_error {
                        break;
                    }
                    self.compile_statement(stmt);
                }
            }
            _ => {}
        }
        print_program(self, ast, "after Phase3");

        // Phase 4: deferred monomorphised functions
        self.debug_log("Phase 4: deferred monomorphised function compilation");
        while !self.pending_monomorphised_functions.is_empty() {
            self.debug_log(&format!(
                "[CODEGEN][Phase4] compiling {} monomorphised functions",
                self.pending_monomorphised_functions.len()
            ));
            let pending = std::mem::take(&mut self.pending_monomorphised_functions);
            for func_node in &pending {
                self.compile_statement(func_node);
                if self.has_error {
                    return String::new();
                }
            }
        }
        print_program(self, ast, "after Phase4");

        // Phase 5: emit concrete struct definitions
        self.emit_pending_concrete_structs();

        if self.gpu_mode.is_some() && self.has_kernel && !self.device_ir.is_empty() {
            let triple = self.device_triple.clone();
            if let Some(triple) = triple {
                if let Some(binary) = self.finalize_device_code(&triple) {
                    let binary_const = self.add_binary_constant(&binary);
                    self.kernel_binary_const = Some(binary_const);
                } else {
                    emit_diagnostic(
                        &Diagnostic::warning(
                            "Could not generate GPU binary; kernels will run on CPU stub",
                        )
                        .with_code("VX0419"),
                    );
                }
            }
        }

        if self.gpu_mode.is_some() {
            let stubs = std::mem::take(&mut self.pending_kernel_stubs);
            for (name, params) in stubs {
                self.emit_kernel_launch_stub(&name, &params);
            }
        }

        for worker in &self.pending_workers {
            self.ir.push_str(worker);
            self.ir.push('\n');
        }

        self.emit_string_constants();

        // =================================================================
        // POST‑PROCESSING: preserve all closing braces, keep IR intact
        // =================================================================

        let lines: Vec<String> = self.ir.lines().map(|s| s.to_string()).collect();
        let mut balanced_lines = Vec::new();
        let mut brace_stack = Vec::new(); // track line indices where '{' was seen
        let mut i = 0;

        while i < lines.len() {
            let line = &lines[i];
            let trimmed = line.trim();

            // If we are about to start a new function but there are still unclosed braces,
            // close them first (safety net).
            if trimmed.starts_with("define ") && !brace_stack.is_empty() {
                while let Some(_) = brace_stack.pop() {
                    balanced_lines.push("}".to_string());
                    self.debug_log(
                        "POST‑PROCESSING: added missing closing brace before new function",
                    );
                }
            }

            // Opening brace at the end of a function definition
            if trimmed.starts_with("define ") && trimmed.ends_with('{') {
                balanced_lines.push(line.clone());
                brace_stack.push(balanced_lines.len() - 1);
                i += 1;
                continue;
            }

            // Closing brace: keep it if there is a matching open, otherwise skip (stray brace)
            if trimmed == "}" {
                if let Some(_) = brace_stack.last() {
                    // Properly nested: keep it
                    balanced_lines.push(line.clone());
                    brace_stack.pop();
                } else {
                    // Stray closing brace at top level – discard it
                    self.debug_log("POST‑PROCESSING: removed stray top‑level '}'");
                }
                i += 1;
                continue;
            }

            // Remove top‑level labels that are not inside any function (stray block labels)
            if brace_stack.is_empty() && trimmed.ends_with(':') && !trimmed.starts_with("define") {
                self.debug_log(&format!(
                    "POST‑PROCESSING: removed top‑level label '{}'",
                    trimmed
                ));
                i += 1;
                continue;
            }

            // Remove top‑level `unreachable` instructions outside functions
            if brace_stack.is_empty() && trimmed == "unreachable" {
                self.debug_log("POST‑PROCESSING: removed top‑level unreachable");
                i += 1;
                continue;
            }

            // Normal line – keep it
            balanced_lines.push(line.clone());
            i += 1;
        }

        // After processing all lines, add any missing closing braces (safety)
        while let Some(_) = brace_stack.pop() {
            self.debug_log("POST‑PROCESSING: added missing closing brace at end");
            balanced_lines.push("}".to_string());
        }

        // Remove consecutive blank lines and trim trailing empty lines.
        let mut final_lines = Vec::new();
        let mut last_was_empty = false;
        for line in balanced_lines {
            if line.trim().is_empty() {
                if !last_was_empty {
                    final_lines.push(line);
                    last_was_empty = true;
                }
            } else {
                final_lines.push(line);
                last_was_empty = false;
            }
        }

        // Trim trailing blank lines
        while let Some(last) = final_lines.last() {
            if last.trim().is_empty() {
                final_lines.pop();
            } else {
                break;
            }
        }

        self.ir = final_lines.join("\n");
        if !self.ir.ends_with('\n') {
            self.ir.push('\n');
        }

        if self.debug {
            debug_log(&format!("=== FINAL IR (after repair) ===\n{}", self.ir));
        }

        self.debug_log("========== [CODEGEN] generate() END ==========\n");
        self.ir.clone()
    }

    // ------------------------------------------------------------------------
    // Terminator checking using a boolean flag
    // ------------------------------------------------------------------------
    pub fn is_current_block_terminated(&self) -> bool {
        self.block_terminated
    }

    pub fn reset_block_terminated(&mut self) {
        self.block_terminated = false;
    }
}
