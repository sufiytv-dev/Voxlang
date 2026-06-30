// import.rs - Recursive AST transformation for `import` statements.
// Replaces import nodes with the content of imported modules.

use crate::diagnostic::debug_log;
use crate::frontend::span::Span;
use crate::module::ModuleResolver;
use crate::parser::{ASTNode, MatchArm};

/// Recursively replace import nodes with the content of the imported modules.
/// Returns the transformed AST and a boolean indicating whether any error occurred.
pub fn resolve_imports(node: &ASTNode, resolver: &mut ModuleResolver) -> (ASTNode, bool) {
    match node {
        ASTNode::Program(stmts, span) => {
            let mut new_stmts = Vec::new();
            let mut errors = false;
            for stmt in stmts {
                match stmt {
                    ASTNode::Import { .. } => {
                        debug_log(format!("[IMPORT] Resolving import: {:?}", stmt));
                        if let Some(imported_ast) = resolver.resolve_import(stmt, node_span(stmt)) {
                            let (resolved_import, import_err) =
                                resolve_imports(&imported_ast, resolver);
                            if import_err {
                                errors = true;
                            }
                            if let ASTNode::Program(imported_stmts, _) = resolved_import {
                                new_stmts.extend(imported_stmts);
                            } else {
                                new_stmts.push(resolved_import);
                            }
                        } else {
                            debug_log("[IMPORT] Failed to resolve import");
                            errors = true;
                        }
                    }
                    _ => {
                        let (transformed, err) = resolve_imports(stmt, resolver);
                        if err {
                            errors = true;
                        }
                        new_stmts.push(transformed);
                    }
                }
            }
            (ASTNode::Program(new_stmts, *span), errors)
        }

        ASTNode::Import { span, .. } => {
            debug_log("[IMPORT] Resolving top-level import");
            if let Some(imported_ast) = resolver.resolve_import(node, *span) {
                resolve_imports(&imported_ast, resolver)
            } else {
                (ASTNode::Error, true)
            }
        }

        // These types are not transformed; we just keep them as-is.
        ASTNode::StructDef { .. } => (node.clone(), false),
        ASTNode::EnumDef { .. } => (node.clone(), false),
        ASTNode::TypeAlias { .. } => (node.clone(), false),
        ASTNode::UseDecl { .. } => (node.clone(), false),

        ASTNode::MatchExpr { value, arms, span } => {
            let (new_value, err1) = resolve_imports(value, resolver);
            let mut new_arms = Vec::new();
            let mut err2 = false;
            for arm in arms {
                let mut new_body = Vec::new();
                for stmt in &arm.body {
                    let (new_stmt, e) = resolve_imports(stmt, resolver);
                    if e {
                        err2 = true;
                    }
                    new_body.push(new_stmt);
                }
                new_arms.push(MatchArm {
                    pattern: arm.pattern.clone(),
                    body: new_body,
                    span: arm.span,
                });
            }
            (
                ASTNode::MatchExpr {
                    value: Box::new(new_value),
                    arms: new_arms,
                    span: *span,
                },
                err1 || err2,
            )
        }

        ASTNode::FieldAccess { expr, field, span } => {
            let (new_expr, err) = resolve_imports(expr, resolver);
            (
                ASTNode::FieldAccess {
                    expr: Box::new(new_expr),
                    field: field.clone(),
                    span: *span,
                },
                err,
            )
        }

        ASTNode::ArrayLiteral { elements, span } => {
            let mut new_elems = Vec::new();
            let mut errors = false;
            for elem in elements {
                let (new_elem, err) = resolve_imports(elem, resolver);
                if err {
                    errors = true;
                }
                new_elems.push(new_elem);
            }
            (
                ASTNode::ArrayLiteral {
                    elements: new_elems,
                    span: *span,
                },
                errors,
            )
        }

        ASTNode::ArrayIndex { array, index, span } => {
            let (new_array, err1) = resolve_imports(array, resolver);
            let (new_index, err2) = resolve_imports(index, resolver);
            (
                ASTNode::ArrayIndex {
                    array: Box::new(new_array),
                    index: Box::new(new_index),
                    span: *span,
                },
                err1 || err2,
            )
        }

        ASTNode::SliceExpr {
            base,
            start,
            end,
            span,
        } => {
            let (new_base, err1) = resolve_imports(base, resolver);
            let (new_start, err2) = match start {
                Some(s) => {
                    let (transformed, err) = resolve_imports(s, resolver);
                    (Some(Box::new(transformed)), err)
                }
                None => (None, false),
            };
            let (new_end, err3) = match end {
                Some(e) => {
                    let (transformed, err) = resolve_imports(e, resolver);
                    (Some(Box::new(transformed)), err)
                }
                None => (None, false),
            };
            (
                ASTNode::SliceExpr {
                    base: Box::new(new_base),
                    start: new_start,
                    end: new_end,
                    span: *span,
                },
                err1 || err2 || err3,
            )
        }

        ASTNode::UnaryExpr { op, expr, span } => {
            let (new_expr, err) = resolve_imports(expr, resolver);
            (
                ASTNode::UnaryExpr {
                    op: op.clone(),
                    expr: Box::new(new_expr),
                    span: *span,
                },
                err,
            )
        }

        ASTNode::FloatLiteral(_, _) | ASTNode::CharLiteral(_, _) => (node.clone(), false),

        ASTNode::StructLiteral { name, fields, span } => {
            let mut new_fields = Vec::new();
            let mut errors = false;
            for (field_name, expr) in fields {
                let (new_expr, err) = resolve_imports(expr, resolver);
                if err {
                    errors = true;
                }
                new_fields.push((field_name.clone(), new_expr));
            }
            (
                ASTNode::StructLiteral {
                    name: name.clone(),
                    fields: new_fields,
                    span: *span,
                },
                errors,
            )
        }

        ASTNode::IfLetStatement {
            pattern,
            expr,
            then_branch,
            else_branch,
            span,
        } => {
            let (new_expr, err1) = resolve_imports(expr, resolver);
            let mut then_new = Vec::new();
            let mut err2 = false;
            for stmt in then_branch {
                let (transformed, e) = resolve_imports(stmt, resolver);
                if e {
                    err2 = true;
                }
                then_new.push(transformed);
            }
            let mut else_new = None;
            let mut err3 = false;
            if let Some(branch) = else_branch {
                let mut vec = Vec::new();
                for stmt in branch {
                    let (transformed, e) = resolve_imports(stmt, resolver);
                    if e {
                        err3 = true;
                    }
                    vec.push(transformed);
                }
                else_new = Some(vec);
            }
            (
                ASTNode::IfLetStatement {
                    pattern: pattern.clone(),
                    expr: Box::new(new_expr),
                    then_branch: then_new,
                    else_branch: else_new,
                    span: *span,
                },
                err1 || err2 || err3,
            )
        }

        ASTNode::WhileLetStatement {
            pattern,
            expr,
            body,
            span,
        } => {
            let (new_expr, err1) = resolve_imports(expr, resolver);
            let mut new_body = Vec::new();
            let mut err2 = false;
            for stmt in body {
                let (transformed, e) = resolve_imports(stmt, resolver);
                if e {
                    err2 = true;
                }
                new_body.push(transformed);
            }
            (
                ASTNode::WhileLetStatement {
                    pattern: pattern.clone(),
                    expr: Box::new(new_expr),
                    body: new_body,
                    span: *span,
                },
                err1 || err2,
            )
        }

        ASTNode::TryExpr { expr, span } => {
            let (new_expr, err) = resolve_imports(expr, resolver);
            (
                ASTNode::TryExpr {
                    expr: Box::new(new_expr),
                    span: *span,
                },
                err,
            )
        }

        ASTNode::ForLoop {
            iter_var,
            start,
            end,
            body,
            span,
        } => {
            let (new_start, err1) = resolve_imports(start, resolver);
            let (new_end, err2) = resolve_imports(end, resolver);
            let mut new_body = Vec::new();
            let mut err3 = false;
            for stmt in body {
                let (transformed, e) = resolve_imports(stmt, resolver);
                if e {
                    err3 = true;
                }
                new_body.push(transformed);
            }
            (
                ASTNode::ForLoop {
                    iter_var: iter_var.clone(),
                    start: Box::new(new_start),
                    end: Box::new(new_end),
                    body: new_body,
                    span: *span,
                },
                err1 || err2 || err3,
            )
        }

        ASTNode::Block { statements, span } => {
            let mut new_stmts = Vec::new();
            let mut errors = false;
            for stmt in statements {
                let (transformed, err) = resolve_imports(stmt, resolver);
                if err {
                    errors = true;
                }
                new_stmts.push(transformed);
            }
            (
                ASTNode::Block {
                    statements: new_stmts,
                    span: *span,
                },
                errors,
            )
        }

        ASTNode::FunctionDef {
            name,
            params,
            return_type,
            return_refinement,
            body,
            span,
            generic_params,
        } => {
            let mut new_body = Vec::new();
            let mut errors = false;
            for stmt in body {
                let (transformed, err) = resolve_imports(stmt, resolver);
                if err {
                    errors = true;
                }
                new_body.push(transformed);
            }
            (
                ASTNode::FunctionDef {
                    name: name.clone(),
                    params: params.clone(),
                    return_type: return_type.clone(),
                    return_refinement: return_refinement.clone(),
                    body: new_body,
                    span: *span,
                    generic_params: generic_params.clone(),
                },
                errors,
            )
        }

        ASTNode::KernelFn {
            name,
            params,
            body,
            device_triple,
            attr,
            span,
        } => {
            let mut new_body = Vec::new();
            let mut errors = false;
            for stmt in body {
                let (transformed, err) = resolve_imports(stmt, resolver);
                if err {
                    errors = true;
                }
                new_body.push(transformed);
            }
            (
                ASTNode::KernelFn {
                    name: name.clone(),
                    params: params.clone(),
                    body: new_body,
                    device_triple: device_triple.clone(),
                    attr: attr.clone(),
                    span: *span,
                },
                errors,
            )
        }

        ASTNode::KernelLaunch {
            kernel,
            grid,
            args,
            span,
        } => {
            let (new_kernel, err1) = resolve_imports(kernel, resolver);
            let (new_grid_x, err2) = resolve_imports(&*grid.0, resolver);
            let (new_grid_y, err3) = resolve_imports(&*grid.1, resolver);
            let (new_grid_z, err4) = resolve_imports(&*grid.2, resolver);
            let mut new_args = Vec::new();
            let mut err5 = false;
            for arg in args {
                let (new_arg, e) = resolve_imports(arg, resolver);
                if e {
                    err5 = true;
                }
                new_args.push(new_arg);
            }
            (
                ASTNode::KernelLaunch {
                    kernel: Box::new(new_kernel),
                    grid: (
                        Box::new(new_grid_x),
                        Box::new(new_grid_y),
                        Box::new(new_grid_z),
                    ),
                    args: new_args,
                    span: *span,
                },
                err1 || err2 || err3 || err4 || err5,
            )
        }

        ASTNode::IfStatement {
            condition,
            then_branch,
            else_branch,
            span,
        } => {
            let (new_cond, err1) = resolve_imports(condition, resolver);
            let mut then_new = Vec::new();
            let mut err2 = false;
            for stmt in then_branch {
                let (transformed, e) = resolve_imports(stmt, resolver);
                if e {
                    err2 = true;
                }
                then_new.push(transformed);
            }
            let mut else_new = None;
            let mut err3 = false;
            if let Some(branch) = else_branch {
                let mut vec = Vec::new();
                for stmt in branch {
                    let (transformed, e) = resolve_imports(stmt, resolver);
                    if e {
                        err3 = true;
                    }
                    vec.push(transformed);
                }
                else_new = Some(vec);
            }
            (
                ASTNode::IfStatement {
                    condition: Box::new(new_cond),
                    then_branch: then_new,
                    else_branch: else_new,
                    span: *span,
                },
                err1 || err2 || err3,
            )
        }

        ASTNode::WhileStatement {
            condition,
            body,
            span,
        } => {
            let (new_cond, err1) = resolve_imports(condition, resolver);
            let mut new_body = Vec::new();
            let mut err2 = false;
            for stmt in body {
                let (transformed, e) = resolve_imports(stmt, resolver);
                if e {
                    err2 = true;
                }
                new_body.push(transformed);
            }
            (
                ASTNode::WhileStatement {
                    condition: Box::new(new_cond),
                    body: new_body,
                    span: *span,
                },
                err1 || err2,
            )
        }

        ASTNode::ParallelLoop {
            iter_var,
            start,
            end,
            body,
            span,
        } => {
            let (new_start, err1) = resolve_imports(start, resolver);
            let (new_end, err2) = resolve_imports(end, resolver);
            let mut new_body = Vec::new();
            let mut err3 = false;
            for stmt in body {
                let (transformed, e) = resolve_imports(stmt, resolver);
                if e {
                    err3 = true;
                }
                new_body.push(transformed);
            }
            (
                ASTNode::ParallelLoop {
                    iter_var: iter_var.clone(),
                    start: Box::new(new_start),
                    end: Box::new(new_end),
                    body: new_body,
                    span: *span,
                },
                err1 || err2 || err3,
            )
        }

        ASTNode::ComptimeBlock { body, span } => {
            let mut new_body = Vec::new();
            let mut errors = false;
            for stmt in body {
                let (transformed, e) = resolve_imports(stmt, resolver);
                if e {
                    errors = true;
                }
                new_body.push(transformed);
            }
            (
                ASTNode::ComptimeBlock {
                    body: new_body,
                    span: *span,
                },
                errors,
            )
        }

        ASTNode::VariableDecl {
            name,
            ty,
            refinement,
            value,
            mutable,
            span,
        } => {
            let (new_value, err) = resolve_imports(value, resolver);
            (
                ASTNode::VariableDecl {
                    name: name.clone(),
                    ty: ty.clone(),
                    refinement: refinement.clone(),
                    value: Box::new(new_value),
                    mutable: *mutable,
                    span: *span,
                },
                err,
            )
        }

        ASTNode::DeviceVarDecl {
            name,
            ty,
            refinement,
            value,
            span,
        } => {
            let (new_value, err) = resolve_imports(value, resolver);
            (
                ASTNode::DeviceVarDecl {
                    name: name.clone(),
                    ty: ty.clone(),
                    refinement: refinement.clone(),
                    value: Box::new(new_value),
                    span: *span,
                },
                err,
            )
        }

        ASTNode::Assignment { lhs, value, span } => {
            let (new_lhs, err1) = resolve_imports(lhs, resolver);
            let (new_value, err2) = resolve_imports(value, resolver);
            (
                ASTNode::Assignment {
                    lhs: Box::new(new_lhs),
                    value: Box::new(new_value),
                    span: *span,
                },
                err1 || err2,
            )
        }

        ASTNode::ReturnStatement(expr_opt, span) => {
            if let Some(expr) = expr_opt {
                let (new_expr, err) = resolve_imports(expr, resolver);
                (
                    ASTNode::ReturnStatement(Some(Box::new(new_expr)), *span),
                    err,
                )
            } else {
                (node.clone(), false)
            }
        }

        ASTNode::BinaryExpr {
            left,
            op,
            right,
            span,
        } => {
            let (new_left, err1) = resolve_imports(left, resolver);
            let (new_right, err2) = resolve_imports(right, resolver);
            (
                ASTNode::BinaryExpr {
                    left: Box::new(new_left),
                    op: op.clone(),
                    right: Box::new(new_right),
                    span: *span,
                },
                err1 || err2,
            )
        }

        ASTNode::CastExpr {
            expr,
            target_type,
            span,
        } => {
            let (new_expr, err) = resolve_imports(expr, resolver);
            (
                ASTNode::CastExpr {
                    expr: Box::new(new_expr),
                    target_type: target_type.clone(),
                    span: *span,
                },
                err,
            )
        }

        ASTNode::CallExpr { callee, args, span } => {
            let mut new_args = Vec::new();
            let mut errors = false;
            for arg in args {
                let (new_arg, err) = resolve_imports(arg, resolver);
                if err {
                    errors = true;
                }
                new_args.push(new_arg);
            }
            (
                ASTNode::CallExpr {
                    callee: callee.clone(),
                    args: new_args,
                    span: *span,
                },
                errors,
            )
        }

        ASTNode::BorrowExpr {
            mutable,
            expr,
            span,
        } => {
            let (new_expr, err) = resolve_imports(expr, resolver);
            (
                ASTNode::BorrowExpr {
                    mutable: *mutable,
                    expr: Box::new(new_expr),
                    span: *span,
                },
                err,
            )
        }

        ASTNode::DerefExpr(inner, span) => {
            let (new_inner, err) = resolve_imports(inner, resolver);
            (ASTNode::DerefExpr(Box::new(new_inner), *span), err)
        }

        ASTNode::RefinedType {
            base,
            condition,
            span,
        } => {
            let (new_base, err1) = resolve_imports(base, resolver);
            let (new_cond, err2) = resolve_imports(condition, resolver);
            (
                ASTNode::RefinedType {
                    base: Box::new(new_base),
                    condition: Box::new(new_cond),
                    span: *span,
                },
                err1 || err2,
            )
        }

        ASTNode::Lemma {
            name,
            params,
            return_type,
            proof,
            span,
        } => {
            let mut new_proof = Vec::new();
            let mut errors = false;
            for stmt in proof {
                let (transformed, e) = resolve_imports(stmt, resolver);
                if e {
                    errors = true;
                }
                new_proof.push(transformed);
            }
            (
                ASTNode::Lemma {
                    name: name.clone(),
                    params: params.clone(),
                    return_type: return_type.clone(),
                    proof: new_proof,
                    span: *span,
                },
                errors,
            )
        }

        ASTNode::Identifier(..)
        | ASTNode::IntegerLiteral(..)
        | ASTNode::StringLiteral(..)
        | ASTNode::Error => (node.clone(), false),
    }
}

