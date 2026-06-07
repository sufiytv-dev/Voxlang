// comptime.rs - Compile-time evaluation for @comptime blocks.
// Supports multi-statement blocks, variables, assignments, while/if.
// Returns an integer constant or None on failure.

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};
use crate::frontend::token::TokenKind;
use crate::parser::ASTNode;
use std::collections::HashMap;

/// Evaluation environment mapping variable names to integer values.
#[derive(Debug, Default)]
struct EvalEnv {
    vars: HashMap<String, i64>,
}

impl EvalEnv {
    fn new() -> Self {
        Self::default()
    }
}

pub struct ComptimeEvaluator;

impl ComptimeEvaluator {
    /// Evaluate a comptime block or expression.
    /// Returns a constant ASTNode (IntegerLiteral) if successful, otherwise None.
    pub fn evaluate(node: &ASTNode) -> Option<ASTNode> {
        debug_log(format!("Evaluating comptime node: {:?}", node));
        match node {
            ASTNode::ComptimeBlock { body, span } => {
                let mut env = EvalEnv::new();
                let result = Self::eval_block(&mut env, body)?;
                Some(ASTNode::IntegerLiteral(result, *span))
            }
            _ => {
                let mut env = EvalEnv::new();
                Self::eval_expr(&mut env, node).map(|v| ASTNode::IntegerLiteral(v, node.span()))
            }
        }
    }

    /// Evaluate a sequence of statements, returning the last value.
    fn eval_block(env: &mut EvalEnv, stmts: &[ASTNode]) -> Option<i64> {
        let mut last = None;
        for stmt in stmts {
            last = Some(Self::eval_statement(env, stmt)?);
        }
        last
    }

