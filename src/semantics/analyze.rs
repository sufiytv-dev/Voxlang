// semantics/analyze.rs
//! Core semantic analysis: statement and expression analysis with type inference,
//! borrowing, path conditions, and refinement checking.

use crate::comptime::ComptimeEvaluator;
use crate::diagnostic::{Diagnostic, Suggestion, emit_diagnostic};
use crate::frontend::span::Span;
use crate::frontend::token::TokenKind;
use crate::parser::{ASTNode, KernelAttr, MatchArm, MatchPattern};
use crate::refinement;
use crate::semantics::SemanticAnalyzer;
use crate::semantics::symbol::SymbolTable;
use crate::semantics::types::Type;
use std::collections::HashSet;

// -----------------------------------------------------------------------------
// Helper to extract the span from any AST node
// -----------------------------------------------------------------------------
pub(crate) fn node_span(node: &ASTNode) -> Span {
    match node {
        ASTNode::Program(_, span) => *span,
        ASTNode::Import { span, .. } => *span,
        ASTNode::StructDef { span, .. } => *span,
        ASTNode::Block { span, .. } => *span,
        ASTNode::EnumDef { span, .. } => *span,
        ASTNode::TypeAlias { span, .. } => *span,
        ASTNode::UseDecl { span, .. } => *span,
        ASTNode::FunctionDef { span, .. } => *span,
        ASTNode::KernelFn { span, .. } => *span,
        ASTNode::KernelLaunch { span, .. } => *span,
        ASTNode::IfStatement { span, .. } => *span,
        ASTNode::IfLetStatement { span, .. } => *span,
        ASTNode::WhileLetStatement { span, .. } => *span,
        ASTNode::TryExpr { span, .. } => *span,
        ASTNode::WhileStatement { span, .. } => *span,
        ASTNode::ForLoop { span, .. } => *span,
        ASTNode::ParallelLoop { span, .. } => *span,
        ASTNode::ComptimeBlock { span, .. } => *span,
        ASTNode::ReturnStatement(_, span) => *span,
        ASTNode::VariableDecl { span, .. } => *span,
        ASTNode::DeviceVarDecl { span, .. } => *span,
        ASTNode::Assignment { span, .. } => *span,
        ASTNode::BinaryExpr { span, .. } => *span,
        ASTNode::UnaryExpr { span, .. } => *span,
        ASTNode::CastExpr { span, .. } => *span,
        ASTNode::CallExpr { span, .. } => *span,
        ASTNode::StructLiteral { span, .. } => *span,
        ASTNode::BorrowExpr { span, .. } => *span,
        ASTNode::DerefExpr(_, span) => *span,
        ASTNode::FieldAccess { span, .. } => *span,
        ASTNode::ArrayLiteral { span, .. } => *span,
        ASTNode::ArrayIndex { span, .. } => *span,
        ASTNode::SliceExpr { span, .. } => *span,
        ASTNode::MatchExpr { span, .. } => *span,
        ASTNode::Identifier(_, s) => *s,
        ASTNode::IntegerLiteral(_, s) => *s,
        ASTNode::FloatLiteral(_, s) => *s,
        ASTNode::CharLiteral(_, s) => *s,
        ASTNode::StringLiteral(_, s) => *s,
        ASTNode::RefinedType { span, .. } => *span,
        ASTNode::Lemma { span, .. } => *span,
        ASTNode::Error => Span::new(0, 0, 0, 0),
    }
}

// -----------------------------------------------------------------------------
// Helper to get span from a MatchPattern (used in match expression analysis)
// -----------------------------------------------------------------------------
impl MatchPattern {
    fn span(&self) -> Span {
        match self {
            MatchPattern::UnitVariant { span, .. } => *span,
            MatchPattern::Wildcard(span) => *span,
            MatchPattern::Binding { span, .. } => *span,
        }
    }
}

