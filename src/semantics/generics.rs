// semantics/generics.rs
//! Generic parameter handling, type substitution, and monomorphisation helpers.
//!
//! This module contains functions for substituting generic parameters in types
//! and AST nodes, as well as a helper for unifying generic parameters.

use crate::parser::{ASTNode, MatchArm, Param};
use crate::semantics::SemanticAnalyzer;
use crate::semantics::types::Type;
use std::collections::HashMap;

impl SemanticAnalyzer<'_> {
    // -------------------------------------------------------------------------
    // Type substitution for monomorphisation (kept for future use)
    // -------------------------------------------------------------------------

    /// Substitute generic parameters in a type string (used by monomorphisation).
    #[allow(dead_code)]
    pub(crate) fn substitute_type_in_string(ty: &str, subst: &HashMap<String, String>) -> String {
        let mut result = ty.to_string();
        for (gp, conc) in subst {
            result = result.replace(gp, conc);
        }
        result
    }

    /// Recursively substitute generic parameters in an AST node.
    /// This is used during monomorphisation to instantiate generic functions.
    #[allow(dead_code)]
    pub(crate) fn substitute_types_in_node(
        node: &ASTNode,
        subst: &HashMap<String, String>,
    ) -> ASTNode {
        use ASTNode::*;
        match node {
            Identifier(name, span) => {
                if subst.contains_key(name) {
                    Identifier(subst.get(name).unwrap().clone(), *span)
                } else {
                    Identifier(name.clone(), *span)
                }
            }
            VariableDecl {
                name,
                ty,
                refinement,
                value,
                mutable,
                span,
            } => {
                let new_ty = ty
                    .as_ref()
                    .map(|t| Self::substitute_type_in_string(t, subst));
                VariableDecl {
                    name: name.clone(),
                    ty: new_ty,
                    refinement: refinement.clone(),
                    value: Box::new(Self::substitute_types_in_node(value, subst)),
                    mutable: *mutable,
                    span: *span,
                }
            }
            FunctionDef {
                name,
                generic_params: _,
                params,
                return_type,
                return_refinement,
                body,
                span,
            } => {
                let new_params = params
                    .iter()
                    .map(|p| Param {
                        name: p.name.clone(),
                        ty: Self::substitute_type_in_string(&p.ty, subst),
                        refinement: p.refinement.clone(),
                        span: p.span,
                    })
                    .collect();
                let new_return_type = Self::substitute_type_in_string(return_type, subst);
                let new_body = body
                    .iter()
                    .map(|stmt| Self::substitute_types_in_node(stmt, subst))
                    .collect();
                FunctionDef {
                    name: name.clone(),
                    generic_params: vec![], // monomorphised – no generics left
                    params: new_params,
                    return_type: new_return_type,
                    return_refinement: return_refinement.clone(),
                    body: new_body,
                    span: *span,
                }
            }
            CastExpr {
                expr,
                target_type,
                span,
            } => {
                let new_target = Self::substitute_type_in_string(target_type, subst);
                CastExpr {
                    expr: Box::new(Self::substitute_types_in_node(expr, subst)),
                    target_type: new_target,
                    span: *span,
                }
            }
            RefinedType {
                base,
                condition,
                span,
            } => RefinedType {
                base: Box::new(Self::substitute_types_in_node(base, subst)),
                condition: Box::new(Self::substitute_types_in_node(condition, subst)),
                span: *span,
            },
            StructLiteral { name, fields, span } => {
                let new_fields: Vec<(String, ASTNode)> = fields
                    .iter()
                    .map(|(fname, expr)| {
                        (fname.clone(), Self::substitute_types_in_node(expr, subst))
                    })
                    .collect();
                StructLiteral {
                    name: name.clone(),
                    fields: new_fields,
                    span: *span,
                }
            }
            ArrayLiteral { elements, span } => {
                let new_elems = elements
                    .iter()
                    .map(|e| Self::substitute_types_in_node(e, subst))
                    .collect();
                ArrayLiteral {
                    elements: new_elems,
                    span: *span,
                }
            }
            BinaryExpr {
                left,
                op,
                right,
                span,
            } => BinaryExpr {
                left: Box::new(Self::substitute_types_in_node(left, subst)),
                op: op.clone(),
                right: Box::new(Self::substitute_types_in_node(right, subst)),
                span: *span,
            },
            UnaryExpr { op, expr, span } => UnaryExpr {
                op: op.clone(),
                expr: Box::new(Self::substitute_types_in_node(expr, subst)),
                span: *span,
            },
            CallExpr { callee, args, span } => {
                let new_args = args
                    .iter()
                    .map(|a| Self::substitute_types_in_node(a, subst))
                    .collect();
                CallExpr {
                    callee: callee.clone(),
                    args: new_args,
                    span: *span,
                }
            }
            IfStatement {
                condition,
                then_branch,
                else_branch,
                span,
            } => IfStatement {
                condition: Box::new(Self::substitute_types_in_node(condition, subst)),
                then_branch: then_branch
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                else_branch: else_branch.as_ref().map(|b| {
                    b.iter()
                        .map(|s| Self::substitute_types_in_node(s, subst))
                        .collect()
                }),
                span: *span,
            },
            WhileStatement {
                condition,
                body,
                span,
            } => WhileStatement {
                condition: Box::new(Self::substitute_types_in_node(condition, subst)),
                body: body
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                span: *span,
            },
            ReturnStatement(expr, span) => ReturnStatement(
                expr.as_ref()
                    .map(|e| Box::new(Self::substitute_types_in_node(e, subst))),
                *span,
            ),
            Assignment { lhs, value, span } => Assignment {
                lhs: Box::new(Self::substitute_types_in_node(lhs, subst)),
                value: Box::new(Self::substitute_types_in_node(value, subst)),
                span: *span,
            },
            MatchExpr { value, arms, span } => {
                let new_arms = arms
                    .iter()
                    .map(|arm| {
                        let new_body = arm
                            .body
                            .iter()
                            .map(|s| Self::substitute_types_in_node(s, subst))
                            .collect();
                        MatchArm {
                            pattern: arm.pattern.clone(),
                            body: new_body,
                            span: arm.span,
                        }
                    })
                    .collect();
                MatchExpr {
                    value: Box::new(Self::substitute_types_in_node(value, subst)),
                    arms: new_arms,
                    span: *span,
                }
            }
            Program(stmts, span) => Program(
                stmts
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                *span,
            ),
            Import {
                source,
                alias,
                hash,
                span,
            } => Import {
                source: source.clone(),
                alias: alias.clone(),
                hash: hash.clone(),
                span: *span,
            },
            StructDef {
                name,
                generic_params,
                fields,
                span,
            } => StructDef {
                name: name.clone(),
                generic_params: generic_params.clone(),
                fields: fields.clone(),
                span: *span,
            },
            EnumDef {
                name,
                params,
                variants,
                span,
            } => EnumDef {
                name: name.clone(),
                params: params.clone(),
                variants: variants.clone(),
                span: *span,
            },
            TypeAlias {
                name,
                target_type,
                span,
            } => TypeAlias {
                name: name.clone(),
                target_type: target_type.clone(),
                span: *span,
            },
            UseDecl {
                path,
                alias,
                is_glob,
                span,
            } => UseDecl {
                path: path.clone(),
                alias: alias.clone(),
                is_glob: *is_glob,
                span: *span,
            },
            KernelFn {
                name,
                params,
                body,
                device_triple,
                span,
            } => {
                let new_params = params
                    .iter()
                    .map(|p| Param {
                        name: p.name.clone(),
                        ty: Self::substitute_type_in_string(&p.ty, subst),
                        refinement: p.refinement.clone(),
                        span: p.span,
                    })
                    .collect();
                KernelFn {
                    name: name.clone(),
                    params: new_params,
                    body: body
                        .iter()
                        .map(|s| Self::substitute_types_in_node(s, subst))
                        .collect(),
                    device_triple: device_triple.clone(),
                    span: *span,
                }
            }
            DeviceVarDecl {
                name,
                ty,
                refinement,
                value,
                span,
            } => DeviceVarDecl {
                name: name.clone(),
                ty: ty.clone(),
                refinement: refinement.clone(),
                value: Box::new(Self::substitute_types_in_node(value, subst)),
                span: *span,
            },
            BorrowExpr {
                mutable,
                expr,
                span,
            } => BorrowExpr {
                mutable: *mutable,
                expr: Box::new(Self::substitute_types_in_node(expr, subst)),
                span: *span,
            },
            DerefExpr(inner, span) => DerefExpr(
                Box::new(Self::substitute_types_in_node(inner, subst)),
                *span,
            ),
            FieldAccess { expr, field, span } => FieldAccess {
                expr: Box::new(Self::substitute_types_in_node(expr, subst)),
                field: field.clone(),
                span: *span,
            },
            ArrayIndex { array, index, span } => ArrayIndex {
                array: Box::new(Self::substitute_types_in_node(array, subst)),
                index: Box::new(Self::substitute_types_in_node(index, subst)),
                span: *span,
            },
            SliceExpr {
                base,
                start,
                end,
                span,
            } => SliceExpr {
                base: Box::new(Self::substitute_types_in_node(base, subst)),
                start: start
                    .as_ref()
                    .map(|s| Box::new(Self::substitute_types_in_node(s, subst))),
                end: end
                    .as_ref()
                    .map(|e| Box::new(Self::substitute_types_in_node(e, subst))),
                span: *span,
            },
            ParallelLoop {
                iter_var,
                start,
                end,
                body,
                span,
            } => ParallelLoop {
                iter_var: iter_var.clone(),
                start: Box::new(Self::substitute_types_in_node(start, subst)),
                end: Box::new(Self::substitute_types_in_node(end, subst)),
                body: body
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                span: *span,
            },
            ForLoop {
                iter_var,
                start,
                end,
                body,
                span,
            } => ForLoop {
                iter_var: iter_var.clone(),
                start: Box::new(Self::substitute_types_in_node(start, subst)),
                end: Box::new(Self::substitute_types_in_node(end, subst)),
                body: body
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                span: *span,
            },
            ComptimeBlock { body, span } => ComptimeBlock {
                body: body
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                span: *span,
            },
            Lemma {
                name,
                params,
                return_type,
                proof,
                span,
            } => {
                let new_params = params
                    .iter()
                    .map(|p| Param {
                        name: p.name.clone(),
                        ty: Self::substitute_type_in_string(&p.ty, subst),
                        refinement: p.refinement.clone(),
                        span: p.span,
                    })
                    .collect();
                Lemma {
                    name: name.clone(),
                    params: new_params,
                    return_type: return_type.clone(),
                    proof: proof
                        .iter()
                        .map(|s| Self::substitute_types_in_node(s, subst))
                        .collect(),
                    span: *span,
                }
            }
            IntegerLiteral(v, s) => IntegerLiteral(*v, *s),
            FloatLiteral(v, s) => FloatLiteral(*v, *s),
            CharLiteral(c, s) => CharLiteral(*c, *s),
            StringLiteral(st, s) => StringLiteral(st.clone(), *s),
            Block { statements, span } => Block {
                statements: statements
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                span: *span,
            },
            // These nodes are desugared away and should never appear here,
            // but we include them for exhaustiveness.
            IfLetStatement {
                pattern,
                expr,
                then_branch,
                else_branch,
                span,
            } => IfLetStatement {
                pattern: pattern.clone(),
                expr: Box::new(Self::substitute_types_in_node(expr, subst)),
                then_branch: then_branch
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                else_branch: else_branch.as_ref().map(|b| {
                    b.iter()
                        .map(|s| Self::substitute_types_in_node(s, subst))
                        .collect()
                }),
                span: *span,
            },
            WhileLetStatement {
                pattern,
                expr,
                body,
                span,
            } => WhileLetStatement {
                pattern: pattern.clone(),
                expr: Box::new(Self::substitute_types_in_node(expr, subst)),
                body: body
                    .iter()
                    .map(|s| Self::substitute_types_in_node(s, subst))
                    .collect(),
                span: *span,
            },
            TryExpr { expr, span } => TryExpr {
                expr: Box::new(Self::substitute_types_in_node(expr, subst)),
                span: *span,
            },
            Error => Error,
        }
    }

    // -------------------------------------------------------------------------
    // Helper: unify a generic parameter inside a type pattern (e.g., &Vec<T> with &Vec<i32>)
    // -------------------------------------------------------------------------
    #[allow(dead_code)]
    pub(crate) fn unify_generic_parameter(
        &self,
        gp: &str,
        generic_ty: &Type,
        concrete_ty: &Type,
    ) -> Option<Type> {
        let stripped_generic = generic_ty.strip_references();
        let stripped_concrete = concrete_ty.strip_references();

        match (stripped_generic, stripped_concrete) {
            (Type::Struct(name1, args1), Type::Struct(name2, args2)) if name1 == name2 => {
                for (i, arg) in args1.iter().enumerate() {
                    if let Type::GenericParam(p) = arg {
                        if p == gp {
                            return Some(args2[i].clone());
                        }
                    }
                }
                None
            }
            (Type::Enum(name1, args1), Type::Enum(name2, args2)) if name1 == name2 => {
                for (i, arg) in args1.iter().enumerate() {
                    if let Type::GenericParam(p) = arg {
                        if p == gp {
                            return Some(args2[i].clone());
                        }
                    }
                }
                None
            }
            (Type::GenericParam(p), _) if p == gp => Some(stripped_concrete.clone()),
            _ => None,
        }
    }
}
