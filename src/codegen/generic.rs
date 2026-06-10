// generic.rs - Generic function monomorphisation and type substitution.
//
// Extracted from the original utils.rs.
// Contains all logic related to on‑the‑fly monomorphisation of generic functions,
// unification of generic parameters, and substitution of concrete types.

use crate::codegen::CodegenEngine;
use crate::codegen::type_map::parse_generic_type;
use crate::diagnostic::{Diagnostic, emit_diagnostic};
use crate::parser::{ASTNode, Param};
use std::collections::HashMap;

// ----------------------------------------------------------------------------
// Sanitize type names for use in LLVM identifiers (moved from utils.rs)
// ----------------------------------------------------------------------------
pub fn sanitize_type_name(s: &str) -> String {
    s.replace("&", "ref_")
        .replace("*", "ptr")
        .replace('<', "_LT_")
        .replace('>', "_GT_")
        .replace(' ', "")
}

impl CodegenEngine {
    /// Unify a generic parameterized type (e.g., `& Vec<T>`) with a concrete type (e.g., `& Vec<i32>`)
    /// and return the concrete type for the generic parameter `gp`.
    ///
    /// This version preserves references when the generic pattern is exactly the parameter name
    /// (i.e., `T`), so that calling `identity(&Option)` makes `T = &Option`, not just `Option`.
    pub(crate) fn unify_generic_parameter(
        &self,
        gp: &str,
        generic_ty: &str,
        concrete_ty: &str,
    ) -> Option<String> {
        // If the generic pattern is exactly the parameter name (no outer structure),
        // return the concrete type unchanged.
        if generic_ty == gp {
            return Some(concrete_ty.to_string());
        }

        // Otherwise, strip outermost references from both types.
        let stripped_generic = Self::strip_references(generic_ty);
        let stripped_concrete = Self::strip_references(concrete_ty);

        // If we removed a reference, recurse with the stripped types.
        if stripped_generic != generic_ty || stripped_concrete != concrete_ty {
            return self.unify_generic_parameter(gp, stripped_generic, stripped_concrete);
        }

        // Now both are either plain types or struct/enum types.
        // Try to parse as generic struct/enum, e.g., "Vec<T>" or "Option<T>".
        if let Some((base_gen, args_gen)) = parse_generic_type(stripped_generic) {
            if let Some((base_con, args_con)) = parse_generic_type(stripped_concrete) {
                if base_gen == base_con {
                    // Find the position of `gp` in the generic arguments
                    for (i, arg) in args_gen.iter().enumerate() {
                        if arg == gp {
                            return Some(args_con[i].clone());
                        }
                    }
                }
            }
        }

        None
    }

