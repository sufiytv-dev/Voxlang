// semantics/mod.rs
//! Semantic analysis: type checking, inference, borrow checking, and name resolution.

mod analyze;
mod builtins;
mod generics;
mod infer;
mod resolver;
mod symbol;
mod types;
mod unify;

// Re‑export public API
pub use symbol::{EnumInfo, QualifiedSymbol, StructInfo, SymbolTable};
pub use types::Type;
pub use unify::UnificationTable;

use crate::bridge::ForeignFunction;
use crate::frontend::span::Span;
use crate::module::ModuleResolver;
use crate::parser::ASTNode;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

// -----------------------------------------------------------------------------
// Semantic analyzer – main structure
// -----------------------------------------------------------------------------

/// The core semantic analysis driver.
/// Performs type inference, borrow checking, refinement validation, and name resolution.
pub struct SemanticAnalyzer<'a> {
    pub symbols: SymbolTable,
    pub(crate) in_kernel: bool,
    pub(crate) current_return_type: Option<Type>,
    pub(crate) current_return_refinement: Option<Box<ASTNode>>,
    pub(crate) current_function_name: Option<String>,
    pub(crate) error_occurred: bool,
    pub(crate) debug: bool,
    pub(crate) borrowed_in_scope: Vec<Vec<(String, Span)>>,
    pub(crate) path_conditions: Vec<Vec<ASTNode>>,
    pub(crate) current_param_refinements: Vec<(String, Option<Box<ASTNode>>)>,
    pub(crate) module_resolver: Option<&'a mut ModuleResolver>,
    pub(crate) processing_paths: Vec<PathBuf>,
    pub(crate) current_generic_params: Option<Vec<String>>,
    pub concrete_enum_instances: HashMap<String, Vec<String>>,
    // monomorphisation support (reserved for future use)
    #[allow(dead_code)]
    pub(crate) monomorphised_functions: HashMap<String, ASTNode>,
    #[allow(dead_code)]
    pub(crate) pending_monomorphised_functions: Vec<ASTNode>,
    #[allow(dead_code)]
    pub(crate) monomorphising_in_progress: HashSet<String>,
    // bidirectional inference
    pub(crate) unify: UnificationTable,
    pub resolved_variable_types: HashMap<String, String>,
    // imported modules for code generation
    pub imported_modules: Vec<(String, ASTNode)>,
    // use imports mapping local name -> fully qualified name
    pub(crate) use_imports: HashMap<String, String>,
}

impl<'a> SemanticAnalyzer<'a> {
    /// Create a new semantic analyzer with an empty symbol table and fresh inference state.
    pub fn new() -> Self {
        let debug = std::env::var("SEM_DEBUG").is_ok();
        if debug {
            crate::diagnostic::debug_log("[SEM] SemanticAnalyzer debug ENABLED".to_string());
        }
        Self {
            symbols: SymbolTable::new(),
            in_kernel: false,
            current_return_type: None,
            current_return_refinement: None,
            current_function_name: None,
            error_occurred: false,
            debug,
            borrowed_in_scope: vec![Vec::new()],
            path_conditions: vec![Vec::new()],
            current_param_refinements: Vec::new(),
            module_resolver: None,
            processing_paths: Vec::new(),
            current_generic_params: None,
            concrete_enum_instances: HashMap::new(),
            monomorphised_functions: HashMap::new(),
            pending_monomorphised_functions: Vec::new(),
            monomorphising_in_progress: HashSet::new(),
            unify: UnificationTable::new(),
            resolved_variable_types: HashMap::new(),
            imported_modules: Vec::new(),
            use_imports: HashMap::new(),
        }
    }

    /// Create a new analyzer that can resolve imports using the given module resolver.
    pub fn with_resolver(resolver: &'a mut ModuleResolver) -> Self {
        let mut s = Self::new();
        s.module_resolver = Some(resolver);
        s
    }

    /// Take ownership of the list of imported modules (ASTs) for later code generation.
    pub fn take_imported_modules(&mut self) -> Vec<(String, ASTNode)> {
        std::mem::take(&mut self.imported_modules)
    }

    /// Internal debug logging (respects `SEM_DEBUG` environment variable).
    pub(crate) fn dbg(&self, msg: &str) {
        if self.debug {
            crate::diagnostic::debug_log(format!("[SEM] {}", msg));
        }
    }

    /// Register FFI function signatures so they are known during semantic analysis.
    pub fn register_ffi_signatures(&mut self, foreign_fns: Vec<ForeignFunction>) {
        let empty_set = HashSet::new();
        for f in foreign_fns {
            let param_types: Vec<Type> = f
                .param_types
                .iter()
                .map(|s| self.parse_type_str_with_imports(s, &empty_set))
                .collect();
            let return_type = self.parse_type_str_with_imports(&f.return_type, &empty_set);
            // FFI functions are never kernels → is_kernel = false
            self.symbols.register_function(
                &f.name,
                param_types,
                vec![None; f.param_types.len()],
                return_type,
                None,
                vec![],
                false,
            );
        }
    }

    /// Perform semantic analysis on the given AST node.
    /// Returns `true` if no errors were reported.
    pub fn check(&mut self, node: &ASTNode) -> bool {
        self.analyze_statement(node);
        !self.error_occurred
    }

    /// Look up the resolved concrete type for a variable using a qualified key.
    /// The key should be of the form `"<function>::<variable>"` or `"global::<variable>"`.
    /// This is the preferred method for code generation.
    pub fn get_resolved_type_qualified(
        &self,
        func_name: Option<&str>,
        var_name: &str,
    ) -> Option<String> {
        let key = if let Some(f) = func_name {
            format!("{}::{}", f, var_name)
        } else {
            format!("global::{}", var_name)
        };
        self.resolved_variable_types.get(&key).cloned()
    }

    /// Legacy: Look up resolved type using an unqualified name.
    /// This only returns types stored with a `"global::"` prefix.
    pub fn get_resolved_type(&self, name: &str) -> Option<String> {
        self.get_resolved_type_qualified(None, name)
    }
}

// Pull in all the method implementations from the sub‑modules.
// The actual `impl SemanticAnalyzer` blocks are spread across:
//   - infer.rs   (fresh_infer_var, fresh_tmp_var, parse_type_str_with_imports, resolve_type,
//                 extract_array_element_type, is_array_compatible, strip_references)
//   - generics.rs (substitute_type_in_string, substitute_types_in_node, unify_generic_parameter)
//   - resolver.rs (process_import, resolve_use_decl)
//   - analyze.rs  (has_return_in_stmts, check_refinement, extract_condition, resolve_lvalue_type,
//                 solve_constraints, collect_resolved_types, analyze_statement, analyze_expression,
//                 is_integer_type, is_arithmetic_type, and the helper `node_span`)

// The `use` statements above already bring the modules into scope,
// and the compiler will see the impl blocks from those files.

// -----------------------------------------------------------------------------
// Helper: register built‑in types at symbol table creation
// -----------------------------------------------------------------------------
// This is called from `SymbolTable::new()` – we need to wire it up.
// The `builtins` module provides `register_builtins`.
// We modify `symbol.rs` to call it, but to avoid circular dependencies,
// we place the call inside `SymbolTable::new` using a `cfg` or a direct call.
// The `symbol.rs` file already contains a placeholder comment.
// We'll update `symbol.rs` to import `crate::semantics::builtins::register_builtins`
// and call it at the end of `new()`.  That is done in the final `symbol.rs` we provided.
