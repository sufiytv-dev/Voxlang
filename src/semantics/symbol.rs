// semantics/symbol.rs
//! Symbol table, scopes, variable state, and name resolution for functions, structs, enums, and modules.

use crate::diagnostic::{Diagnostic, Suggestion, emit_diagnostic};
use crate::frontend::span::Span;
use crate::module::ModuleSymbolTable;
use crate::parser::{ASTNode, EnumVariant, Param, StructField};
use crate::semantics::builtins::register_builtins; // <-- import built-in registration
use crate::semantics::types::Type;
use std::collections::{HashMap, HashSet};

// -----------------------------------------------------------------------------
// Variable state for move semantics + borrowing
// -----------------------------------------------------------------------------
#[derive(Clone, PartialEq)]
pub(crate) enum VarState {
    Alive,
    Moved,
    BorrowedShared(usize),
    BorrowedMut,
}

pub(crate) struct VarInfo {
    pub(crate) ty: Type,
    pub(crate) state: VarState,
    pub(crate) device: bool,
    pub(crate) mutable: bool,
}

// -----------------------------------------------------------------------------
// Extended enum information with structured payload types
// -----------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct EnumInfo {
    pub variants: Vec<EnumVariant>,
    pub generic_params: Vec<String>,
    pub variant_payload_type: Vec<Option<Type>>,
}

// -----------------------------------------------------------------------------
// Struct information with generic parameters and structured fields
// -----------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct StructInfo {
    pub fields: Vec<(String, Type)>,
    pub generic_params: Vec<String>,
}

// -----------------------------------------------------------------------------
// Symbol table with structured types for functions, structs, enums, variables, and modules
// -----------------------------------------------------------------------------
pub struct SymbolTable {
    pub(crate) scopes: Vec<HashMap<String, VarInfo>>,
    functions: HashMap<
        String,
        (
            Vec<Type>,
            Vec<Option<Box<ASTNode>>>,
            Type,
            Option<Box<ASTNode>>,
            Vec<String>,
        ),
    >,
    pub fn_defs: HashMap<
        String,
        (
            ASTNode,
            Vec<String>,
            Vec<Param>,
            Type,
            Option<Box<ASTNode>>,
            Vec<ASTNode>,
            Span,
        ),
    >,
    pub(crate) structs: HashMap<String, StructInfo>,
    pub(crate) enums: HashMap<String, EnumInfo>,
    pub(crate) modules: HashMap<String, ModuleSymbolTable>,
    pub(crate) concrete_structs: HashMap<String, Vec<(String, Type)>>,
    pub(crate) resolved_types: HashMap<String, String>,
    pub type_aliases: HashMap<String, String>,
    debug: bool,
}

#[derive(Clone)]
pub enum QualifiedSymbol {
    Function {
        param_types: Vec<Type>,
        param_refinements: Vec<Option<Box<ASTNode>>>,
        return_type: Type,
        return_refinement: Option<Box<ASTNode>>,
        generic_params: Vec<String>,
    },
    Struct(Vec<(String, Type)>),
    Enum(Vec<EnumVariant>),
}

impl SymbolTable {
    pub fn new() -> Self {
        let debug = std::env::var("SEM_DEBUG").is_ok();
        if debug {
            crate::diagnostic::debug_log("[SEM] SymbolTable debug ENABLED".to_string());
        }
        let mut st = Self {
            scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            fn_defs: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
            modules: HashMap::new(),
            concrete_structs: HashMap::new(),
            resolved_types: HashMap::new(),
            type_aliases: HashMap::new(),
            debug,
        };
        // Register built‑in Option, Result, Vec, HashMap
        register_builtins(&mut st);
        st
    }

    pub(crate) fn dbg(&self, msg: &str) {
        if self.debug {
            crate::diagnostic::debug_log(format!("[SEM] {}", msg));
        }
    }

    /// Strip generic type arguments from a type name, e.g., `Option<i32>` -> `Option`
    pub(crate) fn strip_generic_args(ty: &str) -> String {
        if let Some(angle_pos) = ty.find('<') {
            ty[..angle_pos].to_string()
        } else {
            ty.to_string()
        }
    }