    /// Substitute types in an AST node (used for on‑the‑fly monomorphisation).
    /// This is a simplified version of the one in semantics.rs, sufficient for our needs.
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
                        ty: Self::substitute_type_string(&p.ty, subst),
                        refinement: p.refinement.clone(),
                        span: p.span,
                    })
                    .collect();
                let new_return_type = Self::substitute_type_string(return_type, subst);
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
            VariableDecl {
                name,
                ty,
                refinement,
                value,
                mutable,
                span,
            } => {
                let new_ty = ty.as_ref().map(|t| Self::substitute_type_string(t, subst));
                VariableDecl {
                    name: name.clone(),
                    ty: new_ty,
                    refinement: refinement.clone(),
                    value: Box::new(Self::substitute_types_in_node(value, subst)),
                    mutable: *mutable,
                    span: *span,
                }
            }
            ReturnStatement(expr, span) => ReturnStatement(
                expr.as_ref()
                    .map(|e| Box::new(Self::substitute_types_in_node(e, subst))),
                *span,
            ),
            FieldAccess { expr, field, span } => FieldAccess {
                expr: Box::new(Self::substitute_types_in_node(expr, subst)),
                field: field.clone(),
                span: *span,
            },
            CallExpr { callee, args, span } => {
                let new_args = args
                    .iter()
                    .map(|a| Self::substitute_types_in_node(a, subst))
                    .collect();
                let new_callee = Self::substitute_type_string(callee, subst);
                CallExpr {
                    callee: new_callee,
                    args: new_args,
                    span: *span,
                }
            }
            StructLiteral { name, fields, span } => {
                let new_fields = fields
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
            IntegerLiteral(v, s) => IntegerLiteral(*v, *s),
            StringLiteral(st, s) => StringLiteral(st.clone(), *s),
            // Default: just clone (should not contain generic parameters)
            _ => node.clone(),
        }
    }

    /// Generate a monomorphised version of a generic function on the fly.
    pub(crate) fn generate_monomorphised_function(
        &mut self,
        generic_name: &str,
        monomorphised_name: &str,
        subst: &HashMap<String, String>,
    ) {
        self.debug_log(&format!(
            "generate_monomorphised_function: generic='{}', mono='{}', subst={:?}",
            generic_name, monomorphised_name, subst
        ));
        if let Some(original_ast) = self.generic_function_asts.get(generic_name) {
            let substituted = Self::substitute_types_in_node(original_ast, subst);

            if let ASTNode::FunctionDef {
                name: _,
                generic_params: _,
                params,
                return_type,
                return_refinement,
                body,
                span,
            } = substituted
            {
                let monomorphised_func = ASTNode::FunctionDef {
                    name: monomorphised_name.to_string(),
                    generic_params: vec![],
                    params,
                    return_type: return_type.clone(),
                    return_refinement,
                    body,
                    span,
                };
                self.debug_log(&format!(
                    "Inserting return type for mono function '{}' = '{}'",
                    monomorphised_name, return_type
                ));
                self.function_return_types
                    .insert(monomorphised_name.to_string(), return_type.clone());
                self.pending_monomorphised_functions
                    .push(monomorphised_func.clone());
                self.debug_log(&format!(
                    "on‑the‑fly generated monomorphised function '{}' from '{}'",
                    monomorphised_name, generic_name
                ));
            } else {
                emit_diagnostic(
                    &Diagnostic::error(&format!(
                        "Internal error: generic function '{}' AST is not a FunctionDef",
                        generic_name
                    ))
                    .with_code("VX9008"),
                );
                self.has_error = true;
            }
        } else {
            emit_diagnostic(
                &Diagnostic::error(&format!(
                    "Internal error: generic function '{}' not found in AST map",
                    generic_name
                ))
                .with_code("VX9007"),
            );
            self.has_error = true;
        }
    }

    /// Compute the concrete return type of a generic function call by unifying
    /// generic parameters with argument types (and optionally an expected return type).
    pub(crate) fn get_concrete_return_type_for_generic_call(
        &self,
        callee: &str,
        args: &[ASTNode],
        expected_ret: Option<&str>,
    ) -> Option<String> {
        self.debug_log(&format!(
            "get_concrete_return_type_for_generic_call: callee='{}', expected_ret={:?}",
            callee, expected_ret
        ));
        let (generic_params, param_tys, return_ty) =
            match self.generic_functions.get(callee).cloned() {
                Some(info) => info,
                None => {
                    self.debug_log(&format!("  callee '{}' not a generic function", callee));
                    return None;
                }
            };

        let mut subst = HashMap::new();

        // Infer from arguments
        for (i, param_ty) in param_tys.iter().enumerate() {
            if i >= args.len() {
                break;
            }
            let arg_ty = self.infer_vox_type(&args[i]);
            self.debug_log(&format!(
                "  param[{}] type='{}', arg type='{}'",
                i, param_ty, arg_ty
            ));
            for gp in &generic_params {
                if param_ty.contains(gp) {
                    if let Some(concrete) = self.unify_generic_parameter(gp, param_ty, &arg_ty) {
                        self.debug_log(&format!("    unified {} = {}", gp, concrete));
                        subst.insert(gp.clone(), concrete);
                    } else {
                        self.debug_log(&format!("    fallback {} = {}", gp, arg_ty));
                        subst.insert(gp.clone(), arg_ty.clone());
                    }
                }
            }
        }

        // Infer from expected return type if available
        if let Some(expected) = expected_ret {
            let base_ret = Self::strip_generic_args(&return_ty);
            let base_exp = Self::strip_generic_args(expected);
            if base_ret == base_exp && return_ty.contains('<') && expected.contains('<') {
                if let (Some((_, ret_args)), Some((_, exp_args))) =
                    (parse_generic_type(&return_ty), parse_generic_type(expected))
                {
                    for (i, rarg) in ret_args.iter().enumerate() {
                        if i < exp_args.len() && generic_params.contains(rarg) {
                            self.debug_log(&format!(
                                "  from expected return: {} = {}",
                                rarg, exp_args[i]
                            ));
                            subst.insert(rarg.clone(), exp_args[i].clone());
                        }
                    }
                }
            }
        }

        let all_resolved = generic_params.iter().all(|gp| subst.contains_key(gp));
        if all_resolved {
            let concrete_ret = Self::substitute_type_string(&return_ty, &subst);
            self.debug_log(&format!(
                "Generic call '{}': returning concrete type '{}'",
                callee, concrete_ret
            ));
            Some(concrete_ret)
        } else {
            self.debug_log("  not all generic parameters resolved");
            None
        }
    }
}
