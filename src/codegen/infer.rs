// infer.rs - Type inference and constant folding for Vox expressions.
//
// Contains methods for inferring the Vox type of an AST node,
// resolving variable types from semantic analysis results,
// and constant folding for compile‑time evaluation.

use crate::codegen::CodegenEngine;
use crate::frontend::token::TokenKind;
use crate::parser::ASTNode;
use std::collections::HashMap;

impl CodegenEngine {
    /// Populate the resolved types map from the semantic analyzer's results.
    /// This should be called after semantic analysis and before code generation.
    pub fn set_resolved_types(&mut self, types: HashMap<String, String>) {
        self.resolved_types = types;
    }

    /// Get the resolved concrete type for a variable (if known) **without** qualification.
    /// This is legacy; new code should use `get_resolved_type_qualified`.
    pub fn get_resolved_type(&self, name: &str) -> Option<String> {
        self.resolved_types.get(name).cloned()
    }

    /// Get the resolved concrete type for a variable using a qualified key.
    /// The key is either "<function>::<var>" (if `func_name` is Some) or "global::<var>".
    /// This matches the keys stored by the semantic analyzer's `collect_resolved_types`.
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
        self.resolved_types.get(&key).cloned()
    }

    /// Infer the Vox type of an expression node.
    /// This is used during code generation when explicit type annotations are missing.
    pub(crate) fn infer_vox_type(&self, node: &ASTNode) -> String {
        use ASTNode::*;
        let result = match node {
            IntegerLiteral(..) => "i32".to_string(),
            FloatLiteral(..) => "f64".to_string(),
            CharLiteral(..) => "i32".to_string(),
            StringLiteral(..) => "&str".to_string(),
            Identifier(name, _) => {
                // First, check `var_vox_types` – this holds the types of match‑arm bindings,
                // which should shadow outer variables (e.g., `v` inside `Some(v) => v`).
                if let Some(vox_ty) = self.var_vox_types.get(name) {
                    vox_ty.clone()
                }
                // Otherwise, consult the resolved types from inference (qualified lookup),
                // which may refer to variables in the outer scope.
                else if let Some(ty) =
                    self.get_resolved_type_qualified(self.current_function_name.as_deref(), name)
                {
                    ty
                }
                // If still not found, try global variables (module‑scope).
                else if let Some((llvm_ty, _)) = self.global_variables.get(name) {
                    match llvm_ty.as_str() {
                        "double" => "f64".to_string(),
                        "float" => "f32".to_string(),
                        "{ i8*, i64 }" => "&str".to_string(),
                        "{ i8*, i64, i64 }" => "String".to_string(),
                        t if t.starts_with('%') => t[1..].to_string(),
                        t => t.to_string(),
                    }
                }
                // Qualified enum variant (e.g., `Option::Some`).
                else if let Some((enum_name, _)) = name.split_once("::") {
                    let base_enum = CodegenEngine::strip_generic_args(enum_name);
                    if self.enum_variants.contains_key(&base_enum) {
                        enum_name.to_string()
                    } else {
                        "i32".to_string()
                    }
                } else {
                    "i32".to_string()
                }
            }
            StructLiteral { name, .. } => name.clone(),
            CallExpr { callee, args, .. } => {
                // First, check registered function return types (for already monomorphised names)
                if let Some(ty) = self.function_return_types.get(callee) {
                    ty.clone()
                } else if self.generic_functions.contains_key(callee) {
                    if let Some(concrete_ty) =
                        self.get_concrete_return_type_for_generic_call(callee, args, None)
                    {
                        concrete_ty
                    } else {
                        "i32".to_string()
                    }
                } else if let Some((enum_name, variant_name)) = callee.split_once("::") {
                    let base_enum = CodegenEngine::strip_generic_args(enum_name);
                    if self.enum_variants.contains_key(&base_enum) {
                        if args.len() == 1 {
                            let arg_ty = self.infer_vox_type(&args[0]);
                            if base_enum == "Option" {
                                format!("{}<{}>", base_enum, arg_ty)
                            } else if base_enum == "Result" {
                                if variant_name == "Ok" {
                                    format!("{}<{}, ?>", base_enum, arg_ty)
                                } else if variant_name == "Err" {
                                    format!("{}<?, {}>", base_enum, arg_ty)
                                } else {
                                    enum_name.to_string()
                                }
                            } else {
                                enum_name.to_string()
                            }
                        } else if args.is_empty() {
                            if base_enum == "Option" {
                                format!("{}<?>", base_enum)
                            } else if base_enum == "Result" {
                                format!("{}<?, ?>", base_enum)
                            } else {
                                enum_name.to_string()
                            }
                        } else {
                            enum_name.to_string()
                        }
                    } else {
                        "i32".to_string()
                    }
                } else {
                    match callee.as_str() {
                        "None" => "Option<?>".to_string(),
                        "Some" => {
                            if args.len() == 1 {
                                let arg_ty = self.infer_vox_type(&args[0]);
                                format!("Option<{}>", arg_ty)
                            } else {
                                "i32".to_string()
                            }
                        }
                        "Ok" => {
                            if args.len() == 1 {
                                let arg_ty = self.infer_vox_type(&args[0]);
                                format!("Result<{}, ?>", arg_ty)
                            } else {
                                "i32".to_string()
                            }
                        }
                        "Err" => {
                            if args.len() == 1 {
                                let arg_ty = self.infer_vox_type(&args[0]);
                                format!("Result<?, {}>", arg_ty)
                            } else {
                                "i32".to_string()
                            }
                        }
                        _ => "i32".to_string(),
                    }
                }
            }
            BinaryExpr { left, .. } => self.infer_vox_type(left),
            CastExpr { target_type, .. } => target_type.clone(),
            MatchExpr { arms, .. } => {
                if let Some(first_arm) = arms.first() {
                    if let Some(expr) = first_arm.body.first() {
                        self.infer_vox_type(expr)
                    } else {
                        "i32".to_string()
                    }
                } else {
                    "i32".to_string()
                }
            }
            BorrowExpr { mutable, expr, .. } => {
                let inner = self.infer_vox_type(expr);
                if *mutable {
                    format!("&mut {}", inner)
                } else {
                    format!("& {}", inner)
                }
            }
            DerefExpr(expr, _) => {
                let inner = self.infer_vox_type(expr);
                if let Some(s) = inner.strip_prefix("&mut ") {
                    s.to_string()
                } else if let Some(s) = inner.strip_prefix("& ") {
                    s.to_string()
                } else {
                    "i32".to_string()
                }
            }
            _ => "i32".to_string(),
        };
        result
    }

    /// Attempt to constant‑fold an expression at compile time.
    /// Returns `Some(String)` with the constant value if successful, otherwise `None`.
    pub fn const_fold_expr(&self, node: &ASTNode) -> Option<String> {
        match node {
            ASTNode::IntegerLiteral(v, _) => Some(v.to_string()),
            ASTNode::FloatLiteral(v, _) => Some(format!("{:.10}", v)),
            ASTNode::CharLiteral(c, _) => Some(c.to_string()),
            ASTNode::BinaryExpr {
                left, op, right, ..
            } => {
                let l = self.const_fold_expr(left)?;
                let r = self.const_fold_expr(right)?;
                if let (Ok(l_int), Ok(r_int)) = (l.parse::<i64>(), r.parse::<i64>()) {
                    let result = match op {
                        TokenKind::Plus => l_int + r_int,
                        TokenKind::Minus => l_int - r_int,
                        TokenKind::Star => l_int * r_int,
                        TokenKind::Div => l_int / r_int,
                        TokenKind::Mod => l_int % r_int,
                        _ => return None,
                    };
                    return Some(result.to_string());
                }
                if let (Ok(l_float), Ok(r_float)) = (l.parse::<f64>(), r.parse::<f64>()) {
                    let result = match op {
                        TokenKind::Plus => l_float + r_float,
                        TokenKind::Minus => l_float - r_float,
                        TokenKind::Star => l_float * r_float,
                        TokenKind::Div => l_float / r_float,
                        _ => return None,
                    };
                    return Some(format!("{:.10}", result));
                }
                None
            }
            ASTNode::UnaryExpr { op, expr, .. } => {
                let v = self.const_fold_expr(expr)?;
                if let Ok(v_int) = v.parse::<i64>() {
                    let result = match op {
                        TokenKind::Minus => -v_int,
                        _ => return None,
                    };
                    return Some(result.to_string());
                }
                if let Ok(v_float) = v.parse::<f64>() {
                    let result = match op {
                        TokenKind::Minus => -v_float,
                        _ => return None,
                    };
                    return Some(format!("{:.10}", result));
                }
                None
            }
            _ => None,
        }
    }
}
