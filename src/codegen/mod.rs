//! High‑speed LLVM IR generator for Voxlang.
//! Delegates to specialised submodules.

mod device;
mod expr;
mod generic;
mod gpu;
mod helpers;
mod infer;
mod ir_builder;
pub mod msl;
mod parallel;
mod runtime;
mod stmt;
mod string_const;
mod type_map;

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};
use crate::parser::{ASTNode, KernelAttr};
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
    pub kernel_binary_const: Option<String>,
    pub register_counter: usize,
    pub alloca_counter: usize,
    pub variable_symbols: HashMap<String, (String, String, bool, bool)>,
    pub string_counter: usize,
    pub string_map: HashMap<String, String>,
    pub string_len: HashMap<String, usize>,
    pub pending_strings: Vec<(String, String, usize)>,
    pub pending_workers: Vec<String>,
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
    pub brace_emission_log: Vec<String>,
    pub current_function_stack: Vec<String>,
    pub type_aliases: HashMap<String, String>,
    pub register_allocations: Vec<(usize, String, String)>,
    pub kernel_attrs: HashMap<String, KernelAttr>,
    pub gpu_arch: Option<String>,
    pub kernel_param_types: HashMap<String, Vec<String>>,
    pub pending_declarations: Vec<String>,
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
            kernel_binary_const: None,
            generic_functions: HashMap::new(),
            generic_function_asts: HashMap::new(),
            concrete_struct_defs: RefCell::new(HashMap::new()),
            pending_concrete_struct_defs: RefCell::new(Vec::new()),
            var_vox_types: HashMap::new(),
            register_counter: 0,
            alloca_counter: 0,
            variable_symbols: HashMap::new(),
            string_counter: 0,
            string_map: HashMap::new(),
            string_len: HashMap::new(),
            pending_strings: Vec::new(),
            pending_workers: Vec::new(),
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
            register_allocations: Vec::new(),
            kernel_attrs: HashMap::new(),
            gpu_arch: None,
            kernel_param_types: HashMap::new(),
            pending_declarations: Vec::new(),
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

    pub fn set_device_triple_override(&mut self, triple: String) {
        self.device_triple_override = Some(triple);
    }

    pub fn set_gpu_mode(&mut self, mode: Option<&str>) {
        self.gpu_mode = mode.map(|s| s.to_string());
    }

    pub fn set_gpu_arch(&mut self, arch: Option<String>) {
        self.gpu_arch = arch;
    }

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

    pub fn get_kernel_param_types(&self, name: &str) -> Option<&Vec<String>> {
        self.kernel_param_types.get(name)
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
    // generate() – cleaned up logging
    // -------------------------------------------------------------
    pub fn generate(&mut self, ast: &ASTNode) -> String {
        self.debug_log("========== [CODEGEN] generate() START ==========");
        self.debug_log("starting IR generation");

        // Collect strings and emit module header
        self.collect_strings(ast);
        self.ir.clear();
        self.emit_module_header();
        if self.has_error {
            self.debug_log("[CODEGEN] has_error after emit_module_header, aborting");
            return String::new();
        }

        // Emit pending forward declarations
        let decls = std::mem::take(&mut self.pending_declarations);
        for decl in decls {
            self.debug_emit(&decl);
        }

        self.has_kernel = self.contains_kernel(ast);
        if self.has_kernel && self.gpu_mode.is_some() {
            self.emit_gpu_runtime_declarations();
        }

        // Phase 0: store generic function ASTs
        self.debug_log("Phase 0: storing generic function ASTs");
        if let ASTNode::Program(statements, _) = ast {
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

        // Phase 1: emit struct/enum definitions
        self.debug_log("Phase 1: emitting struct/enum definitions");
        if let ASTNode::Program(statements, _) = ast {
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

        // Phase 1.5: pre‑generate concrete struct definitions
        self.debug_log("Phase 1.5: pre‑generating concrete struct definitions");
        for ty in self.resolved_types.values() {
            self.map_type(ty, false);
        }
        self.emit_pending_concrete_structs();

        // Phase 2: compile imported modules
        self.debug_log("Phase 2: compiling imported modules");
        let saved_prefix = self.module_prefix.take();
        let imported = std::mem::take(&mut self.imported_modules);
        for (alias, module_ast) in &imported {
            self.module_prefix = Some(alias.clone());
            self.debug_log(&format!("compiling module '{}'", alias));
            if let ASTNode::Program(stmts, _) = module_ast {
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

        // Phase 3: compile original program (skip types and main)
        self.debug_log(
            "Phase 3: compiling original program statements (excluding type definitions and main)",
        );
        let mut main_ast = None;
        if let ASTNode::Program(statements, _) = ast {
            for stmt in statements {
                if matches!(stmt, ASTNode::StructDef { .. } | ASTNode::EnumDef { .. }) {
                    continue;
                }
                if let ASTNode::FunctionDef { name, .. } = stmt {
                    if name == "main" {
                        self.debug_log("[CODEGEN] Deferring compilation of 'main' until after device binary generation");
                        main_ast = Some(stmt.clone());
                        continue;
                    }
                }
                self.compile_statement(stmt);
                if self.has_error {
                    return String::new();
                }
            }
        }

        // Phase 4: deferred monomorphised functions
        self.debug_log("Phase 4: deferred monomorphised function compilation");
        while !self.pending_monomorphised_functions.is_empty() {
            let pending = std::mem::take(&mut self.pending_monomorphised_functions);
            for func_node in &pending {
                self.compile_statement(func_node);
                if self.has_error {
                    return String::new();
                }
            }
        }

        // Phase 5: emit concrete struct definitions
        self.emit_pending_concrete_structs();

        // Generate device binary (PTX/HSACO/Metal) and set kernel_binary_const
        if self.gpu_mode.is_some() && self.has_kernel && !self.device_ir.is_empty() {
            let triple = self.device_triple.clone();
            if let Some(triple) = triple {
                if let Some(mut binary) = self.finalize_device_code(&triple, Some(ast)) {
                    if self.gpu_mode.as_deref() == Some("metal") {
                        binary.push(0);
                        self.debug_log("[CODEGEN] Added null terminator to Metal MSL source");
                    }
                    let binary_const = self.add_binary_constant(&binary);
                    self.kernel_binary_const = Some(binary_const.clone());
                    self.debug_log(&format!(
                        "[CODEGEN] Device binary generated and stored as constant '{}'",
                        binary_const
                    ));
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

        // Now compile main (if it exists)
        if let Some(main_node) = main_ast {
            self.debug_log(
                "[CODEGEN] Compiling deferred 'main' function (after device binary generation)",
            );
            self.compile_statement(&main_node);
            if self.has_error {
                return String::new();
            }
        }

        // Phase 4.5: compile any monomorphised functions generated during main compilation
        while !self.pending_monomorphised_functions.is_empty() {
            let pending = std::mem::take(&mut self.pending_monomorphised_functions);
            for func_node in &pending {
                self.compile_statement(func_node);
                if self.has_error {
                    return String::new();
                }
            }
        }

        for worker in &self.pending_workers {
            self.ir.push_str(worker);
            self.ir.push('\n');
        }

        self.emit_string_constants();

        // POST‑PROCESSING: preserve all closing braces, keep IR intact
        let lines: Vec<String> = self.ir.lines().map(|s| s.to_string()).collect();
        let mut balanced_lines = Vec::new();
        let mut brace_stack = Vec::new();
        let mut i = 0;

        while i < lines.len() {
            let line = &lines[i];
            let trimmed = line.trim();

            if trimmed.starts_with("define ") && !brace_stack.is_empty() {
                while let Some(_) = brace_stack.pop() {
                    balanced_lines.push("}".to_string());
                    self.debug_log(
                        "POST‑PROCESSING: added missing closing brace before new function",
                    );
                }
            }

            if trimmed.starts_with("define ") && trimmed.ends_with('{') {
                balanced_lines.push(line.clone());
                brace_stack.push(balanced_lines.len() - 1);
                i += 1;
                continue;
            }

            if trimmed == "}" {
                if let Some(_) = brace_stack.last() {
                    balanced_lines.push(line.clone());
                    brace_stack.pop();
                } else {
                    self.debug_log("POST‑PROCESSING: removed stray top‑level '}'");
                }
                i += 1;
                continue;
            }

            if brace_stack.is_empty() && trimmed.ends_with(':') && !trimmed.starts_with("define") {
                self.debug_log(&format!(
                    "POST‑PROCESSING: removed top‑level label '{}'",
                    trimmed
                ));
                i += 1;
                continue;
            }

            if brace_stack.is_empty() && trimmed == "unreachable" {
                self.debug_log("POST‑PROCESSING: removed top‑level unreachable");
                i += 1;
                continue;
            }

            balanced_lines.push(line.clone());
            i += 1;
        }

        while let Some(_) = brace_stack.pop() {
            self.debug_log("POST‑PROCESSING: added missing closing brace at end");
            balanced_lines.push("}".to_string());
        }

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

        // -------------------------------------------------------------
        // REGISTER NUMBERING VALIDATION (definitions only)
        // -------------------------------------------------------------
        if crate::diagnostic::global_debug() {
            self.debug_log("=== REGISTER NUMBERING VALIDATION (definitions only) ===");
            let ir_lines: Vec<&str> = self.ir.lines().collect();

            let mut current_func = String::new();
            let mut func_defs: Vec<usize> = Vec::new();
            let mut all_defs: Vec<(String, Vec<usize>)> = Vec::new();

            for line in ir_lines {
                let trimmed = line.trim();

                if trimmed.starts_with("define ") {
                    if !current_func.is_empty() && !func_defs.is_empty() {
                        all_defs.push((current_func.clone(), func_defs.clone()));
                    }
                    if let Some(func_start) = trimmed.find('@') {
                        if let Some(func_end) = trimmed[func_start + 1..].find('(') {
                            current_func =
                                trimmed[func_start + 1..func_start + 1 + func_end].to_string();
                        }
                    }
                    func_defs.clear();
                    continue;
                }

                if trimmed.starts_with("declare ")
                    || trimmed.ends_with(':')
                    || trimmed.starts_with(';')
                    || trimmed.is_empty()
                {
                    continue;
                }

                if let Some(eq_pos) = line.find(" = ") {
                    let before_eq = &line[..eq_pos];
                    let mut chars = before_eq.chars().enumerate();
                    while let Some((_pos, ch)) = chars.next() {
                        if ch == '%' {
                            let mut num_str = String::new();
                            while let Some((_, next_ch)) = chars.next() {
                                if next_ch.is_ascii_digit() {
                                    num_str.push(next_ch);
                                } else {
                                    break;
                                }
                            }
                            if !num_str.is_empty() {
                                let num: usize = num_str.parse().unwrap();
                                func_defs.push(num);
                                break;
                            }
                        }
                    }
                }
            }

            if !current_func.is_empty() && !func_defs.is_empty() {
                all_defs.push((current_func.clone(), func_defs.clone()));
            }

            let mut errors = Vec::new();
            for (func_name, defs) in &all_defs {
                if defs.is_empty() {
                    continue;
                }
                let n = defs.len();
                let mut sorted = defs.clone();
                sorted.sort();
                sorted.dedup();
                let expected: Vec<usize> = (0..n).collect();
                if sorted != expected {
                    let msg = format!(
                        "Function '{}': register definition set mismatch. Expected {:?}, got {:?}",
                        func_name, expected, sorted
                    );
                    errors.push(msg.clone());
                    self.debug_log(&format!("  ❌ {}", msg));
                }
                if let Some(max) = sorted.last() {
                    if *max >= n {
                        let missing: Vec<usize> = (0..n).filter(|i| !sorted.contains(i)).collect();
                        if !missing.is_empty() {
                            let msg = format!(
                                "Function '{}': missing register definitions: {:?}",
                                func_name, missing
                            );
                            errors.push(msg.clone());
                            self.debug_log(&format!("  ❌ {}", msg));
                        }
                    }
                }
            }

            if errors.is_empty() {
                self.debug_log("✓ Register numbering validation passed");
            } else {
                self.debug_log(&format!(
                    "❌ Found {} register numbering errors",
                    errors.len()
                ));
                if let Ok(debug_path) = std::env::current_dir() {
                    let debug_dir = debug_path.join("target").join("debug");
                    let _ = std::fs::create_dir_all(&debug_dir);
                    let debug_file = debug_dir.join("register_errors.txt");
                    let mut error_output = errors.join("\n");
                    error_output.push_str(&format!("\n\n=== IR Snapshot ===\n{}", self.ir));
                    let _ = std::fs::write(&debug_file, error_output);
                    self.debug_log(&format!(
                        "  Register errors written to {}",
                        debug_file.display()
                    ));
                }
            }

            // Check allocated registers vs used (sanity check)
            if !self.register_allocations.is_empty() {
                let mut all_defined = HashSet::new();
                for (_, defs) in &all_defs {
                    for d in defs {
                        all_defined.insert(*d);
                    }
                }
                let allocated_regs: HashSet<usize> = self
                    .register_allocations
                    .iter()
                    .map(|(n, _, _)| *n)
                    .collect();
                let unused: Vec<&usize> = allocated_regs
                    .iter()
                    .filter(|n| !all_defined.contains(*n))
                    .collect();
                if !unused.is_empty() {
                    self.debug_log(&format!("⚠️ Unused register allocations: {:?}", unused));
                    for (reg_num, func, loc) in &self.register_allocations {
                        if unused.contains(&reg_num) {
                            self.debug_log(&format!(
                                "  %{} allocated in {} at {}",
                                reg_num, func, loc
                            ));
                        }
                    }
                }
            }

            self.debug_log("=========================================");
        }

        if crate::diagnostic::global_debug() {
            debug_log(&format!(
                "[CODEGEN] === FINAL IR (after repair) ===\n{}",
                self.ir
            ));
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