    /// Get the base name of a generic type (without arguments) if it is a known enum.
    pub fn base_enum_name(ty: &str) -> Option<String> {
        let stripped = Self::strip_generic_args(ty);
        if stripped == "Option" || stripped == "Result" {
            Some(stripped)
        } else {
            None
        }
    }

    /// Resolve a concrete struct type (e.g., `Vec<i32>`) to its substituted field types.
    pub fn resolve_concrete_struct(
        &mut self,
        ty: &Type,
        span: Span,
    ) -> Option<Vec<(String, Type)>> {
        let key = ty.to_string();
        if let Some(cached) = self.concrete_structs.get(&key) {
            return Some(cached.clone());
        }

        let (base_name, concrete_args) = match ty {
            Type::Struct(name, args) => (name.clone(), args.clone()),
            Type::Concrete(s) => {
                let info = self.structs.get(s)?;
                let fields = info.fields.clone();
                self.concrete_structs.insert(s.clone(), fields.clone());
                return Some(fields);
            }
            _ => {
                emit_diagnostic(
                    &Diagnostic::error(&format!("Expected struct type, got {}", ty.to_string()))
                        .with_code("VX9006")
                        .with_span(span),
                );
                return None;
            }
        };

        let struct_info = self.structs.get(&base_name)?;
        let generic_params = &struct_info.generic_params;

        if concrete_args.len() != generic_params.len() {
            emit_diagnostic(
                &Diagnostic::error(&format!(
                    "Wrong number of generic arguments for struct '{}': expected {}, got {}",
                    base_name,
                    generic_params.len(),
                    concrete_args.len()
                ))
                .with_code("VX9006")
                .with_span(span),
            );
            return None;
        }

        let mut subst = HashMap::new();
        for (gp, conc) in generic_params.iter().zip(concrete_args.iter()) {
            subst.insert(gp.clone(), conc.clone());
        }

        let mut fields = Vec::new();
        for (field_name, field_ty) in &struct_info.fields {
            let substituted = field_ty.substitute(&subst);
            fields.push((field_name.clone(), substituted));
        }

        self.concrete_structs.insert(key, fields.clone());
        Some(fields)
    }

    pub fn enter_scope(&mut self) {
        self.dbg(&format!("enter_scope (depth {})", self.scopes.len()));
        self.scopes.push(HashMap::new());
    }

    pub fn exit_scope(&mut self) -> bool {
        self.dbg(&format!("exit_scope (depth {})", self.scopes.len()));
        if self.scopes.len() > 1 {
            self.scopes.pop();
            true
        } else {
            emit_diagnostic(
                &Diagnostic::error("Internal compiler error: attempted to pop root scope.")
                    .with_code("VX0200"),
            );
            false
        }
    }

    pub fn insert_type(
        &mut self,
        name: &str,
        ty: Type,
        device: bool,
        mutable: bool,
        span: Span,
    ) -> bool {
        self.dbg(&format!(
            "insert variable '{}' type '{:?}' mutable={} device={} (scope depth {})",
            name,
            ty,
            mutable,
            device,
            self.scopes.len()
        ));
        if let Some(current_scope) = self.scopes.last_mut() {
            if current_scope.contains_key(name) {
                emit_diagnostic(
                    &Diagnostic::error(&format!("Redeclaration of identifier '{}'", name))
                        .with_code("VX0201")
                        .with_span(span)
                        .with_suggestion(Suggestion {
                            message: "Use a different name or remove the previous declaration."
                                .to_string(),
                            span: Some(span),
                        }),
                );
                return false;
            }
            current_scope.insert(
                name.to_string(),
                VarInfo {
                    ty,
                    state: VarState::Alive,
                    device,
                    mutable,
                },
            );
            true
        } else {
            emit_diagnostic(
                &Diagnostic::error("Internal error: no scope active").with_code("VX0202"),
            );
            false
        }
    }