// -----------------------------------------------------------------------------
// SemanticAnalyzer method implementations
// -----------------------------------------------------------------------------
impl SemanticAnalyzer<'_> {
    // -------------------------------------------------------------------------
    // Return refinement helpers
    // -------------------------------------------------------------------------
    pub(crate) fn current_path_condition(&self) -> Option<ASTNode> {
        let mut all = Vec::new();
        for conds in &self.path_conditions {
            all.extend(conds.iter().cloned());
        }
        if all.is_empty() {
            return None;
        }
        let mut combined = all[0].clone();
        for cond in &all[1..] {
            combined = ASTNode::BinaryExpr {
                left: Box::new(combined),
                op: TokenKind::And,
                right: Box::new(cond.clone()),
                span: node_span(cond),
            };
        }
        Some(combined)
    }

    pub(crate) fn push_path_condition(&mut self, cond: ASTNode) {
        if let Some(top) = self.path_conditions.last_mut() {
            top.push(cond);
        } else {
            self.path_conditions.push(vec![cond]);
        }
    }

    pub(crate) fn enter_path_level(&mut self) {
        self.path_conditions.push(Vec::new());
    }

    pub(crate) fn exit_path_level(&mut self) {
        self.path_conditions.pop();
    }

    // -------------------------------------------------------------------------
    // Refinement checking helpers
    // -------------------------------------------------------------------------
    pub(crate) fn check_refinement(
        &mut self,
        refinement: &Option<Box<ASTNode>>,
        context_name: &str,
        span: Span,
    ) -> bool {
        if let Some(cond) = refinement {
            let cond_type = match self.analyze_expression(cond, None) {
                Some(t) => t,
                None => return false,
            };
            if cond_type != Type::Concrete("i32".to_string()) {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Refinement condition for '{}' must evaluate to i32 (boolean), got {}.",
                        context_name,
                        cond_type.to_string()
                    ))
                    .with_code("VX0245")
                    .with_span(span),
                );
                self.error_occurred = true;
                return false;
            }
        }
        true
    }

    pub(crate) fn extract_condition(refinement: &Option<Box<ASTNode>>) -> Option<&ASTNode> {
        refinement.as_ref().and_then(|r| {
            if let ASTNode::RefinedType { condition, .. } = &**r {
                Some(condition.as_ref())
            } else {
                Some(r.as_ref())
            }
        })
    }

    // -------------------------------------------------------------------------
    // Determine if a sequence of statements contains a return
    // -------------------------------------------------------------------------
    pub(crate) fn has_return_in_stmts(&self, stmts: &[ASTNode]) -> bool {
        for stmt in stmts {
            match stmt {
                ASTNode::ReturnStatement(Some(_), _) => return true,
                ASTNode::Program(nested, _) => {
                    if self.has_return_in_stmts(nested) {
                        return true;
                    }
                }
                ASTNode::FunctionDef { body, .. } => {
                    if self.has_return_in_stmts(body) {
                        return true;
                    }
                }
                ASTNode::KernelFn { body, .. } => {
                    if self.has_return_in_stmts(body) {
                        return true;
                    }
                }
                ASTNode::IfStatement {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    if self.has_return_in_stmts(then_branch) {
                        return true;
                    }
                    if let Some(b) = else_branch {
                        if self.has_return_in_stmts(b) {
                            return true;
                        }
                    }
                }
                ASTNode::WhileStatement { body, .. } => {
                    if self.has_return_in_stmts(body) {
                        return true;
                    }
                }
                ASTNode::ForLoop { body, .. } => {
                    if self.has_return_in_stmts(body) {
                        return true;
                    }
                }
                ASTNode::ParallelLoop { body, .. } => {
                    if self.has_return_in_stmts(body) {
                        return true;
                    }
                }
                ASTNode::ComptimeBlock { body, .. } => {
                    if self.has_return_in_stmts(body) {
                        return true;
                    }
                }
                ASTNode::Block { statements, .. } => {
                    if self.has_return_in_stmts(statements) {
                        return true;
                    }
                }
                // Desugared if-let becomes MatchExpr – check its arms
                ASTNode::MatchExpr { arms, .. } => {
                    for arm in arms {
                        if self.has_return_in_stmts(&arm.body) {
                            return true;
                        }
                    }
                }
                // TryExpr desugars to a match that always returns on error
                ASTNode::TryExpr { .. } => {
                    return true;
                }
                ASTNode::KernelLaunch { .. } => {
                    // Kernel launch is an expression, not a return
                }
                _ => {}
            }
        }
        false
    }

    // -------------------------------------------------------------------------
    // Resolve the type and mutability of an lvalue (assignment left‑hand side)
    // -------------------------------------------------------------------------
    pub(crate) fn resolve_lvalue_type(&mut self, node: &ASTNode) -> Option<(Type, bool)> {
        match node {
            ASTNode::Identifier(name, span) => match self.symbols.lookup_info(name) {
                Some((ty, true, mutable)) => {
                    let resolved_ty = self.unify.resolve(ty);
                    self.dbg(&format!(
                        "resolve_lvalue_type: resolved '{}' to {:?}",
                        name, resolved_ty
                    ));
                    // Treat mutable reference as mutable for assignment
                    let is_mutable = mutable || matches!(resolved_ty, Type::Reference(true, _));
                    let stripped_ty = match resolved_ty.strip_references() {
                        Type::Reference(_, inner) => inner.as_ref().clone(),
                        _ => resolved_ty.clone(),
                    };
                    Some((stripped_ty, is_mutable))
                }
                Some((_, false, _)) => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Cannot assign to '{}' (value is moved or mutably borrowed).",
                            name
                        ))
                        .with_code("VX0213")
                        .with_span(*span),
                    );
                    self.error_occurred = true;
                    None
                }
                None => {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Undeclared identifier '{}'", name))
                            .with_code("VX0214")
                            .with_span(*span),
                    );
                    self.error_occurred = true;
                    None
                }
            },
            ASTNode::DerefExpr(inner, span) => {
                let inner_ty = match self.analyze_expression(inner, None) {
                    Some(ty) => ty,
                    None => return None,
                };
                let resolved_inner_ty = self.unify.resolve(&inner_ty);
                match resolved_inner_ty {
                    Type::Reference(mut_, inner) => Some((inner.as_ref().clone(), mut_)),
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot dereference non‑reference type '{}'",
                                inner_ty.to_string()
                            ))
                            .with_code("VX0215")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        None
                    }
                }
            }
            ASTNode::FieldAccess { expr, field, span } => {
                let (base_ty, base_mutable) = match self.resolve_lvalue_type(expr) {
                    Some(pair) => pair,
                    None => return None,
                };
                let base_ty_stripped = base_ty.strip_references();
                let field_ty = match base_ty_stripped {
                    Type::Struct(_, _) | Type::Concrete(_) => {
                        // Resolve concrete struct fields
                        let base_resolved = self.resolve_type(base_ty_stripped.clone(), *span);
                        if let Some(fields) =
                            self.symbols.resolve_concrete_struct(&base_resolved, *span)
                        {
                            fields
                                .iter()
                                .find(|(fname, _)| fname == field)
                                .map(|(_, ty)| ty.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                match field_ty {
                    Some(ty) => Some((ty, base_mutable)),
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Struct '{}' has no field named '{}'",
                                base_ty_stripped.to_string(),
                                field
                            ))
                            .with_code("VX0261")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        None
                    }
                }
            }
            ASTNode::ArrayIndex { array, index, span } => {
                let (array_ty, base_mutable) = match self.resolve_lvalue_type(array) {
                    Some((ty, mutable)) => (ty, mutable),
                    None => return None,
                };
                let index_ty = match self.analyze_expression(index, None) {
                    Some(ty) => ty,
                    None => return None,
                };
                if index_ty != Type::Concrete("i32".to_string()) {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Array index must be i32, got {}",
                            index_ty.to_string()
                        ))
                        .with_code("VX0262")
                        .with_span(node_span(index)),
                    );
                    self.error_occurred = true;
                    return None;
                }
                if let Some(elem) = Self::extract_array_element_type(&array_ty) {
                    return Some((elem, base_mutable));
                }
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Cannot index non‑array type '{}'",
                        array_ty.to_string()
                    ))
                    .with_code("VX0263")
                    .with_span(*span),
                );
                self.error_occurred = true;
                None
            }
            ASTNode::SliceExpr { span, .. } => {
                emit_diagnostic(
                    &Diagnostic::error("Cannot assign to a string slice (slice is immutable).")
                        .with_code("VX0277")
                        .with_span(*span),
                );
                self.error_occurred = true;
                None
            }
            _ => {
                let span = node_span(node);
                emit_diagnostic(
                    &Diagnostic::error("Invalid left‑hand side expression")
                        .with_code("VX0216")
                        .with_span(span),
                );
                self.error_occurred = true;
                None
            }
        }
    }

    // -------------------------------------------------------------------------
    // Constraint solving and type collection
    // -------------------------------------------------------------------------
    pub(crate) fn solve_constraints(&mut self, span: Span) -> bool {
        if self.unify.report_unbound(span) {
            return false;
        }
        true
    }

    pub(crate) fn collect_resolved_types(&mut self) {
        let mut resolved = Vec::new();
        // Determine the current function name (or "global" for module scope)
        let func_name = self.current_function_name.as_deref().unwrap_or("global");
        for scope in &self.symbols.scopes {
            for (name, var_info) in scope {
                let resolved_ty = self.unify.resolve(&var_info.ty);
                if let Some(concrete) = self.unify.as_concrete_string(&resolved_ty) {
                    // Use a qualified key to avoid collisions across functions
                    let key = format!("{}::{}", func_name, name);
                    resolved.push((key, concrete));
                } else {
                    self.dbg(&format!(
                        "Variable '{}' has unresolved type {:?}",
                        name, resolved_ty
                    ));
                }
            }
        }
        for (key, concrete) in resolved {
            self.resolved_variable_types
                .insert(key.clone(), concrete.clone());
            self.symbols.set_resolved_type(&key, concrete);
        }
    }

    // -------------------------------------------------------------------------
    // Kernel block dimension validation
    // -------------------------------------------------------------------------
    fn validate_kernel_block(&mut self, attr: &KernelAttr, span: Span) -> bool {
        let (bx, by, bz) = attr.block;
        let max_total = 1024;
        let max_dim = 1024;
        if bx == 0 || by == 0 || bz == 0 {
            emit_diagnostic(
                &Diagnostic::error("Kernel block dimensions must be positive integers")
                    .with_code("VX0293")
                    .with_span(span),
            );
            return false;
        }
        if bx > max_dim || by > max_dim || bz > max_dim {
            emit_diagnostic(
                &Diagnostic::error(&format!(
                    "Kernel block dimension exceeds limit (max {})",
                    max_dim
                ))
                .with_code("VX0294")
                .with_span(span),
            );
            return false;
        }
        if bx * by * bz > max_total {
            emit_diagnostic(
                &Diagnostic::error(&format!(
                    "Kernel block has too many threads ({} > {})",
                    bx * by * bz,
                    max_total
                ))
                .with_code("VX0295")
                .with_span(span),
            );
            return false;
        }
        true
    }

    // -------------------------------------------------------------------------
    // Statement analysis (top‑level entry point for checking)
    // -------------------------------------------------------------------------
    pub(crate) fn analyze_statement(&mut self, node: &ASTNode) {
        self.dbg(&format!(
            "analyze_statement: {:?} at {:?}",
            node,
            node_span(node)
        ));
        match node {
            ASTNode::Program(statements, _) => {
                for stmt in statements {
                    self.analyze_statement(stmt);
                }
            }
            ASTNode::Error => {
                self.error_occurred = true;
            }
            ASTNode::StructDef {
                name,
                generic_params,
                fields,
                span: _,
            } => {
                self.symbols
                    .register_struct(name, generic_params.clone(), fields.clone());
            }
            ASTNode::EnumDef {
                name,
                params,
                variants,
                span: _,
            } => {
                let payload_types = vec![None; variants.len()];
                self.symbols
                    .register_enum(name, params.clone(), variants.clone(), payload_types);
                if !params.is_empty() {
                    emit_diagnostic(
                        &Diagnostic::warning(&format!(
                            "Generic enum '{}' defined but payload fields not yet supported; all variants treated as unit.",
                            name
                        ))
                        .with_code("VX9995")
                        .with_span(node_span(node)),
                    );
                }
            }
            ASTNode::TypeAlias {
                name,
                target_type,
                span,
            } => {
                self.symbols.register_type_alias(name, target_type, *span);
            }
            ASTNode::UseDecl { span, .. } => {
                if !self.resolve_use_decl(node, *span) {
                    self.error_occurred = true;
                }
            }
            ASTNode::Import { span, .. } => {
                if !self.process_import(node, *span) {
                    self.error_occurred = true;
                }
            }
            ASTNode::MatchExpr { .. } => {
                if self.analyze_expression(node, None).is_none() {
                    self.error_occurred = true;
                }
            }
            ASTNode::Lemma {
                name,
                params,
                return_type,
                proof,
                span,
            } => {
                if return_type != "bool" && return_type != "i32" {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Lemma '{}' must return bool or i32, got {}.",
                            name, return_type
                        ))
                        .with_code("VX0246")
                        .with_span(*span),
                    );
                    self.error_occurred = true;
                    return;
                }
                let empty_set = HashSet::new();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.parse_type_str_with_imports(&p.ty, &empty_set))
                    .collect();
                let param_refinements: Vec<Option<Box<ASTNode>>> =
                    params.iter().map(|p| p.refinement.clone()).collect();
                let return_ty = self.parse_type_str_with_imports(return_type, &empty_set);
                self.symbols.register_function(
                    name,
                    param_types,
                    param_refinements,
                    return_ty,
                    None,
                    vec![],
                    false,
                );
                self.symbols.enter_scope();
                self.borrowed_in_scope.push(Vec::new());
                for param in params {
                    if !self
                        .symbols
                        .insert(&param.name, &param.ty, false, false, param.span)
                    {
                        self.error_occurred = true;
                    }
                    self.check_refinement(&param.refinement, &param.name, param.span);
                }
                for stmt in proof {
                    self.analyze_statement(stmt);
                }
                for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                    self.symbols.release_borrow(&name, span);
                }
                self.symbols.exit_scope();
            }
            ASTNode::KernelFn {
                name,
                params,
                body,
                device_triple: _,
                attr,
                span: _,
            } => {
                // Validate block dimensions
                if !self.validate_kernel_block(attr, node_span(node)) {
                    self.error_occurred = true;
                    return;
                }

                // =============================================================
                // Register kernel and its launch stub using register_function_def
                // =============================================================
                let generic_params = Vec::<String>::new(); // kernels have no generics
                let return_type_str = "void".to_string();
                let return_refinement = None;
                let span = node_span(node);

                self.symbols.register_function_def(
                    name,
                    generic_params,
                    params.clone(),
                    return_type_str,
                    return_refinement,
                    body.clone(),
                    span,
                    true, // is_kernel = true
                );

                // Now enter scope and analyze the kernel body (same as before)
                self.symbols.enter_scope();
                self.borrowed_in_scope.push(Vec::new());
                let old_in_kernel = self.in_kernel;
                let old_return_type = self.current_return_type.take();
                let old_return_refinement = self.current_return_refinement.take();
                let old_function_name = self.current_function_name.take();
                self.current_return_type = Some(Type::Concrete("void".to_string()));
                self.current_return_refinement = None;
                self.current_function_name = Some(name.clone());
                self.in_kernel = true;

                let empty_set = HashSet::new();
                for param in params {
                    let param_type = self.parse_type_str_with_imports(&param.ty, &empty_set);
                    if !self
                        .symbols
                        .insert_type(&param.name, param_type, true, false, param.span)
                    {
                        self.error_occurred = true;
                    }
                    self.check_refinement(&param.refinement, &param.name, param.span);
                }
                for stmt in body {
                    self.analyze_statement(stmt);
                }
                for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                    self.symbols.release_borrow(&name, span);
                }
                self.in_kernel = old_in_kernel;
                self.current_return_type = old_return_type;
                self.current_return_refinement = old_return_refinement;
                self.current_function_name = old_function_name;
                self.symbols.exit_scope();
            }
            ASTNode::DeviceVarDecl {
                name,
                ty,
                refinement,
                value,
                span,
            } => {
                let rhs_type = match self.analyze_expression(value, None) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                let final_type = match ty {
                    Some(explicit_ty_str) => {
                        let gp_set = self
                            .current_generic_params
                            .as_ref()
                            .map(|v| v.iter().cloned().collect())
                            .unwrap_or(HashSet::new());
                        let explicit_ty =
                            self.parse_type_str_with_imports(explicit_ty_str, &gp_set);
                        let explicit_ty = self.resolve_type(explicit_ty, *span);
                        if !self.unify.unify(&explicit_ty, &rhs_type, *span) {
                            self.error_occurred = true;
                            return;
                        }
                        explicit_ty
                    }
                    None => rhs_type.clone(),
                };
                if !self
                    .symbols
                    .insert_type(name, final_type, true, false, *span)
                {
                    self.error_occurred = true;
                }
                self.check_refinement(refinement, name, *span);
                if let Some(refinement) = refinement {
                    if let ASTNode::IntegerLiteral(val, _) = &**value {
                        if let Some(cond) = Self::extract_condition(&Some(refinement.clone())) {
                            let func_name =
                                self.current_function_name.as_deref().unwrap_or("global");
                            if !refinement::verify_refinement_with_ctx(
                                cond,
                                name,
                                *val,
                                node_span(value),
                                func_name,
                                true,
                            ) {
                                self.error_occurred = true;
                                return;
                            }
                        }
                    } else {
                        emit_diagnostic(
                            &Diagnostic::warning(&format!(
                                "Refinement on device variable '{}' with non-constant initializer not verified.",
                                name
                            ))
                            .with_code("VX0302")
                            .with_span(*span),
                        );
                    }
                }
            }
            ASTNode::ComptimeBlock { body, span: _ } => {
                for stmt in body {
                    if let Some(evaluated) = ComptimeEvaluator::evaluate(stmt) {
                        self.analyze_statement(&evaluated);
                    } else {
                        self.analyze_statement(stmt);
                    }
                }
            }
            ASTNode::VariableDecl {
                name,
                ty,
                refinement,
                value,
                mutable,
                span,
            } => {
                self.dbg(&format!(
                    "VariableDecl '{}' optional type '{:?}' mutable={}",
                    name, ty, mutable
                ));
                let expected_type = ty.as_deref();
                let gp_set = self
                    .current_generic_params
                    .as_ref()
                    .map(|v| v.iter().cloned().collect())
                    .unwrap_or(HashSet::new());
                let var_type = if let Some(explicit_ty_str) = expected_type {
                    let ty = self.parse_type_str_with_imports(explicit_ty_str, &gp_set);
                    self.resolve_type(ty, *span)
                } else {
                    self.fresh_infer_var()
                };
                if !self
                    .symbols
                    .insert_type(name, var_type.clone(), false, *mutable, *span)
                {
                    self.error_occurred = true;
                    return;
                }
                let rhs_type = match self.analyze_expression(value, Some(&var_type)) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                if !self.unify.unify(&var_type, &rhs_type, *span) {
                    self.error_occurred = true;
                    return;
                }
                self.check_refinement(refinement, name, *span);
                if let Some(refinement) = refinement {
                    if let ASTNode::IntegerLiteral(val, _) = &**value {
                        if let Some(cond) = Self::extract_condition(&Some(refinement.clone())) {
                            let func_name =
                                self.current_function_name.as_deref().unwrap_or("global");
                            if !refinement::verify_refinement_with_ctx(
                                cond,
                                name,
                                *val,
                                node_span(value),
                                func_name,
                                true,
                            ) {
                                self.error_occurred = true;
                                return;
                            }
                        }
                    } else {
                        emit_diagnostic(
                            &Diagnostic::warning(&format!(
                                "Refinement on variable '{}' with non‑constant initializer not verified; assuming true.",
                                name
                            ))
                            .with_code("VX0302")
                            .with_span(*span),
                        );
                    }
                }
                if let ASTNode::Identifier(source_name, _) = &**value {
                    if let Some((src_ty, alive, _)) = self.symbols.lookup_state(source_name) {
                        if !alive {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Use of moved value '{}'",
                                    source_name
                                ))
                                .with_code("VX0219")
                                .with_span(node_span(value)),
                            );
                            self.error_occurred = true;
                            return;
                        }
                        if !SymbolTable::is_copy_type(&src_ty) {
                            if !self.symbols.mark_moved(source_name, node_span(value)) {
                                self.error_occurred = true;
                            }
                        }
                    }
                }
            }
            ASTNode::Assignment { lhs, value, span } => {
                let value_type = match self.analyze_expression(value, None) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                let (lhs_type, is_mutable) = match self.resolve_lvalue_type(lhs) {
                    Some(pair) => pair,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                if !self.unify.unify(&lhs_type, &value_type, *span) {
                    self.error_occurred = true;
                    return;
                }
                if !is_mutable {
                    emit_diagnostic(
                        &Diagnostic::error("Cannot assign to immutable location.")
                            .with_code("VX0221")
                            .with_span(*span),
                    );
                    self.error_occurred = true;
                    return;
                }
                if let ASTNode::Identifier(source_name, _) = &**value {
                    if let Some((src_ty, alive, _)) = self.symbols.lookup_state(source_name) {
                        if !alive {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Use of moved value '{}'",
                                    source_name
                                ))
                                .with_code("VX0222")
                                .with_span(node_span(value)),
                            );
                            self.error_occurred = true;
                            return;
                        }
                        if !SymbolTable::is_copy_type(&src_ty) {
                            if !self.symbols.mark_moved(source_name, node_span(value)) {
                                self.error_occurred = true;
                            }
                        }
                    }
                }
            }
            ASTNode::IfStatement {
                condition,
                then_branch,
                else_branch,
                span: _,
            } => {
                let cond_type = match self.analyze_expression(condition, None) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                if cond_type != Type::Concrete("i32".to_string()) {
                    let span = node_span(condition);
                    emit_diagnostic(
                        &Diagnostic::error("If condition must be of type i32.")
                            .with_code("VX0223")
                            .with_span(span)
                            .with_suggestion(Suggestion {
                                message: "Use a comparison operator or logical operator (and/or) to produce i32."
                                    .to_string(),
                                span: Some(span),
                            }),
                    );
                    self.error_occurred = true;
                    return;
                }
                self.symbols.enter_scope();
                self.borrowed_in_scope.push(Vec::new());
                for stmt in then_branch {
                    self.analyze_statement(stmt);
                }
                for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                    self.symbols.release_borrow(&name, span);
                }
                self.symbols.exit_scope();
                if let Some(else_branch) = else_branch {
                    self.symbols.enter_scope();
                    self.borrowed_in_scope.push(Vec::new());
                    for stmt in else_branch {
                        self.analyze_statement(stmt);
                    }
                    for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                        self.symbols.release_borrow(&name, span);
                    }
                    self.symbols.exit_scope();
                }
            }
            ASTNode::WhileStatement {
                condition,
                body,
                span: _,
            } => {
                let cond_type = match self.analyze_expression(condition, None) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                if cond_type != Type::Concrete("i32".to_string()) {
                    let span = node_span(condition);
                    emit_diagnostic(
                        &Diagnostic::error("While condition must be of type i32.")
                            .with_code("VX0224")
                            .with_span(span)
                            .with_suggestion(Suggestion {
                                message: "Use a comparison operator or logical operator (and/or) to produce i32."
                                    .to_string(),
                                span: Some(span),
                            }),
                    );
                    self.error_occurred = true;
                    return;
                }
                self.symbols.enter_scope();
                self.borrowed_in_scope.push(Vec::new());
                for stmt in body {
                    self.analyze_statement(stmt);
                }
                for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                    self.symbols.release_borrow(&name, span);
                }
                self.symbols.exit_scope();
            }
            ASTNode::ForLoop {
                iter_var: _,
                start,
                end,
                body,
                span: _,
            } => {
                let start_type = match self.analyze_expression(start, None) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                let end_type = match self.analyze_expression(end, None) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                if start_type != Type::Concrete("i32".to_string()) {
                    let span = node_span(start);
                    emit_diagnostic(
                        &Diagnostic::error("`for` loop start must be i32.")
                            .with_code("VX0303")
                            .with_span(span),
                    );
                    self.error_occurred = true;
                    return;
                }
                if end_type != Type::Concrete("i32".to_string()) {
                    let span = node_span(end);
                    emit_diagnostic(
                        &Diagnostic::error("`for` loop end must be i32.")
                            .with_code("VX0304")
                            .with_span(span),
                    );
                    self.error_occurred = true;
                    return;
                }

                self.symbols.enter_scope();
                self.borrowed_in_scope.push(Vec::new());

                // The loop variable is introduced in the body's own scope,
                // but the parser wraps the body in a Block, so we just analyze that Block.
                for stmt in body {
                    self.analyze_statement(stmt);
                }

                for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                    self.symbols.release_borrow(&name, span);
                }
                self.symbols.exit_scope();
            }
            ASTNode::ParallelLoop {
                iter_var,
                start,
                end,
                body,
                span: _,
            } => {
                let start_type = match self.analyze_expression(start, None) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                let end_type = match self.analyze_expression(end, None) {
                    Some(t) => t,
                    None => {
                        self.error_occurred = true;
                        return;
                    }
                };
                if start_type != Type::Concrete("i32".to_string()) {
                    let span = node_span(start);
                    emit_diagnostic(
                        &Diagnostic::error("Parallel loop start must be i32.")
                            .with_code("VX0225")
                            .with_span(span),
                    );
                    self.error_occurred = true;
                    return;
                }
                if end_type != Type::Concrete("i32".to_string()) {
                    let span = node_span(end);
                    emit_diagnostic(
                        &Diagnostic::error("Parallel loop end must be i32.")
                            .with_code("VX0226")
                            .with_span(span),
                    );
                    self.error_occurred = true;
                    return;
                }
                self.symbols.enter_scope();
                self.borrowed_in_scope.push(Vec::new());
                if !self
                    .symbols
                    .insert(iter_var, "i32", false, false, node_span(node))
                {
                    self.error_occurred = true;
                }
                for stmt in body {
                    self.analyze_statement(stmt);
                }
                for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                    self.symbols.release_borrow(&name, span);
                }
                self.symbols.exit_scope();
            }
            ASTNode::FunctionDef {
                name,
                generic_params,
                params,
                return_type,
                return_refinement,
                body,
                span,
            } => {
                self.dbg(&format!("FunctionDef '{}'", name));
                self.symbols.register_function_def(
                    name,
                    generic_params.clone(),
                    params.clone(),
                    return_type.clone(),
                    return_refinement.clone(),
                    body.clone(),
                    *span,
                    false,
                );
                self.symbols.enter_scope();
                self.borrowed_in_scope.push(Vec::new());
                self.dbg(&format!("  entered function body scope for '{}'", name));

                let old_return_type = self.current_return_type.take();
                let old_return_refinement = self.current_return_refinement.take();
                let old_function_name = self.current_function_name.take();
                let gp_set: HashSet<_> = generic_params.iter().cloned().collect();
                let return_ty = self.parse_type_str_with_imports(return_type, &gp_set);
                let return_ty = self.resolve_type(return_ty, *span);
                self.current_return_type = Some(return_ty);
                self.current_return_refinement = return_refinement.clone();
                self.current_function_name = Some(name.clone());

                self.current_generic_params = Some(generic_params.clone());

                let old_param_refinements = std::mem::take(&mut self.current_param_refinements);
                for param in params {
                    let param_type = self.parse_type_str_with_imports(&param.ty, &gp_set);
                    let param_type = self.resolve_type(param_type, param.span);
                    if !self
                        .symbols
                        .insert_type(&param.name, param_type, false, false, param.span)
                    {
                        self.error_occurred = true;
                    }
                    self.check_refinement(&param.refinement, &param.name, param.span);
                    self.current_param_refinements
                        .push((param.name.clone(), param.refinement.clone()));
                }

                for stmt in body {
                    self.analyze_statement(stmt);
                }

                if return_type != "void" && !self.has_return_in_stmts(body) {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Function '{}' declares return type '{}' but has no `return` statement.", name, return_type))
                            .with_code("VX0227")
                            .with_span(*span),
                    );
                    self.error_occurred = true;
                }

                if !self.solve_constraints(*span) {
                    self.error_occurred = true;
                }

                self.collect_resolved_types();

                self.current_param_refinements = old_param_refinements;

                for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                    self.symbols.release_borrow(&name, span);
                }
                self.current_return_type = old_return_type;
                self.current_return_refinement = old_return_refinement;
                self.current_function_name = old_function_name;
                self.current_generic_params = None;
                self.symbols.exit_scope();
                self.dbg(&format!("  exited function body scope for '{}'", name));
            }
            ASTNode::ReturnStatement(expr, span) => {
                let expected = match &self.current_return_type {
                    Some(ty) => ty.clone(),
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error("Return statement outside of function body.")
                                .with_code("VX0228")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return;
                    }
                };
                match expr {
                    Some(val) => {
                        let val_type = match self.analyze_expression(val, Some(&expected)) {
                            Some(t) => t,
                            None => {
                                self.error_occurred = true;
                                return;
                            }
                        };
                        if !self.unify.unify(&expected, &val_type, *span) {
                            self.error_occurred = true;
                            return;
                        }
                        if let Some(refinement) = &self.current_return_refinement {
                            let refinement_some = Some(refinement.clone());
                            let cond_opt = Self::extract_condition(&refinement_some);
                            if let Some(cond) = cond_opt {
                                let path_cond = self.current_path_condition();
                                let param_refs = self.current_param_refinements.clone();
                                let func_name =
                                    self.current_function_name.as_deref().unwrap_or("unknown");
                                if !refinement::verify_return_refinement(
                                    cond,
                                    val,
                                    &param_refs,
                                    path_cond.as_ref(),
                                    func_name,
                                    node_span(val),
                                ) {
                                    self.error_occurred = true;
                                    return;
                                }
                            } else {
                                emit_diagnostic(
                                    &Diagnostic::warning(
                                        "Return refinement present but could not extract condition.",
                                    )
                                    .with_code("VX0458")
                                    .with_span(*span),
                                );
                            }
                        }
                    }
                    None => {
                        if expected != Type::Concrete("void".to_string()) {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Function expects return type '{}', but return has no value.",
                                    expected.to_string()
                                ))
                                .with_code("VX0231")
                                .with_span(*span),
                            );
                            self.error_occurred = true;
                            return;
                        }
                    }
                }
            }
            ASTNode::RefinedType { condition, .. } => {
                self.analyze_expression(condition, None);
            }
            // Handle Block nodes – they create a new scope.
            ASTNode::Block {
                statements,
                span: _,
            } => {
                self.symbols.enter_scope();
                self.borrowed_in_scope.push(Vec::new());
                for stmt in statements {
                    self.analyze_statement(stmt);
                }
                for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                    self.symbols.release_borrow(&name, span);
                }
                self.symbols.exit_scope();
            }
            _ => {
                let _ = self.analyze_expression(node, None);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Expression analysis (returns Type) – with resolution of inference variables
    // -------------------------------------------------------------------------
    pub(crate) fn analyze_expression(
        &mut self,
        node: &ASTNode,
        expected: Option<&Type>,
    ) -> Option<Type> {
        self.dbg(&format!(
            "analyze_expression: {:?}, expected={:?}",
            node,
            expected.map(|t| t.to_string())
        ));
        // Helper to resolve imported identifier names
        let resolve_imported_identifier =
            |name: &str| -> Option<String> { self.use_imports.get(name).cloned() };

        match node {
            ASTNode::IntegerLiteral(_, _) => Some(Type::Concrete("i32".to_string())),
            ASTNode::FloatLiteral(_, _) => Some(Type::Concrete("f64".to_string())),
            ASTNode::CharLiteral(_, _) => Some(Type::Concrete("char".to_string())),
            ASTNode::StringLiteral(_, _) => Some(Type::Concrete("&str".to_string())),
            ASTNode::Error => {
                self.error_occurred = true;
                None
            }
            ASTNode::ArrayLiteral { elements, span } => {
                if elements.is_empty() {
                    emit_diagnostic(
                        &Diagnostic::error("Empty array literals are not supported (infer element type impossible).")
                            .with_code("VX0270")
                            .with_span(*span),
                    );
                    self.error_occurred = true;
                    return None;
                }
                let first_ty = self.analyze_expression(&elements[0], None)?;
                for elem in &elements[1..] {
                    let elem_ty = self.analyze_expression(elem, None)?;
                    if !self.unify.unify(&first_ty, &elem_ty, node_span(elem)) {
                        self.error_occurred = true;
                        return None;
                    }
                }
                // Check expected type for fixed-size array
                let array_ty = if let Some(expected_ty) = expected {
                    match expected_ty {
                        Type::Array(expected_elem, expected_len) => {
                            if !self.unify.unify(&first_ty, expected_elem, *span) {
                                self.error_occurred = true;
                                return None;
                            }
                            Type::Array(Box::new(first_ty), *expected_len)
                        }
                        _ => Type::Array(Box::new(first_ty), None),
                    }
                } else {
                    Type::Array(Box::new(first_ty), None)
                };
                Some(array_ty)
            }
            ASTNode::MatchExpr { value, arms, span } => {
                let mut scr = value.clone();
                let any_by_ref = arms.iter().any(|arm| match &arm.pattern {
                    MatchPattern::UnitVariant { by_ref, .. } => *by_ref,
                    MatchPattern::Binding { by_ref, .. } => *by_ref,
                    _ => false,
                });
                if any_by_ref {
                    scr = Box::new(ASTNode::DerefExpr(scr.clone(), node_span(&scr)));
                }

                let value_ty = match self.analyze_expression(&scr, None) {
                    Some(ty) => ty,
                    None => return None,
                };

                let base_enum = match &value_ty {
                    Type::Enum(name, _) => name.clone(),
                    Type::Concrete(s) => SymbolTable::strip_generic_args(s),
                    _ => "".to_string(),
                };
                let enum_info = self.symbols.lookup_enum(&base_enum).cloned();
                let mut subst = std::collections::HashMap::new();
                if let Some(info) = &enum_info {
                    if let Type::Enum(_, concrete_args) = &value_ty {
                        for (i, gp) in info.generic_params.iter().enumerate() {
                            if i < concrete_args.len() {
                                subst.insert(gp.clone(), concrete_args[i].clone());
                            }
                        }
                    }
                }

                if enum_info.is_none() {
                    emit_diagnostic(
                        &Diagnostic::warning(&format!(
                            "Match expression used on non‑enum type `{}`; assuming type is `{}`.",
                            value_ty.to_string(),
                            value_ty.to_string()
                        ))
                        .with_code("VX0295")
                        .with_span(*span),
                    );
                }

                let mut arm_types = Vec::new();
                for (_idx, arm) in arms.iter().enumerate() {
                    let variant_index = match &arm.pattern {
                        MatchPattern::UnitVariant { variant, .. } => {
                            if let Some(info) = &enum_info {
                                info.variants.iter().position(|v| v.name == *variant)
                            } else {
                                None
                            }
                        }
                        MatchPattern::Binding { variant, .. } => {
                            if let Some(info) = &enum_info {
                                info.variants.iter().position(|v| v.name == *variant)
                            } else {
                                None
                            }
                        }
                        MatchPattern::Wildcard(_) => None,
                    };
                    let cond = if let Some(idx) = variant_index {
                        ASTNode::BinaryExpr {
                            left: scr.clone(),
                            op: TokenKind::Equal,
                            right: Box::new(ASTNode::IntegerLiteral(idx as i64, node_span(&scr))),
                            span: node_span(&scr),
                        }
                    } else {
                        ASTNode::IntegerLiteral(1, node_span(&scr))
                    };
                    self.enter_path_level();
                    if !matches!(cond, ASTNode::IntegerLiteral(1, _)) {
                        self.push_path_condition(cond);
                    }

                    self.symbols.enter_scope();
                    self.borrowed_in_scope.push(Vec::new());

                    if let MatchPattern::Binding { bindings, .. } = &arm.pattern {
                        let raw_payload_ty = if let Some(info) = &enum_info {
                            if let Some(idx) = variant_index {
                                info.variant_payload_type[idx].clone()
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        let concrete_payload_ty = raw_payload_ty.map_or_else(
                            || Type::Concrete("i32".to_string()),
                            |ty| ty.substitute(&subst),
                        );
                        for binding in bindings {
                            if binding == "_" {
                                continue;
                            }
                            self.symbols.insert_type(
                                binding,
                                concrete_payload_ty.clone(),
                                false,
                                true,
                                arm.pattern.span(),
                            );
                        }
                    }

                    for stmt in &arm.body {
                        self.analyze_statement(stmt);
                    }

                    let has_return = self.has_return_in_stmts(&arm.body);
                    let arm_type = if !has_return {
                        if let Some(last) = arm.body.last() {
                            match last {
                                ASTNode::IntegerLiteral(_, _)
                                | ASTNode::FloatLiteral(_, _)
                                | ASTNode::CharLiteral(_, _)
                                | ASTNode::StringLiteral(_, _)
                                | ASTNode::Identifier(_, _)
                                | ASTNode::CallExpr { .. }
                                | ASTNode::BinaryExpr { .. }
                                | ASTNode::UnaryExpr { .. }
                                | ASTNode::CastExpr { .. }
                                | ASTNode::StructLiteral { .. }
                                | ASTNode::ArrayLiteral { .. }
                                | ASTNode::MatchExpr { .. }
                                | ASTNode::BorrowExpr { .. }
                                | ASTNode::DerefExpr(_, _)
                                | ASTNode::FieldAccess { .. }
                                | ASTNode::ArrayIndex { .. }
                                | ASTNode::SliceExpr { .. } => self
                                    .analyze_expression(last, None)
                                    .unwrap_or(Type::Concrete("void".to_string())),
                                _ => Type::Concrete("void".to_string()),
                            }
                        } else {
                            Type::Concrete("void".to_string())
                        }
                    } else {
                        Type::Concrete("void".to_string())
                    };
                    arm_types.push(arm_type);

                    for (name, span) in self.borrowed_in_scope.pop().unwrap() {
                        self.symbols.release_borrow(&name, span);
                    }
                    self.symbols.exit_scope();
                    self.exit_path_level();
                }

                if arm_types.is_empty() {
                    emit_diagnostic(
                        &Diagnostic::error("Match expression must have at least one arm.")
                            .with_code("VX0293")
                            .with_span(*span),
                    );
                    self.error_occurred = true;
                    return None;
                }

                let non_void_types: Vec<&Type> = arm_types
                    .iter()
                    .filter(|t| **t != Type::Concrete("void".to_string()))
                    .collect();
                if non_void_types.is_empty() {
                    return Some(Type::Concrete("void".to_string()));
                }
                let first_type = non_void_types[0];
                for t in &non_void_types[1..] {
                    if !self.unify.unify(first_type, t, *span) {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Match arms have inconsistent types: first non‑void arm returns `{}`, another returns `{}`",
                                first_type.to_string(),
                                t.to_string()
                            ))
                            .with_code("VX0294")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                }
                Some(first_type.clone())
            }
            ASTNode::Identifier(name, span) => {
                // First, check use imports
                if let Some(qualified) = resolve_imported_identifier(name) {
                    let new_node = ASTNode::Identifier(qualified, *span);
                    return self.analyze_expression(&new_node, expected);
                }

                // Primitive types
                if matches!(
                    name.as_str(),
                    "i32"
                        | "i64"
                        | "i8"
                        | "i16"
                        | "u32"
                        | "u64"
                        | "u8"
                        | "u16"
                        | "f32"
                        | "f64"
                        | "char"
                        | "bool"
                        | "void"
                        | "String"
                        | "&str"
                ) {
                    return Some(Type::Concrete(name.clone()));
                }

                // -------------------------------------------------------------------------
                // Qualified identifier handling – split on "::" or a single colon
                // -------------------------------------------------------------------------
                let has_double = name.contains("::");
                let has_single = name.contains(':');
                if has_double || has_single {
                    let separator = if has_double { "::" } else { ":" };
                    let parts: Vec<&str> = name.split(separator).collect();
                    if parts.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Invalid qualified identifier '{}' (expected module::item or enum::variant)",
                                name
                            ))
                            .with_code("VX0314")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let left = parts[0];
                    let right = parts[1];
                    let stripped_left = if let Some(angle) = left.find('<') {
                        &left[..angle]
                    } else {
                        left
                    };

                    // ---- 1. Try enum variant ----
                    if let Some(info) = self.symbols.lookup_enum(stripped_left) {
                        if info.variants.iter().any(|v| v.name == right) {
                            let mut enum_ty = Type::Enum(stripped_left.to_string(), vec![]);
                            if let Some(expected_ty) = expected {
                                if let Type::Enum(_, exp_args) = expected_ty.strip_references() {
                                    enum_ty =
                                        Type::Enum(stripped_left.to_string(), exp_args.clone());
                                }
                            }
                            self.dbg(&format!(
                                "Resolved enum variant '{}' -> type {:?}",
                                name, enum_ty
                            ));
                            return Some(self.resolve_type(enum_ty, *span));
                        }
                    }

                    // ---- 2. Try module qualified lookup ----
                    if self.symbols.lookup_module(left).is_some() {
                        match self.symbols.lookup_qualified(left, right) {
                            Some(crate::semantics::symbol::QualifiedSymbol::Function {
                                return_type,
                                ..
                            }) => Some(return_type),
                            Some(crate::semantics::symbol::QualifiedSymbol::Struct(_)) => {
                                Some(Type::Concrete(left.to_string()))
                            }
                            Some(crate::semantics::symbol::QualifiedSymbol::Enum(_)) => {
                                Some(Type::Concrete(left.to_string()))
                            }
                            None => {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Cannot find item '{}' in module '{}'",
                                        right, left
                                    ))
                                    .with_code("VX0315")
                                    .with_span(*span),
                                );
                                self.error_occurred = true;
                                None
                            }
                        }
                    } else {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot resolve qualified identifier '{}'",
                                name
                            ))
                            .with_code("VX9003")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        None
                    }
                } else {
                    // ---- Plain identifier ----
                    match self.symbols.lookup_info(name) {
                        Some((ty, true, _)) => {
                            let resolved_ty = self.unify.resolve(ty);
                            let resolved_ty = self.resolve_type(resolved_ty, *span);
                            if let Some(expected_ty) = expected {
                                if !self.unify.unify(&resolved_ty, expected_ty, span.clone()) {
                                    self.error_occurred = true;
                                    return None;
                                }
                            }
                            Some(resolved_ty)
                        }
                        Some((_, false, _)) => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Use of moved or mutably borrowed value: '{}'",
                                    name
                                ))
                                .with_code("VX0232")
                                .with_span(*span),
                            );
                            self.error_occurred = true;
                            None
                        }
                        None => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!("Undeclared identifier: '{}'", name))
                                    .with_code("VX0233")
                                    .with_span(*span),
                            );
                            self.error_occurred = true;
                            None
                        }
                    }
                }
            }
            ASTNode::FieldAccess { expr, field, span } => {
                let base_ty = match self.analyze_expression(expr, None) {
                    Some(ty) => ty,
                    None => return None,
                };
                let resolved_base_ty = self.unify.resolve(&base_ty);
                let base_ty_stripped = resolved_base_ty.strip_references();
                self.dbg(&format!(
                    "FieldAccess: expr type = '{:?}', resolved = '{:?}', stripped = '{}', field = '{}'",
                    base_ty, resolved_base_ty, base_ty_stripped.to_string(), field
                ));
                let field_ty = match base_ty_stripped {
                    Type::Struct(_, _) | Type::Concrete(_) => {
                        let base_resolved = self.resolve_type(base_ty_stripped.clone(), *span);
                        if let Some(fields) =
                            self.symbols.resolve_concrete_struct(&base_resolved, *span)
                        {
                            fields
                                .iter()
                                .find(|(fname, _)| fname == field)
                                .map(|(_, ty)| ty.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                match field_ty {
                    Some(ty) => {
                        self.dbg(&format!("Field '{}' resolved to type '{:?}'", field, ty));
                        if let Some(expected_ty) = expected {
                            if !self.unify.unify(&ty, expected_ty, *span) {
                                self.error_occurred = true;
                                return None;
                            }
                        }
                        Some(ty)
                    }
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Struct '{}' has no field named '{}'",
                                base_ty_stripped.to_string(),
                                field
                            ))
                            .with_code("VX0261")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        None
                    }
                }
            }
            ASTNode::ArrayIndex { array, index, span } => {
                let array_ty = match self.analyze_expression(array, None) {
                    Some(ty) => ty,
                    None => return None,
                };
                let resolved_array_ty = self.unify.resolve(&array_ty);
                let index_ty = match self.analyze_expression(index, None) {
                    Some(ty) => ty,
                    None => return None,
                };
                if index_ty != Type::Concrete("i32".to_string()) {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Array index must be i32, got {}",
                            index_ty.to_string()
                        ))
                        .with_code("VX0262")
                        .with_span(node_span(index)),
                    );
                    self.error_occurred = true;
                    return None;
                }

                // Handle indexing into Vec<T>
                if let Type::Struct(name, type_args) = &resolved_array_ty {
                    if name == "Vec" && type_args.len() == 1 {
                        let elem_ty = type_args[0].clone();
                        if let Some(expected_ty) = expected {
                            if !self.unify.unify(&elem_ty, expected_ty, *span) {
                                self.error_occurred = true;
                                return None;
                            }
                        }
                        return Some(elem_ty);
                    }
                }

                if let Some(elem) = Self::extract_array_element_type(&resolved_array_ty) {
                    if let Some(expected_ty) = expected {
                        if !self.unify.unify(&elem, expected_ty, *span) {
                            self.error_occurred = true;
                            return None;
                        }
                    }
                    return Some(elem);
                }
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Cannot index non‑array type '{}'",
                        array_ty.to_string()
                    ))
                    .with_code("VX0263")
                    .with_span(*span),
                );
                self.error_occurred = true;
                None
            }
            ASTNode::SliceExpr {
                base,
                start,
                end,
                span,
            } => {
                let base_ty = self.analyze_expression(base, None)?;
                if let Some(s) = start {
                    let s_ty = self.analyze_expression(s, None)?;
                    if s_ty != Type::Concrete("i32".to_string()) {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Slice start must be i32, got {}",
                                s_ty.to_string()
                            ))
                            .with_code("VX0278")
                            .with_span(node_span(s)),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                }
                if let Some(e) = end {
                    let e_ty = self.analyze_expression(e, None)?;
                    if e_ty != Type::Concrete("i32".to_string()) {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Slice end must be i32, got {}",
                                e_ty.to_string()
                            ))
                            .with_code("VX0279")
                            .with_span(node_span(e)),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                }
                if base_ty == Type::Concrete("String".to_string()) {
                    if let ASTNode::Identifier(name, _) = &**base {
                        if self.symbols.borrow(name, false, *span).is_none() {
                            self.error_occurred = true;
                            return None;
                        }
                        if let Some(current_scope) = self.borrowed_in_scope.last_mut() {
                            current_scope.push((name.clone(), *span));
                        }
                    } else {
                        emit_diagnostic(
                            &Diagnostic::warning(
                                "Slicing a String that is not a simple variable; borrow not tracked.",
                            )
                            .with_code("VX0280")
                            .with_span(*span),
                        );
                    }
                    Some(Type::Concrete("&str".to_string()))
                } else if base_ty == Type::Concrete("&str".to_string()) {
                    Some(Type::Concrete("&str".to_string()))
                } else {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Cannot slice non‑string type '{}'",
                            base_ty.to_string()
                        ))
                        .with_code("VX0281")
                        .with_span(*span),
                    );
                    self.error_occurred = true;
                    None
                }
            }
            ASTNode::StructLiteral { name, fields, span } => {
                let struct_info = match self.symbols.lookup_struct_info(name) {
                    Some(info) => info,
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!("Unknown struct '{}' in literal", name))
                                .with_code("VX0273")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                };
                let generic_params = struct_info.generic_params.clone();
                let type_name = if generic_params.is_empty() {
                    Type::Struct(name.clone(), vec![])
                } else {
                    let mut args = Vec::new();
                    for _ in &generic_params {
                        args.push(self.fresh_infer_var());
                    }
                    Type::Struct(name.clone(), args)
                };
                let resolved_type_name = self.unify.resolve(&type_name);
                let field_map = match self
                    .symbols
                    .resolve_concrete_struct(&resolved_type_name, *span)
                {
                    Some(map) => map,
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Failed to resolve struct '{}' fields",
                                resolved_type_name.to_string()
                            ))
                            .with_code("VX0273")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                };
                let mut provided_fields = std::collections::HashSet::new();
                for (field_name, expr) in fields {
                    provided_fields.insert(field_name.clone());
                    let expected_ty = match field_map.iter().find(|(fname, _)| fname == field_name)
                    {
                        Some((_, ty)) => ty.clone(),
                        None => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Struct '{}' has no field named '{}'",
                                    resolved_type_name.to_string(),
                                    field_name
                                ))
                                .with_code("VX0274")
                                .with_span(node_span(expr)),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    };
                    let actual_ty = match self.analyze_expression(expr, Some(&expected_ty)) {
                        Some(ty) => ty,
                        None => return None,
                    };
                    if !self.unify.unify(&expected_ty, &actual_ty, node_span(expr)) {
                        self.error_occurred = true;
                        return None;
                    }
                }
                if provided_fields.len() != field_map.len() {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Struct literal for '{}' must specify all {} fields (got {})",
                            resolved_type_name.to_string(),
                            field_map.len(),
                            fields.len()
                        ))
                        .with_code("VX0276")
                        .with_span(*span),
                    );
                    self.error_occurred = true;
                    return None;
                }
                Some(type_name)
            }
            ASTNode::CastExpr {
                expr,
                target_type,
                span: _,
            } => {
                if self.analyze_expression(expr, None).is_none() {
                    return None;
                }
                let gp_set = self
                    .current_generic_params
                    .as_ref()
                    .map(|v| v.iter().cloned().collect())
                    .unwrap_or(HashSet::new());
                Some(self.parse_type_str_with_imports(target_type, &gp_set))
            }
            ASTNode::CallExpr { callee, args, span } => {
                // Helper to release borrows taken for arguments (BorrowExpr)
                let release_borrows = |this: &mut Self| {
                    for arg in args {
                        if let ASTNode::BorrowExpr { expr, span, .. } = arg {
                            if let ASTNode::Identifier(name, _) = &**expr {
                                this.symbols.release_borrow(name, *span);
                                this.dbg(&format!("released borrow for '{}' after call", name));
                            }
                        }
                    }
                };

                // Built‑in assert
                if callee == "assert" {
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`assert` expects exactly 2 arguments: condition and message",
                            )
                            .with_code("VX9998")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let cond_ty = self.analyze_expression(&args[0], None)?;
                    if cond_ty != Type::Concrete("i32".to_string()) {
                        emit_diagnostic(
                            &Diagnostic::error("`assert` condition must be of type i32 (boolean)")
                                .with_code("VX9997")
                                .with_span(node_span(&args[0])),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let msg_ty = self.analyze_expression(&args[1], None)?;
                    if msg_ty != Type::Concrete("&str".to_string()) {
                        emit_diagnostic(
                            &Diagnostic::error("`assert` message must be of type &str")
                                .with_code("VX9996")
                                .with_span(node_span(&args[1])),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    release_borrows(self);
                    return Some(Type::Concrete("void".to_string()));
                }

                // String methods
                if callee == "String::new" {
                    if !args.is_empty() {
                        emit_diagnostic(
                            &Diagnostic::error("`String::new` expects no arguments")
                                .with_code("VX0282")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    return Some(Type::Concrete("String".to_string()));
                }
                if callee == "String::from" {
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`String::from` expects exactly one argument (a &str)",
                            )
                            .with_code("VX0283")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let arg_ty = self.analyze_expression(&args[0], None)?;
                    if arg_ty != Type::Concrete("&str".to_string()) {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "`String::from` expects `&str`, got `{}`",
                                arg_ty.to_string()
                            ))
                            .with_code("VX0284")
                            .with_span(node_span(&args[0])),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    return Some(Type::Concrete("String".to_string()));
                }
                if callee == "as_str" {
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`as_str` expects exactly one argument (a `String` or `&String`)",
                            )
                            .with_code("VX0285")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let arg_ty = self.analyze_expression(&args[0], None)?;
                    if arg_ty != Type::Concrete("String".to_string())
                        && arg_ty
                            != Type::Reference(
                                false,
                                Box::new(Type::Concrete("String".to_string())),
                            )
                    {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "`as_str` expects `String` or `&String`, got `{}`",
                                arg_ty.to_string()
                            ))
                            .with_code("VX0286")
                            .with_span(node_span(&args[0])),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    if let ASTNode::Identifier(name, _) = &args[0] {
                        if self
                            .symbols
                            .borrow(name, false, node_span(&args[0]))
                            .is_none()
                        {
                            self.error_occurred = true;
                            return None;
                        }
                        if let Some(current_scope) = self.borrowed_in_scope.last_mut() {
                            current_scope.push((name.clone(), node_span(&args[0])));
                        }
                    }
                    release_borrows(self);
                    return Some(Type::Concrete("&str".to_string()));
                }
                if callee == "push_str" {
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error("`push_str` expects exactly two arguments: a `&mut String` and a `&str`")
                                .with_code("VX0287")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let arg0_ty = self.analyze_expression(&args[0], None)?;
                    let is_mutable_ref = arg0_ty
                        == Type::Reference(true, Box::new(Type::Concrete("String".to_string())))
                        || arg0_ty == Type::Concrete("String".to_string());
                    if !is_mutable_ref {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "`push_str` first argument must be `&mut String` or `String`, got `{}`",
                                arg0_ty.to_string()
                            ))
                            .with_code("VX0288")
                            .with_span(node_span(&args[0])),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    if let ASTNode::Identifier(name, _) = &args[0] {
                        if !self.symbols.is_mutable(name) {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "`push_str` requires a mutable variable `mut {}`",
                                    name
                                ))
                                .with_code("VX0289")
                                .with_span(node_span(&args[0])),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                        if self
                            .symbols
                            .borrow(name, true, node_span(&args[0]))
                            .is_none()
                        {
                            self.error_occurred = true;
                            return None;
                        }
                        if let Some(current_scope) = self.borrowed_in_scope.last_mut() {
                            current_scope.push((name.clone(), node_span(&args[0])));
                        }
                    }
                    let arg1_ty = self.analyze_expression(&args[1], None)?;
                    if arg1_ty != Type::Concrete("&str".to_string()) {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "`push_str` second argument must be `&str`, got `{}`",
                                arg1_ty.to_string()
                            ))
                            .with_code("VX0290")
                            .with_span(node_span(&args[1])),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    release_borrows(self);
                    return Some(Type::Concrete("void".to_string()));
                }

                // as_ptr() method for &str and String
                if callee == "as_ptr" {
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`as_ptr` expects exactly one argument (a &str or String)",
                            )
                            .with_code("VX0291")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let arg_ty = match self.analyze_expression(&args[0], None) {
                        Some(ty) => ty,
                        None => return None,
                    };
                    let arg_ty_stripped = Self::strip_references(&arg_ty).clone();
                    let is_string = arg_ty_stripped == Type::Concrete("String".to_string())
                        || arg_ty_stripped == Type::Concrete("&str".to_string());
                    if !is_string {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "`as_ptr` can only be called on `&str` or `String`, got `{}`",
                                arg_ty_stripped.to_string()
                            ))
                            .with_code("VX0292")
                            .with_span(node_span(&args[0])),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    if let ASTNode::Identifier(name, _) = &args[0] {
                        if self
                            .symbols
                            .borrow(name, false, node_span(&args[0]))
                            .is_none()
                        {
                            self.error_occurred = true;
                            return None;
                        }
                        if let Some(current_scope) = self.borrowed_in_scope.last_mut() {
                            current_scope.push((name.clone(), node_span(&args[0])));
                        }
                    }
                    release_borrows(self);
                    return Some(Type::Concrete("*const u8".to_string()));
                }

                // Unqualified enum constructors (Some, None, Ok, Err)
                match callee.as_str() {
                    "Some" | "None" | "Ok" | "Err" => {
                        let (enum_name, variant_name, needs_payload) = match callee.as_str() {
                            "Some" => ("Option", "Some", true),
                            "None" => ("Option", "None", false),
                            "Ok" => ("Result", "Ok", true),
                            "Err" => ("Result", "Err", true),
                            _ => unreachable!(),
                        };

                        let enum_info = match self.symbols.lookup_enum(enum_name).cloned() {
                            Some(info) => info,
                            None => {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Built‑in enum '{}' not found in symbol table",
                                        enum_name
                                    ))
                                    .with_code("VX0503")
                                    .with_span(*span),
                                );
                                self.error_occurred = true;
                                return None;
                            }
                        };
                        let generic_params = enum_info.generic_params.clone();

                        let mut type_args = Vec::new();

                        if !needs_payload {
                            if let Some(expected_ty) = expected {
                                let resolved_expected =
                                    self.resolve_type(expected_ty.clone(), *span);
                                if let Type::Enum(name, args) = resolved_expected {
                                    if name == enum_name {
                                        type_args = args;
                                    }
                                }
                            }
                            while type_args.len() < generic_params.len() {
                                type_args.push(self.fresh_infer_var());
                            }
                        } else {
                            if args.len() != 1 {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "`{}` expects exactly one argument",
                                        callee
                                    ))
                                    .with_code("VX0504")
                                    .with_span(*span),
                                );
                                self.error_occurred = true;
                                return None;
                            }
                            let arg_ty = match self.analyze_expression(&args[0], None) {
                                Some(ty) => ty,
                                None => return None,
                            };

                            if enum_name == "Option" {
                                type_args.push(arg_ty);
                                if type_args.len() > generic_params.len() {
                                    type_args.truncate(generic_params.len());
                                }
                                while type_args.len() < generic_params.len() {
                                    type_args.push(self.fresh_infer_var());
                                }
                            } else {
                                if variant_name == "Ok" {
                                    type_args.push(arg_ty);
                                    type_args.push(self.fresh_infer_var());
                                } else {
                                    type_args.push(self.fresh_infer_var());
                                    type_args.push(arg_ty);
                                }
                                while type_args.len() < generic_params.len() {
                                    type_args.push(self.fresh_infer_var());
                                }
                            }
                        }

                        let enum_ty = Type::Enum(enum_name.to_string(), type_args);
                        let resolved_enum_ty = self.resolve_type(enum_ty, *span);

                        if let Some(expected_ty) = expected {
                            if !self.unify.unify(&resolved_enum_ty, expected_ty, *span) {
                                return None;
                            }
                        }

                        if needs_payload {
                            if let ASTNode::Identifier(name, _) = &args[0] {
                                if let Some((ty, alive, _)) = self.symbols.lookup_state(name) {
                                    if !alive {
                                        emit_diagnostic(
                                            &Diagnostic::error(&format!(
                                                "Use of moved value '{}' in `{}` constructor",
                                                name, callee
                                            ))
                                            .with_code("VX0505")
                                            .with_span(node_span(&args[0])),
                                        );
                                        self.error_occurred = true;
                                        return None;
                                    }
                                    if !SymbolTable::is_copy_type(&ty) {
                                        if !self.symbols.mark_moved(name, node_span(&args[0])) {
                                            self.error_occurred = true;
                                            return None;
                                        }
                                    }
                                }
                            }
                            for arg in args {
                                if let ASTNode::BorrowExpr { expr, span, .. } = arg {
                                    if let ASTNode::Identifier(name, _) = &**expr {
                                        self.symbols.release_borrow(name, *span);
                                    }
                                }
                            }
                        }

                        return Some(resolved_enum_ty);
                    }
                    _ => {}
                }

                // Qualified enum constructor
                if callee.contains("::") {
                    let parts: Vec<&str> = callee.split("::").collect();
                    if parts.len() == 2 {
                        let enum_name = parts[0];
                        let variant_name = parts[1];
                        let enum_info_clone = self.symbols.lookup_enum(enum_name).cloned();
                        if let Some(enum_info) = enum_info_clone {
                            let variant_idx = enum_info
                                .variants
                                .iter()
                                .position(|v| v.name == variant_name);
                            if let Some(idx) = variant_idx {
                                let payload_type_opt = enum_info.variant_payload_type[idx].clone();
                                let generic_params = enum_info.generic_params.clone();

                                if payload_type_opt.is_none() && !args.is_empty() {
                                    emit_diagnostic(
                                        &Diagnostic::error(&format!(
                                            "Variant `{}::{}` takes 0 arguments, got {}",
                                            enum_name,
                                            variant_name,
                                            args.len()
                                        ))
                                        .with_code("VX0300")
                                        .with_span(*span),
                                    );
                                    self.error_occurred = true;
                                    return None;
                                }
                                if payload_type_opt.is_some() && args.len() != 1 {
                                    emit_diagnostic(
                                        &Diagnostic::error(&format!(
                                            "Variant `{}::{}` takes 1 argument, got {}",
                                            enum_name,
                                            variant_name,
                                            args.len()
                                        ))
                                        .with_code("VX0301")
                                        .with_span(*span),
                                    );
                                    self.error_occurred = true;
                                    return None;
                                }

                                let concrete_type = if let Some(_payload_ty) = payload_type_opt {
                                    let arg_ty = self.analyze_expression(&args[0], None)?;
                                    let mut subst = std::collections::HashMap::new();
                                    if generic_params.len() == 1 {
                                        let gp = &generic_params[0];
                                        subst.insert(gp.clone(), arg_ty.clone());
                                    } else if generic_params.len() == 2 {
                                        if variant_name == "Ok" {
                                            subst.insert(generic_params[0].clone(), arg_ty.clone());
                                        } else if variant_name == "Err" {
                                            subst.insert(generic_params[1].clone(), arg_ty.clone());
                                        }
                                    }
                                    let args: Vec<Type> = generic_params
                                        .iter()
                                        .map(|gp| {
                                            subst
                                                .get(gp)
                                                .cloned()
                                                .unwrap_or_else(|| self.fresh_infer_var())
                                        })
                                        .collect();
                                    Type::Enum(enum_name.to_string(), args)
                                } else {
                                    if generic_params.is_empty() {
                                        Type::Enum(enum_name.to_string(), vec![])
                                    } else {
                                        let args: Vec<Type> = generic_params
                                            .iter()
                                            .map(|_| self.fresh_infer_var())
                                            .collect();
                                        Type::Enum(enum_name.to_string(), args)
                                    }
                                };

                                let resolved_concrete = self.resolve_type(concrete_type, *span);
                                if let Some(expected_ty) = expected {
                                    let resolved_expected =
                                        self.resolve_type(expected_ty.clone(), *span);
                                    if !self.unify.unify(
                                        &resolved_concrete,
                                        &resolved_expected,
                                        *span,
                                    ) {
                                        self.error_occurred = true;
                                        return None;
                                    }
                                }

                                if let Some(arg) = args.first() {
                                    if let ASTNode::Identifier(name, _) = arg {
                                        if let Some((ty, alive, _)) =
                                            self.symbols.lookup_state(name)
                                        {
                                            if !alive {
                                                emit_diagnostic(
                                                    &Diagnostic::error(&format!(
                                                        "Use of moved value '{}'",
                                                        name
                                                    ))
                                                    .with_code("VX0303")
                                                    .with_span(node_span(arg)),
                                                );
                                                self.error_occurred = true;
                                                return None;
                                            }
                                            if !SymbolTable::is_copy_type(&ty) {
                                                self.symbols.mark_moved(name, node_span(arg));
                                            }
                                        }
                                    }
                                }

                                for arg in args {
                                    if let ASTNode::BorrowExpr { expr, span, .. } = arg {
                                        if let ASTNode::Identifier(name, _) = &**expr {
                                            self.symbols.release_borrow(name, *span);
                                        }
                                    }
                                }
                                return Some(resolved_concrete);
                            }
                        }
                    }
                }

                // Module‑qualified function call
                if callee.contains("::") {
                    let parts: Vec<&str> = callee.split("::").collect();
                    if parts.len() == 2 {
                        let module_name = parts[0];
                        let func_name = parts[1];
                        if self.symbols.lookup_module(module_name).is_some() {
                            if let Some(crate::semantics::symbol::QualifiedSymbol::Function {
                                param_types: expected_params,
                                return_type,
                                ..
                            }) = self.symbols.lookup_qualified(module_name, func_name)
                            {
                                let full_callee = callee.clone();
                                if expected_params.len() != args.len() {
                                    emit_diagnostic(
                                        &Diagnostic::error(&format!(
                                            "Function '{}' expects {} arguments, got {}.",
                                            full_callee,
                                            expected_params.len(),
                                            args.len()
                                        ))
                                        .with_code("VX0236")
                                        .with_span(*span),
                                    );
                                    self.error_occurred = true;
                                    return None;
                                }

                                for (i, arg) in args.iter().enumerate() {
                                    let arg_type = match self
                                        .analyze_expression(arg, Some(&expected_params[i]))
                                    {
                                        Some(t) => t,
                                        None => return None,
                                    };
                                    if !self.unify.unify(
                                        &expected_params[i],
                                        &arg_type,
                                        node_span(arg),
                                    ) {
                                        let span = node_span(arg);
                                        emit_diagnostic(
                                            &Diagnostic::error(&format!(
                                                "Argument {} mismatch for call to '{}'.",
                                                i, full_callee
                                            ))
                                            .with_code("VX0237")
                                            .with_span(span)
                                            .with_suggestion(Suggestion {
                                                message: format!(
                                                    "Cast explicitly: [expr] as {}",
                                                    expected_params[i].to_string()
                                                ),
                                                span: Some(span),
                                            }),
                                        );
                                        self.error_occurred = true;
                                        return None;
                                    }
                                }

                                for arg in args {
                                    if let ASTNode::Identifier(name, _) = arg {
                                        if let Some((ty, alive, _)) =
                                            self.symbols.lookup_state(name)
                                        {
                                            if !alive {
                                                let span = node_span(arg);
                                                emit_diagnostic(
                                                    &Diagnostic::error(&format!("Use of moved or mutably borrowed value: '{}'", name))
                                                        .with_code("VX0238")
                                                        .with_span(span),
                                                );
                                                self.error_occurred = true;
                                                return None;
                                            }
                                            if !SymbolTable::is_copy_type(&ty) {
                                                if !self.symbols.mark_moved(name, node_span(arg)) {
                                                    self.error_occurred = true;
                                                }
                                            }
                                        }
                                    }
                                }

                                return Some(return_type);
                            } else {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Cannot find function '{}' in module '{}'",
                                        func_name, module_name
                                    ))
                                    .with_code("VX0315")
                                    .with_span(*span),
                                );
                                self.error_occurred = true;
                                return None;
                            }
                        }
                    }
                }

                // Vec::new constructor
                if callee == "Vec::new" {
                    if !args.is_empty() {
                        emit_diagnostic(
                            &Diagnostic::error("`Vec::new` expects no arguments")
                                .with_code("VX0501")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let elem_ty = match expected {
                        Some(Type::Struct(name, type_args))
                            if name == "Vec" && type_args.len() == 1 =>
                        {
                            type_args[0].clone()
                        }
                        _ => self.fresh_infer_var(),
                    };
                    let vec_ty = Type::Struct("Vec".to_string(), vec![elem_ty]);
                    let resolved_vec_ty = self.resolve_type(vec_ty, *span);
                    return Some(resolved_vec_ty);
                }

                // HashMap built‑ins
                if callee == "HashMap::new" {
                    if !args.is_empty() {
                        emit_diagnostic(
                            &Diagnostic::error("`HashMap::new` expects no arguments")
                                .with_code("VX0601")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let (k_ty, v_ty) = match expected {
                        Some(Type::Struct(name, type_args))
                            if name == "HashMap" && type_args.len() == 2 =>
                        {
                            (type_args[0].clone(), type_args[1].clone())
                        }
                        _ => (self.fresh_infer_var(), self.fresh_infer_var()),
                    };
                    let map_ty = Type::Struct("HashMap".to_string(), vec![k_ty, v_ty]);
                    return Some(self.resolve_type(map_ty, *span));
                }

                if callee == "insert" {
                    if args.len() != 3 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`insert` expects exactly 3 arguments: map, key, value",
                            )
                            .with_code("VX0602")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let map_ty = self.analyze_expression(&args[0], None)?;
                    match map_ty {
                        Type::Struct(name, type_args)
                            if name == "HashMap" && type_args.len() == 2 =>
                        {
                            let k_ty = &type_args[0];
                            let v_ty = &type_args[1];
                            let key_ty = self.analyze_expression(&args[1], Some(k_ty))?;
                            let val_ty = self.analyze_expression(&args[2], Some(v_ty))?;
                            if !self.unify.unify(k_ty, &key_ty, node_span(&args[1])) {
                                self.error_occurred = true;
                                return None;
                            }
                            if !self.unify.unify(v_ty, &val_ty, node_span(&args[2])) {
                                self.error_occurred = true;
                                return None;
                            }
                            for (_idx, arg) in [&args[1], &args[2]].iter().enumerate() {
                                if let ASTNode::Identifier(name, _) = arg {
                                    if let Some((ty, alive, _)) = self.symbols.lookup_state(name) {
                                        if !alive {
                                            emit_diagnostic(
                                                &Diagnostic::error(&format!(
                                                    "Use of moved value '{}' in `insert`",
                                                    name
                                                ))
                                                .with_code("VX0603")
                                                .with_span(node_span(arg)),
                                            );
                                            self.error_occurred = true;
                                            return None;
                                        }
                                        if !SymbolTable::is_copy_type(&ty) {
                                            if !self.symbols.mark_moved(name, node_span(arg)) {
                                                self.error_occurred = true;
                                                return None;
                                            }
                                        }
                                    }
                                }
                            }
                            release_borrows(self);
                            return Some(Type::Concrete("void".to_string()));
                        }
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "`insert` called on non‑HashMap type `{}`",
                                    map_ty.to_string()
                                ))
                                .with_code("VX0604")
                                .with_span(node_span(&args[0])),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    }
                }

                if callee == "get" {
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error("`get` expects exactly 2 arguments: map, key")
                                .with_code("VX0605")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let map_ty = self.analyze_expression(&args[0], None)?;
                    match map_ty {
                        Type::Struct(name, type_args)
                            if name == "HashMap" && type_args.len() == 2 =>
                        {
                            let k_ty = &type_args[0];
                            let v_ty = &type_args[1];
                            let key_ty = self.analyze_expression(&args[1], Some(k_ty))?;
                            if !self.unify.unify(k_ty, &key_ty, node_span(&args[1])) {
                                self.error_occurred = true;
                                return None;
                            }
                            let option_ty = Type::Enum("Option".to_string(), vec![v_ty.clone()]);
                            release_borrows(self);
                            return Some(option_ty);
                        }
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "`get` called on non‑HashMap type `{}`",
                                    map_ty.to_string()
                                ))
                                .with_code("VX0606")
                                .with_span(node_span(&args[0])),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    }
                }

                if callee == "contains_key" {
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`contains_key` expects exactly 2 arguments: map, key",
                            )
                            .with_code("VX0607")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let map_ty = self.analyze_expression(&args[0], None)?;
                    match map_ty {
                        Type::Struct(name, type_args)
                            if name == "HashMap" && type_args.len() == 2 =>
                        {
                            let k_ty = &type_args[0];
                            let key_ty = self.analyze_expression(&args[1], Some(k_ty))?;
                            if !self.unify.unify(k_ty, &key_ty, node_span(&args[1])) {
                                self.error_occurred = true;
                                return None;
                            }
                            release_borrows(self);
                            return Some(Type::Concrete("i32".to_string()));
                        }
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "`contains_key` called on non‑HashMap type `{}`",
                                    map_ty.to_string()
                                ))
                                .with_code("VX0608")
                                .with_span(node_span(&args[0])),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    }
                }

                if callee == "remove" {
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error("`remove` expects exactly 2 arguments: map, key")
                                .with_code("VX0609")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let map_ty = self.analyze_expression(&args[0], None)?;
                    match map_ty {
                        Type::Struct(name, type_args)
                            if name == "HashMap" && type_args.len() == 2 =>
                        {
                            let k_ty = &type_args[0];
                            let v_ty = &type_args[1];
                            let key_ty = self.analyze_expression(&args[1], Some(k_ty))?;
                            if !self.unify.unify(k_ty, &key_ty, node_span(&args[1])) {
                                self.error_occurred = true;
                                return None;
                            }
                            let option_ty = Type::Enum("Option".to_string(), vec![v_ty.clone()]);
                            release_borrows(self);
                            return Some(option_ty);
                        }
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "`remove` called on non‑HashMap type `{}`",
                                    map_ty.to_string()
                                ))
                                .with_code("VX0610")
                                .with_span(node_span(&args[0])),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    }
                }

                // Struct constructor (unqualified)
                if let Some(_fields) = self.symbols.lookup_struct(callee) {
                    let generic_params =
                        if let Some(struct_info) = self.symbols.lookup_struct_info(callee) {
                            struct_info.generic_params.clone()
                        } else {
                            emit_diagnostic(
                                &Diagnostic::error(&format!("Unknown struct '{}'", callee))
                                    .with_code("VX0273")
                                    .with_span(*span),
                            );
                            return None;
                        };
                    let struct_field_count =
                        if let Some(struct_info) = self.symbols.lookup_struct_info(callee) {
                            struct_info.fields.len()
                        } else {
                            0
                        };
                    if args.len() != struct_field_count {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Struct '{}' expects {} fields, got {} arguments.",
                                callee,
                                struct_field_count,
                                args.len()
                            ))
                            .with_code("VX0272")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }

                    let struct_type = if generic_params.is_empty() {
                        Type::Struct(callee.to_string(), vec![])
                    } else {
                        let mut arg_types = Vec::new();
                        for (i, _gp) in generic_params.iter().enumerate() {
                            if let Some(expected_ty) = expected {
                                if let Type::Struct(name, concrete_args) = expected_ty {
                                    if name == callee && i < concrete_args.len() {
                                        arg_types.push(concrete_args[i].clone());
                                        continue;
                                    }
                                }
                            }
                            if i < args.len() {
                                let arg_ty = self.analyze_expression(&args[i], None)?;
                                arg_types.push(arg_ty);
                            } else {
                                arg_types.push(self.fresh_infer_var());
                            }
                        }
                        Type::Struct(callee.to_string(), arg_types)
                    };
                    return Some(struct_type);
                }

                // Built‑in copy
                if callee == "copy" {
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error("`copy` expects exactly one argument.")
                                .with_code("VX0234")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let arg_type = match self.analyze_expression(&args[0], None) {
                        Some(t) => t,
                        None => return None,
                    };
                    if !SymbolTable::is_copy_type(&arg_type.to_string()) {
                        let span = node_span(&args[0]);
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot copy non‑copy type '{}'.",
                                arg_type.to_string()
                            ))
                            .with_code("VX0235")
                            .with_span(span)
                            .with_suggestion(Suggestion {
                                message:
                                    "Only primitive types, pointers, and references are copyable."
                                        .to_string(),
                                span: Some(span),
                            }),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    return Some(arg_type);
                }

                // push, pop, len built‑ins (extended to handle Vec)
                if callee == "push" {
                    if args.len() != 2 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`push` expects exactly 2 arguments: container and value",
                            )
                            .with_code("VX0432")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let container_ty = match self.analyze_expression(&args[0], None) {
                        Some(ty) => ty,
                        None => return None,
                    };
                    match container_ty {
                        Type::Struct(name, type_args) if name == "Vec" && type_args.len() == 1 => {
                            let elem_ty = &type_args[0];
                            let val_ty = match self.analyze_expression(&args[1], Some(elem_ty)) {
                                Some(t) => t,
                                None => return None,
                            };
                            if !self.unify.unify(elem_ty, &val_ty, node_span(&args[1])) {
                                self.error_occurred = true;
                                return None;
                            }
                            if let ASTNode::Identifier(name, _) = &args[1] {
                                if let Some((ty, alive, _)) = self.symbols.lookup_state(name) {
                                    if !alive {
                                        emit_diagnostic(
                                            &Diagnostic::error(&format!(
                                                "Use of moved value '{}'",
                                                name
                                            ))
                                            .with_code("VX0502")
                                            .with_span(node_span(&args[1])),
                                        );
                                        self.error_occurred = true;
                                        return None;
                                    }
                                    if !SymbolTable::is_copy_type(&ty) {
                                        if !self.symbols.mark_moved(name, node_span(&args[1])) {
                                            self.error_occurred = true;
                                        }
                                    }
                                }
                            }
                            for arg in args {
                                if let ASTNode::BorrowExpr { expr, span, .. } = arg {
                                    if let ASTNode::Identifier(name, _) = &**expr {
                                        self.symbols.release_borrow(name, *span);
                                    }
                                }
                            }
                            return Some(Type::Concrete("void".to_string()));
                        }
                        Type::Array(_, None) => {
                            let elem_ty = match &container_ty {
                                Type::Array(elem, None) => elem.as_ref().clone(),
                                _ => unreachable!(),
                            };
                            let val_ty = match self.analyze_expression(&args[1], Some(&elem_ty)) {
                                Some(t) => t,
                                None => return None,
                            };
                            if !self.unify.unify(&elem_ty, &val_ty, node_span(&args[1])) {
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Type mismatch in `push`: expected `{}`, got `{}`",
                                        elem_ty.to_string(),
                                        val_ty.to_string()
                                    ))
                                    .with_code("VX0442")
                                    .with_span(node_span(&args[1])),
                                );
                                self.error_occurred = true;
                                return None;
                            }
                            release_borrows(self);
                            return Some(Type::Concrete("void".to_string()));
                        }
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "`push` requires a dynamic array or Vec, got `{}`",
                                    container_ty.to_string()
                                ))
                                .with_code("VX0441")
                                .with_span(node_span(&args[0])),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    }
                }
                if callee == "pop" {
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error("`pop` expects exactly 1 argument: container")
                                .with_code("VX0434")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let container_ty = match self.analyze_expression(&args[0], None) {
                        Some(ty) => ty,
                        None => return None,
                    };
                    match container_ty {
                        Type::Struct(name, type_args) if name == "Vec" && type_args.len() == 1 => {
                            let elem_ty = type_args[0].clone();
                            let option_ty = Type::Enum("Option".to_string(), vec![elem_ty]);
                            release_borrows(self);
                            return Some(option_ty);
                        }
                        Type::Array(_, None) => {
                            let elem_ty = match &container_ty {
                                Type::Array(elem, None) => elem.as_ref().clone(),
                                _ => unreachable!(),
                            };
                            release_borrows(self);
                            return Some(elem_ty);
                        }
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "`pop` requires a dynamic array or Vec, got `{}`",
                                    container_ty.to_string()
                                ))
                                .with_code("VX0444")
                                .with_span(node_span(&args[0])),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    }
                }
                if callee == "len" {
                    if args.len() != 1 {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "`len` expects exactly 1 argument: container or string",
                            )
                            .with_code("VX0436")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    let arg_ty = match self.analyze_expression(&args[0], None) {
                        Some(ty) => ty,
                        None => return None,
                    };
                    if let Type::Struct(name, _) = &arg_ty {
                        if name == "Vec" || name == "HashMap" {
                            release_borrows(self);
                            return Some(Type::Concrete("i32".to_string()));
                        }
                    }
                    if matches!(arg_ty, Type::Array(_, None))
                        || arg_ty == Type::Concrete("String".to_string())
                        || arg_ty == Type::Concrete("&str".to_string())
                    {
                        release_borrows(self);
                        return Some(Type::Concrete("i32".to_string()));
                    }
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "`len` requires a dynamic array, Vec, String, or &str, got `{}`",
                            arg_ty.to_string()
                        ))
                        .with_code("VX0437")
                        .with_span(node_span(&args[0])),
                    );
                    self.error_occurred = true;
                    return None;
                }

                // Regular function call – possibly generic
                let (
                    expected_params,
                    _param_refinements,
                    return_type,
                    _return_refinement,
                    generic_params,
                    _is_kernel,
                ) = if let Some((ptypes, pref, ret, retref, gparams, _is_kernel)) =
                    self.symbols.lookup_function(callee)
                {
                    (ptypes, pref, ret, retref, gparams, _is_kernel)
                } else {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Undefined function or struct constructor: '{}'",
                            callee
                        ))
                        .with_code("VX0239")
                        .with_span(*span),
                    );
                    self.error_occurred = true;
                    return None;
                };

                let func_generic_params = generic_params.clone();

                if !func_generic_params.is_empty() {
                    let mut generic_vars = std::collections::HashMap::new();
                    for gp in &func_generic_params {
                        let var = self.fresh_infer_var();
                        generic_vars.insert(gp.clone(), var);
                    }

                    let substitute_params = |ty: &Type| -> Type { ty.substitute(&generic_vars) };

                    for (i, param_ty) in expected_params.iter().enumerate() {
                        if i >= args.len() {
                            break;
                        }
                        let substituted_param = substitute_params(param_ty);
                        let arg_ty =
                            match self.analyze_expression(&args[i], Some(&substituted_param)) {
                                Some(t) => t,
                                None => return None,
                            };
                        if !self.unify.unify(&substituted_param, &arg_ty, *span) {
                            self.error_occurred = true;
                            return None;
                        }
                    }

                    let substituted_return = substitute_params(&return_type);
                    if let Some(expected_ty) = expected {
                        if !self.unify.unify(&substituted_return, expected_ty, *span) {
                            self.error_occurred = true;
                            return None;
                        }
                    }

                    let resolved_return = substituted_return;

                    for arg in args {
                        if let ASTNode::Identifier(name, _) = arg {
                            if let Some((ty, alive, _)) = self.symbols.lookup_state(name) {
                                if !alive {
                                    let span = node_span(arg);
                                    emit_diagnostic(
                                        &Diagnostic::error(&format!(
                                            "Use of moved or mutably borrowed value: '{}'",
                                            name
                                        ))
                                        .with_code("VX0238")
                                        .with_span(span),
                                    );
                                    self.error_occurred = true;
                                    return None;
                                }
                                if !SymbolTable::is_copy_type(&ty) {
                                    if !self.symbols.mark_moved(name, node_span(arg)) {
                                        self.error_occurred = true;
                                    }
                                }
                            }
                        }
                    }
                    release_borrows(self);
                    Some(resolved_return)
                } else {
                    if expected_params.len() != args.len() {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Function '{}' expects {} arguments, got {}.",
                                callee,
                                expected_params.len(),
                                args.len()
                            ))
                            .with_code("VX0236")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                    for (i, arg) in args.iter().enumerate() {
                        let arg_type = match self.analyze_expression(arg, Some(&expected_params[i]))
                        {
                            Some(t) => t,
                            None => return None,
                        };
                        if !self
                            .unify
                            .unify(&expected_params[i], &arg_type, node_span(arg))
                        {
                            let span = node_span(arg);
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Argument {} mismatch for call to '{}'.",
                                    i, callee
                                ))
                                .with_code("VX0237")
                                .with_span(span)
                                .with_suggestion(Suggestion {
                                    message: format!(
                                        "Cast explicitly: [expr] as {}",
                                        expected_params[i].to_string()
                                    ),
                                    span: Some(span),
                                }),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    }
                    for arg in args {
                        if let ASTNode::Identifier(name, _) = arg {
                            if let Some((ty, alive, _)) = self.symbols.lookup_state(name) {
                                if !alive {
                                    let span = node_span(arg);
                                    emit_diagnostic(
                                        &Diagnostic::error(&format!(
                                            "Use of moved or mutably borrowed value: '{}'",
                                            name
                                        ))
                                        .with_code("VX0238")
                                        .with_span(span),
                                    );
                                    self.error_occurred = true;
                                    return None;
                                }
                                if !SymbolTable::is_copy_type(&ty) {
                                    if !self.symbols.mark_moved(name, node_span(arg)) {
                                        self.error_occurred = true;
                                    }
                                }
                            }
                        }
                    }
                    release_borrows(self);
                    Some(return_type)
                }
            }
            ASTNode::KernelLaunch { kernel, grid, args, span } => {
                // Resolve kernel name
                let kernel_name = match kernel.as_ref() {
                    ASTNode::Identifier(name, _) => name,
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error("Expected identifier for kernel name in launch")
                                .with_code("VX0296")
                                .with_span(node_span(kernel)),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                };
                // Lookup function info; must be a kernel
                let (param_types, _, _, _, _, is_kernel) =
                    match self.symbols.lookup_function(kernel_name) {
                        Some(info) => info,
                        None => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!("Unknown function '{}' used as kernel", kernel_name))
                                    .with_code("VX0297")
                                    .with_span(*span),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                    };
                if !is_kernel {
                    emit_diagnostic(
                        &Diagnostic::error(&format!("'{}' is not a kernel (missing @kernel)", kernel_name))
                            .with_code("VX0298")
                            .with_span(*span),
                    );
                    self.error_occurred = true;
                    return None;
                }
                if args.len() != param_types.len() {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Kernel '{}' expects {} arguments, got {}",
                            kernel_name,
                            param_types.len(),
                            args.len()
                        ))
                        .with_code("VX0299")
                        .with_span(*span),
                    );
                    self.error_occurred = true;
                    return None;
                }
                // Type-check each argument
                for (i, arg) in args.iter().enumerate() {
                    let arg_ty = match self.analyze_expression(arg, Some(&param_types[i])) {
                        Some(ty) => ty,
                        None => return None,
                    };
                    if !self.unify.unify(&param_types[i], &arg_ty, node_span(arg)) {
                        let span = node_span(arg);
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Kernel argument {} mismatch: expected `{}`, got `{}`",
                                i,
                                param_types[i].to_string(),
                                arg_ty.to_string()
                            ))
                            .with_code("VX0300")
                            .with_span(span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                }
                // Type-check grid expressions (must be i32)
                let (gx, gy, gz) = grid;
                let mut check_grid = |expr: &ASTNode, dim: &str| -> bool {
                    let ty = match self.analyze_expression(expr, Some(&Type::Concrete("i32".to_string()))) {
                        Some(t) => t,
                        None => return false,
                    };
                    if ty != Type::Concrete("i32".to_string()) {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Kernel grid dimension '{}' must be i32, got {}",
                                dim,
                                ty.to_string()
                            ))
                            .with_code("VX0301")
                            .with_span(node_span(expr)),
                        );
                        self.error_occurred = true;
                        false
                    } else {
                        true
                    }
                };
                if !check_grid(gx, "grid_x") || !check_grid(gy, "grid_y") || !check_grid(gz, "grid_z") {
                    return None;
                }

                // Handle move/borrow for arguments (same as regular function call)
                for arg in args {
                    if let ASTNode::Identifier(name, _) = arg {
                        if let Some((ty, alive, _)) = self.symbols.lookup_state(name) {
                            if !alive {
                                let span = node_span(arg);
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Use of moved or mutably borrowed value: '{}'",
                                        name
                                    ))
                                    .with_code("VX0302")
                                    .with_span(span),
                                );
                                self.error_occurred = true;
                                return None;
                            }
                            if !SymbolTable::is_copy_type(&ty) {
                                if !self.symbols.mark_moved(name, node_span(arg)) {
                                    self.error_occurred = true;
                                }
                            }
                        }
                    }
                    if let ASTNode::BorrowExpr { expr, span, .. } = arg {
                        if let ASTNode::Identifier(name, _) = &**expr {
                            self.symbols.release_borrow(name, *span);
                        }
                    }
                }

                // Kernel launch returns void
                Some(Type::Concrete("void".to_string()))
            }
            ASTNode::BorrowExpr {
                mutable,
                expr,
                span,
            } => {
                let var_name = match &**expr {
                    ASTNode::Identifier(name, _) => name,
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "Borrow expression must be applied to an identifier (e.g., &x)",
                            )
                            .with_code("VX0240")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                };
                let span = node_span(expr);
                match self.symbols.borrow(var_name, *mutable, span) {
                    Some(ty) => {
                        let resolved_ty = self.resolve_type(ty, span);
                        if let Some(current_scope) = self.borrowed_in_scope.last_mut() {
                            current_scope.push((var_name.clone(), span));
                        }
                        if let Some(expected_ty) = expected {
                            let resolved_expected = self.resolve_type(expected_ty.clone(), span);
                            if !self.unify.unify(&resolved_ty, &resolved_expected, span) {
                                self.error_occurred = true;
                                return None;
                            }
                        }
                        Some(resolved_ty)
                    }
                    None => {
                        self.error_occurred = true;
                        None
                    }
                }
            }
            ASTNode::DerefExpr(inner, span) => {
                let inner_ty = match self.analyze_expression(inner, None) {
                    Some(t) => t,
                    None => return None,
                };
                if self.in_kernel {
                    let var_name = match &**inner {
                        ASTNode::Identifier(name, _) => Some(name.as_str()),
                        _ => None,
                    };
                    let is_device = var_name.map_or(false, |n| self.symbols.is_device_var(n));
                    if !is_device {
                        let name_str = var_name.unwrap_or("?");
                        emit_diagnostic(
                            &Diagnostic::error(&format!("Cannot dereference host pointer '{}' inside @kernel. Use @device memory or copy to device first.", name_str))
                                .with_code("VX0241")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                }
                let deref_ty = match inner_ty {
                    Type::Reference(_, inner) => inner.as_ref().clone(),
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Cannot dereference non‑reference type '{}'",
                                inner_ty.to_string()
                            ))
                            .with_code("VX0242")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        None
                    }?,
                };
                if let Some(expected_ty) = expected {
                    if !self.unify.unify(&deref_ty, expected_ty, *span) {
                        self.error_occurred = true;
                        return None;
                    }
                }
                Some(deref_ty)
            }
            ASTNode::BinaryExpr {
                left,
                op,
                right,
                span,
            } => {
                let left_ty = match self.analyze_expression(left, None) {
                    Some(t) => t,
                    None => return None,
                };
                let right_ty = match self.analyze_expression(right, None) {
                    Some(t) => t,
                    None => return None,
                };
                let left_ty_resolved = self.unify.resolve(&left_ty);
                let right_ty_resolved = self.unify.resolve(&right_ty);
                if !self
                    .unify
                    .unify(&left_ty_resolved, &right_ty_resolved, *span)
                {
                    emit_diagnostic(
                        &Diagnostic::error(&format!(
                            "Binary operator {:?} requires same types.",
                            op
                        ))
                        .with_code("VX0243")
                        .with_span(*span)
                        .with_suggestion(Suggestion {
                            message: "Cast both sides to a common type.".to_string(),
                            span: Some(*span),
                        }),
                    );
                    self.error_occurred = true;
                    return None;
                }
                match op {
                    TokenKind::And | TokenKind::Or => {
                        if left_ty_resolved != Type::Concrete("i32".to_string()) {
                            emit_diagnostic(
                                &Diagnostic::error(&format!("Logical operator {:?} requires i32 operands (boolean), got {}.", op, left_ty_resolved.to_string()))
                                    .with_code("VX0247")
                                    .with_span(*span),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                        Some(Type::Concrete("i32".to_string()))
                    }
                    TokenKind::Equal
                    | TokenKind::NotEqual
                    | TokenKind::LessThan
                    | TokenKind::GreaterThan
                    | TokenKind::LessThanOrEqual
                    | TokenKind::GreaterThanOrEqual => Some(Type::Concrete("i32".to_string())),
                    TokenKind::Ampersand
                    | TokenKind::Pipe
                    | TokenKind::Caret
                    | TokenKind::Shl
                    | TokenKind::Shr => {
                        if !Self::is_integer_type(&left_ty_resolved.to_string()) {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Bitwise operator {:?} requires integer operands, got {}",
                                    op,
                                    left_ty_resolved.to_string()
                                ))
                                .with_code("VX0248")
                                .with_span(*span),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                        Some(left_ty_resolved)
                    }
                    TokenKind::Plus
                    | TokenKind::Minus
                    | TokenKind::Star
                    | TokenKind::Div
                    | TokenKind::Mod => {
                        if !Self::is_arithmetic_type(&left_ty_resolved.to_string()) {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Arithmetic operator {:?} requires numeric operands, got {}",
                                    op,
                                    left_ty_resolved.to_string()
                                ))
                                .with_code("VX0249")
                                .with_span(*span),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                        Some(left_ty_resolved)
                    }
                    _ => Some(left_ty_resolved),
                }
            }
            ASTNode::UnaryExpr { op, expr, span } => {
                let inner_ty = match self.analyze_expression(expr, None) {
                    Some(t) => t,
                    None => return None,
                };
                let resolved_inner_ty = self.unify.resolve(&inner_ty);
                match op {
                    TokenKind::Not => {
                        if resolved_inner_ty != Type::Concrete("i32".to_string()) {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Logical 'not' requires i32 operand, got {}",
                                    resolved_inner_ty.to_string()
                                ))
                                .with_code("VX0250")
                                .with_span(*span),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                        Some(Type::Concrete("i32".to_string()))
                    }
                    TokenKind::Minus => {
                        if !Self::is_arithmetic_type(&resolved_inner_ty.to_string()) {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Unary minus requires numeric operand, got {}",
                                    resolved_inner_ty.to_string()
                                ))
                                .with_code("VX0251")
                                .with_span(*span),
                            );
                            self.error_occurred = true;
                            return None;
                        }
                        Some(resolved_inner_ty)
                    }
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!("Unsupported unary operator {:?}", op))
                                .with_code("VX0252")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        None
                    }
                }
            }
            ASTNode::RefinedType { condition, .. } => self.analyze_expression(condition, None),
            ASTNode::ComptimeBlock { body, .. } => {
                if let Some(evaluated) = ComptimeEvaluator::evaluate(node) {
                    self.analyze_expression(&evaluated, expected)
                } else {
                    let mut last_type = None;
                    for stmt in body {
                        match stmt {
                            ASTNode::IntegerLiteral(_, _)
                            | ASTNode::FloatLiteral(_, _)
                            | ASTNode::CharLiteral(_, _)
                            | ASTNode::StringLiteral(_, _)
                            | ASTNode::CallExpr { .. }
                            | ASTNode::BinaryExpr { .. }
                            | ASTNode::UnaryExpr { .. } => {
                                last_type = self.analyze_expression(stmt, expected);
                            }
                            _ => {
                                self.analyze_statement(stmt);
                            }
                        }
                    }
                    last_type.or(Some(Type::Concrete("i32".to_string())))
                }
            }
            ASTNode::TryExpr { expr, span } => {
                self.dbg(&format!("Expanding TryExpr at {:?}", span));
                let inner_ty = match self.analyze_expression(expr, None) {
                    Some(ty) => ty,
                    None => return None,
                };
                let resolved_inner_ty = self.unify.resolve(&inner_ty);
                let inner_ty_stripped = Self::strip_references(&resolved_inner_ty).clone();
                // Resolve the type to its canonical enum representation (e.g., Result<i32,&str> -> Enum)
                let inner_ty_canonical = self.resolve_type(inner_ty_stripped.clone(), *span);

                let (enum_name, variant_ok, variant_err) = match &inner_ty_canonical {
                    Type::Enum(name, args) if name == "Result" && args.len() == 2 => {
                        ("Result", "Ok", "Err")
                    }
                    Type::Enum(name, args) if name == "Option" && args.len() == 1 => {
                        ("Option", "Some", "None")
                    }
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "The `?` operator can only be applied to `Result` or `Option` types, got `{}`",
                                inner_ty_canonical.to_string()
                            ))
                            .with_code("VX0600")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                };

                let expected_return = match &self.current_return_type {
                    Some(ty) => ty.clone(),
                    None => {
                        emit_diagnostic(
                            &Diagnostic::error("`?` used outside of a function body")
                                .with_code("VX0601")
                                .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                };
                let expected_return_stripped = Self::strip_references(&expected_return);
                match (enum_name, expected_return_stripped) {
                    ("Result", Type::Enum(name, _)) if name == "Result" => {}
                    ("Option", Type::Enum(name, _)) if name == "Option" => {}
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(&format!(
                                "Function return type `{}` is not compatible with `?` on `{}`",
                                expected_return_stripped.to_string(),
                                enum_name
                            ))
                            .with_code("VX0602")
                            .with_span(*span),
                        );
                        self.error_occurred = true;
                        return None;
                    }
                }

                let ok_tmp = self.fresh_tmp_var();
                let err_tmp = self.fresh_tmp_var();

                let scrutinee = if resolved_inner_ty.is_reference() {
                    Box::new(ASTNode::DerefExpr(expr.clone(), *span))
                } else {
                    expr.clone()
                };

                let success_arm = MatchArm {
                    pattern: MatchPattern::Binding {
                        by_ref: false,
                        enum_name: enum_name.to_string(),
                        variant: variant_ok.to_string(),
                        bindings: vec![ok_tmp.clone()],
                        span: *span,
                    },
                    body: vec![ASTNode::Identifier(ok_tmp, *span)],
                    span: *span,
                };

                // Build the error return value as a proper enum variant constructor.
                let error_value = if enum_name == "Result" {
                    // Result::Err(err_tmp)
                    ASTNode::Identifier(format!("{}::{}", enum_name, variant_err), *span)
                } else {
                    // Option::None (no payload)
                    ASTNode::Identifier(format!("{}::{}", enum_name, variant_err), *span)
                };

                let error_body = vec![ASTNode::ReturnStatement(Some(Box::new(error_value)), *span)];

                let error_arm = MatchArm {
                    pattern: MatchPattern::Binding {
                        by_ref: false,
                        enum_name: enum_name.to_string(),
                        variant: variant_err.to_string(),
                        bindings: if enum_name == "Option" {
                            vec![]
                        } else {
                            vec![err_tmp.clone()]
                        },
                        span: *span,
                    },
                    body: error_body,
                    span: *span,
                };

                let match_node = ASTNode::MatchExpr {
                    value: scrutinee,
                    arms: vec![success_arm, error_arm],
                    span: *span,
                };

                self.analyze_expression(&match_node, expected)
            }
            _ => {
                let span = node_span(node);
                emit_diagnostic(
                    &Diagnostic::error("Semantic verification crash: unhandled expression.")
                        .with_code("VX0244")
                        .with_span(span),
                );
                self.error_occurred = true;
                None
            }
        }
    }

    // -------------------------------------------------------------------------
    // Type helpers for primitive checks
    // -------------------------------------------------------------------------
    pub(crate) fn is_integer_type(ty: &str) -> bool {
        matches!(
            ty,
            "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "char"
        )
    }

    pub(crate) fn is_arithmetic_type(ty: &str) -> bool {
        Self::is_integer_type(ty) || matches!(ty, "f32" | "f64")
    }
}