    /// Evaluate a single statement, returning its resulting value.
    fn eval_statement(env: &mut EvalEnv, stmt: &ASTNode) -> Option<i64> {
        match stmt {
            ASTNode::VariableDecl { name, value, .. } => {
                let v = Self::eval_expr(env, value)?;
                debug_log(format!("Comptime var {} = {}", name, v));
                env.vars.insert(name.clone(), v);
                Some(v)
            }
            ASTNode::Assignment { lhs, value, span } => {
                let lhs_name = match lhs.as_ref() {
                    ASTNode::Identifier(name, _) => name,
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "Left-hand side of assignment must be an identifier",
                            )
                            .with_code("E6001")
                            .with_span(*span),
                        );
                        return None;
                    }
                };
                let v = Self::eval_expr(env, value)?;
                debug_log(format!("Comptime assign {} = {}", lhs_name, v));
                env.vars.insert(lhs_name.clone(), v);
                Some(v)
            }
            ASTNode::WhileStatement {
                condition, body, ..
            } => {
                while Self::eval_expr(env, condition)? != 0 {
                    for b_stmt in body {
                        Self::eval_statement(env, b_stmt)?;
                    }
                }
                Some(0)
            }
            ASTNode::IfStatement {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                if Self::eval_expr(env, condition)? != 0 {
                    for stmt in then_branch {
                        Self::eval_statement(env, stmt)?;
                    }
                } else if let Some(else_branch) = else_branch {
                    for stmt in else_branch {
                        Self::eval_statement(env, stmt)?;
                    }
                }
                Some(0)
            }
            ASTNode::Block { statements, .. } => {
                let mut last = None;
                for stmt in statements {
                    last = Some(Self::eval_statement(env, stmt)?);
                }
                last
            }
            _ => Self::eval_expr(env, stmt),
        }
    }

    /// Evaluate an expression node, returning its integer value.
    fn eval_expr(env: &mut EvalEnv, expr: &ASTNode) -> Option<i64> {
        match expr {
            ASTNode::IntegerLiteral(val, _) => Some(*val),
            ASTNode::Identifier(name, span) => match env.vars.get(name) {
                Some(v) => Some(*v),
                None => {
                    emit_diagnostic(
                        &Diagnostic::error(format!("Undefined variable `{}` in comptime", name))
                            .with_code("E6002")
                            .with_span(*span),
                    );
                    None
                }
            },
            ASTNode::UnaryExpr { op, expr, span } => {
                let inner = Self::eval_expr(env, expr)?;
                match op {
                    TokenKind::Not => Some(if inner == 0 { 1 } else { 0 }),
                    TokenKind::Minus => Some(-inner),
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error("Unsupported unary operator in comptime")
                                .with_code("E6003")
                                .with_span(*span),
                        );
                        None
                    }
                }
            }
            ASTNode::BinaryExpr {
                left,
                op,
                right,
                span,
            } => {
                let l = Self::eval_expr(env, left)?;
                let r = Self::eval_expr(env, right)?;
                match op {
                    TokenKind::Plus => Some(l + r),
                    TokenKind::Minus => Some(l - r),
                    TokenKind::Star => Some(l * r),
                    TokenKind::Div => {
                        if r == 0 {
                            emit_diagnostic(
                                &Diagnostic::error("Division by zero in comptime")
                                    .with_code("E6004")
                                    .with_span(*span),
                            );
                            return None;
                        }
                        Some(l / r)
                    }
                    TokenKind::Mod => {
                        if r == 0 {
                            emit_diagnostic(
                                &Diagnostic::error("Modulo by zero in comptime")
                                    .with_code("E6005")
                                    .with_span(*span),
                            );
                            return None;
                        }
                        Some(l % r)
                    }
                    TokenKind::Equal => Some((l == r) as i64),
                    TokenKind::NotEqual => Some((l != r) as i64),
                    TokenKind::LessThan => Some((l < r) as i64),
                    TokenKind::GreaterThan => Some((l > r) as i64),
                    TokenKind::LessThanOrEqual => Some((l <= r) as i64),
                    TokenKind::GreaterThanOrEqual => Some((l >= r) as i64),
                    TokenKind::And => Some((l != 0 && r != 0) as i64),
                    TokenKind::Or => Some((l != 0 || r != 0) as i64),
                    TokenKind::Pipe => Some(l | r),
                    TokenKind::Ampersand => Some(l & r),
                    TokenKind::Caret => Some(l ^ r),
                    TokenKind::Shl => Some(l << r),
                    TokenKind::Shr => Some(l >> r),
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error("Unsupported binary operator in comptime")
                                .with_code("E6003")
                                .with_span(*span),
                        );
                        None
                    }
                }
            }
            ASTNode::CastExpr {
                expr,
                target_type,
                span,
            } => {
                let val = Self::eval_expr(env, expr)?;
                match target_type.as_str() {
                    "i8" => Some((val as i8) as i64),
                    "i16" => Some((val as i16) as i64),
                    "i32" => Some((val as i32) as i64),
                    "i64" => Some(val),
                    "u8" => Some((val as u8) as i64),
                    "u16" => Some((val as u16) as i64),
                    "u32" => Some((val as u32) as i64),
                    "u64" => Some(val),
                    "char" => {
                        if val >= 0 && val <= 0x10FFFF {
                            Some(val)
                        } else {
                            emit_diagnostic(
                                &Diagnostic::error(format!("Invalid char cast value {}", val))
                                    .with_code("E6006")
                                    .with_span(*span),
                            );
                            None
                        }
                    }
                    _ => {
                        emit_diagnostic(
                            &Diagnostic::error(format!(
                                "Unsupported cast target `{}`",
                                target_type
                            ))
                            .with_code("E6007")
                            .with_span(*span),
                        );
                        None
                    }
                }
            }
            ASTNode::FloatLiteral(_, span) | ASTNode::CharLiteral(_, span) => {
                emit_diagnostic(
                    &Diagnostic::error("Float or char literals not allowed in integer comptime")
                        .with_code("E6008")
                        .with_span(*span),
                );
                None
            }
            _ => {
                emit_diagnostic(
                    &Diagnostic::error("Unsupported node type in comptime evaluation")
                        .with_code("E6009")
                        .with_span(expr.span()),
                );
                None
            }
        }
    }
}