    // Legacy string‑based insertion (converts to Type::Concrete)
    pub fn insert(
        &mut self,
        name: &str,
        ty_str: &str,
        device: bool,
        mutable: bool,
        span: Span,
    ) -> bool {
        self.insert_type(
            name,
            Type::Concrete(ty_str.to_string()),
            device,
            mutable,
            span,
        )
    }

    pub fn mark_moved(&mut self, name: &str, span: Span) -> bool {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(info) = scope.get_mut(name) {
                match info.state {
                    VarState::BorrowedMut => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot move '{}' because it is mutably borrowed.",
                                name
                            ))
                            .with_code("VX0203")
                            .with_span(span),
                        );
                        return false;
                    }
                    VarState::BorrowedShared(count) if count > 0 => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot move '{}' because it is borrowed ({} active references).",
                                name, count
                            ))
                            .with_code("VX0204")
                            .with_span(span),
                        );
                        return false;
                    }
                    _ => {
                        info.state = VarState::Moved;
                        return true;
                    }
                }
            }
        }
        emit_diagnostic(
            &Diagnostic::error(&format!(
                "Internal error: variable '{}' not found to mark moved",
                name
            ))
            .with_code("VX0205"),
        );
        false
    }

    pub fn borrow(&mut self, name: &str, mutable: bool, span: Span) -> Option<Type> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(info) = scope.get_mut(name) {
                if info.state == VarState::Moved {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Cannot borrow moved value '{}'", name))
                            .with_code("VX0206")
                            .with_span(span),
                    );
                    return None;
                }
                if mutable && !info.mutable {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Cannot mutably borrow immutable variable '{}'. Declare it as `mut {}: ...`.", name, name))
                            .with_code("VX0207")
                            .with_span(span)
                            .with_suggestion(Suggestion {
                                message: format!("Add `mut` before the variable name: `mut {}: ...`", name),
                                span: Some(span),
                            }),
                    );
                    return None;
                }
                match info.state {
                    VarState::Alive => {
                        if mutable {
                            info.state = VarState::BorrowedMut;
                            return Some(Type::Reference(true, Box::new(info.ty.clone())));
                        } else {
                            info.state = VarState::BorrowedShared(1);
                            return Some(Type::Reference(false, Box::new(info.ty.clone())));
                        }
                    }
                    VarState::BorrowedShared(count) => {
                        if mutable {
                            emit_diagnostic(
                                &Diagnostic::error(&format!("Cannot mutably borrow '{}' because it is already immutably borrowed ({} active references).", name, count))
                                    .with_code("VX0208")
                                    .with_span(span),
                            );
                            return None;
                        } else {
                            info.state = VarState::BorrowedShared(count + 1);
                            return Some(Type::Reference(false, Box::new(info.ty.clone())));
                        }
                    }
                    VarState::BorrowedMut => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot borrow '{}' because it is already mutably borrowed.",
                                name
                            ))
                            .with_code("VX0209")
                            .with_span(span),
                        );
                        return None;
                    }
                    VarState::Moved => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!("Cannot borrow moved value '{}'", name))
                                .with_code("VX0210")
                                .with_span(span),
                        );
                        return None;
                    }
                }
            }
        }
        emit_diagnostic(
            &Diagnostic::error(&format!("Undeclared identifier '{}'", name))
                .with_code("VX0211")
                .with_span(span),
        );
        None
    }

    pub fn release_borrow(&mut self, name: &str, _span: Span) -> bool {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(info) = scope.get_mut(name) {
                match info.state {
                    VarState::BorrowedShared(count) => {
                        if count > 1 {
                            info.state = VarState::BorrowedShared(count - 1);
                        } else {
                            info.state = VarState::Alive;
                        }
                        return true;
                    }
                    VarState::BorrowedMut => {
                        info.state = VarState::Alive;
                        return true;
                    }
                    _ => {}
                }
            }
        }
        self.dbg(&format!(
            "release_borrow: '{}' not found (already released)",
            name
        ));
        true
    }

    pub fn lookup(&self, name: &str) -> Option<String> {
        self.lookup_info(name)
            .and_then(|(ty, usable, _)| if usable { Some(ty.to_string()) } else { None })
    }

    pub fn lookup_info(&self, name: &str) -> Option<(&Type, bool, bool)> {
        self.dbg(&format!(
            "lookup_info '{}' at depth {}",
            name,
            self.scopes.len()
        ));
        for (depth, scope) in self.scopes.iter().rev().enumerate() {
            if let Some(info) = scope.get(name) {
                self.dbg(&format!("  found '{}' at depth offset {}", name, depth));
                let usable = match info.state {
                    VarState::Alive | VarState::BorrowedShared(_) => true,
                    VarState::Moved => false,
                    VarState::BorrowedMut => SymbolTable::is_copy_type(&info.ty.to_string()),
                };
                return Some((&info.ty, usable, info.mutable));
            }
        }
        self.dbg(&format!("  NOT FOUND '{}'", name));
        None
    }

    pub fn lookup_state(&self, name: &str) -> Option<(String, bool, bool)> {
        self.lookup_info(name)
            .map(|(ty, usable, mutable)| (ty.to_string(), usable, mutable))
    }

    pub fn is_mutable(&self, name: &str) -> bool {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.get(name) {
                return info.mutable;
            }
        }
        false
    }

    pub fn is_device_var(&self, name: &str) -> bool {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.get(name) {
                return info.device;
            }
        }
        false
    }

    pub fn register_function(
        &mut self,
        name: &str,
        param_types: Vec<Type>,
        param_refinements: Vec<Option<Box<ASTNode>>>,
        return_type: Type,
        return_refinement: Option<Box<ASTNode>>,
        generic_params: Vec<String>,
    ) {
        self.functions.insert(
            name.to_string(),
            (
                param_types,
                param_refinements,
                return_type,
                return_refinement,
                generic_params,
            ),
        );
    }

    pub fn register_function_def(
        &mut self,
        name: &str,
        generic_params: Vec<String>,
        params: Vec<Param>,
        return_type: String,
        return_refinement: Option<Box<ASTNode>>,
        body: Vec<ASTNode>,
        span: Span,
    ) {
        use crate::semantics::types::parse_type_str;
        let gp_set: HashSet<_> = generic_params.iter().cloned().collect();
        let return_ty = parse_type_str(&return_type, &gp_set);
        let func_node = ASTNode::FunctionDef {
            name: name.to_string(),
            generic_params: generic_params.clone(),
            params: params.clone(),
            return_type: return_type.clone(),
            return_refinement: return_refinement.clone(),
            body: body.clone(),
            span,
        };
        self.fn_defs.insert(
            name.to_string(),
            (
                func_node,
                generic_params.clone(),
                params.clone(),
                return_ty.clone(),
                return_refinement.clone(),
                body.clone(),
                span,
            ),
        );
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| parse_type_str(&p.ty, &gp_set))
            .collect();
        let param_refinements: Vec<Option<Box<ASTNode>>> =
            params.iter().map(|p| p.refinement.clone()).collect();
        self.register_function(
            name,
            param_types,
            param_refinements,
            return_ty,
            return_refinement,
            generic_params,
        );
    }

    pub fn lookup_function(
        &self,
        name: &str,
    ) -> Option<(
        Vec<Type>,
        Vec<Option<Box<ASTNode>>>,
        Type,
        Option<Box<ASTNode>>,
        Vec<String>,
    )> {
        self.functions.get(name).cloned()
    }

    pub fn register_struct(
        &mut self,
        name: &str,
        generic_params: Vec<String>,
        fields: Vec<StructField>,
    ) {
        use crate::semantics::types::parse_type_str;
        let gp_set: HashSet<_> = generic_params.iter().cloned().collect();
        let struct_fields = fields
            .into_iter()
            .map(|f| (f.name, parse_type_str(&f.ty, &gp_set)))
            .collect();
        self.structs.insert(
            name.to_string(),
            StructInfo {
                fields: struct_fields,
                generic_params,
            },
        );
    }

    pub fn lookup_struct_info(&self, name: &str) -> Option<&StructInfo> {
        let stripped = Self::strip_generic_args(name);
        self.structs.get(&stripped)
    }

    pub fn lookup_struct(&self, name: &str) -> Option<Vec<StructField>> {
        self.lookup_struct_info(name).map(|info| {
            info.fields
                .iter()
                .map(|(n, ty)| StructField {
                    name: n.clone(),
                    ty: ty.to_string(),
                    span: Span::dummy(),
                })
                .collect()
        })
    }

    pub fn register_enum(
        &mut self,
        name: &str,
        generic_params: Vec<String>,
        variants: Vec<EnumVariant>,
        payload_types: Vec<Option<Type>>,
    ) {
        self.enums.insert(
            name.to_string(),
            EnumInfo {
                variants,
                generic_params,
                variant_payload_type: payload_types,
            },
        );
    }

    pub fn lookup_enum(&self, name: &str) -> Option<&EnumInfo> {
        let stripped = Self::strip_generic_args(name);
        self.enums.get(&stripped)
    }

    pub fn register_module(&mut self, alias: String, symbols: ModuleSymbolTable) -> bool {
        if self.modules.contains_key(&alias) {
            emit_diagnostic(
                &Diagnostic::error(&format!("Module alias '{}' already in use", alias))
                    .with_code("VX0309")
                    .with_span(Span::new(0, 0, 0, 0)),
            );
            return false;
        }
        self.modules.insert(alias, symbols);
        true
    }

    pub fn lookup_module(&self, name: &str) -> Option<&ModuleSymbolTable> {
        self.modules.get(name)
    }

    pub fn lookup_qualified(&self, module_name: &str, item_name: &str) -> Option<QualifiedSymbol> {
        let module = self.modules.get(module_name)?;
        if let Some((params, refinements, ret_ty, ret_ref, generic_params)) =
            module.functions.get(item_name)
        {
            let param_types = params.iter().map(|s| Type::Concrete(s.clone())).collect();
            let return_type = Type::Concrete(ret_ty.clone());
            return Some(QualifiedSymbol::Function {
                param_types,
                param_refinements: refinements.clone(),
                return_type,
                return_refinement: ret_ref.clone(),
                generic_params: generic_params.clone(),
            });
        }
        if let Some(fields) = module.structs.get(item_name) {
            let struct_fields = fields
                .iter()
                .map(|f| (f.name.clone(), Type::Concrete(f.ty.clone())))
                .collect();
            return Some(QualifiedSymbol::Struct(struct_fields));
        }
        if let Some(variants) = module.enums.get(item_name) {
            return Some(QualifiedSymbol::Enum(variants.clone()));
        }
        None
    }

    pub fn is_copy_type(ty: &str) -> bool {
        matches!(
            ty,
            "i8" | "i16"
                | "i32"
                | "i64"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "f32"
                | "f64"
                | "char"
                | "i8*"
                | "i16*"
                | "i32*"
                | "i64*"
                | "u8*"
                | "u16*"
                | "u32*"
                | "u64*"
                | "f32*"
                | "f64*"
                | "char*"
                | "&str"
        ) || ty.starts_with("&")
            || ty.starts_with("&mut ")
    }

    pub fn is_dynamic_array(ty: &str) -> bool {
        ty.starts_with("[]")
    }

    pub fn dynamic_array_elem_type(ty: &str) -> Option<String> {
        if ty.starts_with("[]") {
            Some(ty[2..].to_string())
        } else {
            None
        }
    }

    pub fn set_resolved_type(&mut self, name: &str, ty: String) {
        self.resolved_types.insert(name.to_string(), ty);
    }

    pub fn get_resolved_type(&self, name: &str) -> Option<String> {
        self.resolved_types.get(name).cloned()
    }

    pub fn register_type_alias(&mut self, name: &str, target_type: &str, span: Span) -> bool {
        if self.type_aliases.contains_key(name) {
            emit_diagnostic(
                &Diagnostic::error(&format!("Type alias '{}' already defined", name))
                    .with_code("VX0305")
                    .with_span(span),
            );
            return false;
        }
        self.type_aliases
            .insert(name.to_string(), target_type.to_string());
        true
    }

    pub fn lookup_type_alias(&self, name: &str) -> Option<&String> {
        self.type_aliases.get(name)
    }
}