/// Helper to extract span from any AST node.
pub fn node_span(node: &ASTNode) -> Span {
    match node {
        ASTNode::Program(_, span) => *span,
        ASTNode::Import { span, .. } => *span,
        ASTNode::StructDef { span, .. } => *span,
        ASTNode::EnumDef { span, .. } => *span,
        ASTNode::TypeAlias { span, .. } => *span,
        ASTNode::UseDecl { span, .. } => *span,
        ASTNode::Block { span, .. } => *span,
        ASTNode::FunctionDef { span, .. } => *span,
        ASTNode::KernelFn { span, .. } => *span,
        ASTNode::KernelLaunch { span, .. } => *span,
        ASTNode::IfStatement { span, .. } => *span,
        ASTNode::IfLetStatement { span, .. } => *span,
        ASTNode::WhileStatement { span, .. } => *span,
        ASTNode::WhileLetStatement { span, .. } => *span,
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
        ASTNode::TryExpr { span, .. } => *span,
        ASTNode::Identifier(_, span) => *span,
        ASTNode::IntegerLiteral(_, span) => *span,
        ASTNode::FloatLiteral(_, span) => *span,
        ASTNode::CharLiteral(_, span) => *span,
        ASTNode::StringLiteral(_, span) => *span,
        ASTNode::RefinedType { span, .. } => *span,
        ASTNode::Lemma { span, .. } => *span,
        ASTNode::Error => Span::new(0, 0, 0, 0),
    }
}
