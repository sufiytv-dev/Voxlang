// refinement.rs - Z3-based verification for refinement types and postconditions.
// Uses binary Z3 (no crate) to prove where-clauses and return refinements.

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic};
use crate::frontend::span::Span;
use crate::module::compute_hash;
use crate::parser::{ASTNode, HashAlgorithm};
use std::fs;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

// -----------------------------------------------------------------------------
// Global cache control – disabled
// -----------------------------------------------------------------------------
static PROOF_CACHE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_proof_cache_enabled(enabled: bool) {
    PROOF_CACHE_ENABLED.store(enabled, Ordering::Relaxed);
}

// -----------------------------------------------------------------------------
// Helper: compute a stable cache key (unused but kept for compatibility)
// -----------------------------------------------------------------------------
#[allow(dead_code)]
fn compute_cache_key(
    function_name: &str,
    var_name: &str,
    value: i64,
    condition: &ASTNode,
) -> String {
    let cond_str = format!("{:?}", condition);
    let input = format!("{}::{}::{}::{}", function_name, var_name, value, cond_str);
    compute_hash(&input, &HashAlgorithm::Sha256)
}

// -----------------------------------------------------------------------------
// Z3 binary invocation helpers
// -----------------------------------------------------------------------------
static TEMP_COUNTER: Mutex<u32> = Mutex::new(0);

fn run_z3(smt: &str) -> Result<Z3Result, String> {
    if std::env::var("VOX_DEBUG_Z3").is_ok() {
        debug_log(format!(
            "=== Z3 SMT Script ===\n{}\n====================",
            smt
        ));
    }

    let pid = std::process::id();
    let mut counter = TEMP_COUNTER.lock().unwrap();
    *counter += 1;
    let temp_file = std::env::temp_dir().join(format!("vox_z3_{}_{}.smt2", pid, *counter));
    fs::write(&temp_file, smt).map_err(|e| format!("Failed to write SMT file: {}", e))?;

    let output = Command::new("z3")
        .arg(temp_file.to_str().unwrap())
        .output()
        .map_err(|e| format!("Failed to run Z3: {}", e))?;

    let _ = fs::remove_file(&temp_file);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stderr.is_empty() {
        debug_log(format!("Z3 stderr: {}", stderr));
    }

    if stdout.contains("unsat") {
        Ok(Z3Result::Unsat)
    } else if stdout.contains("sat") {
        let model = parse_model(&stdout);
        Ok(Z3Result::Sat(model))
    } else {
        debug_log(format!("Z3 output (neither unsat nor sat): {}", stdout));
        Ok(Z3Result::Unknown)
    }
}

enum Z3Result {
    Unsat,
    Sat(Vec<(String, i64)>),
    Unknown,
}

/// Parse Z3 model output for integer constants.
fn parse_model(z3_output: &str) -> Vec<(String, i64)> {
    let mut result = Vec::new();
    for line in z3_output.lines() {
        if line.contains("(define-fun") && line.contains("Int") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                let name = parts[1].trim_start_matches('(').to_string();
                let val_str = parts[4].trim_end_matches(')');
                if let Ok(val) = val_str.parse::<i64>() {
                    result.push((name, val));
                }
            }
        }
        if line.contains("(define-const") && line.contains("Int") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let name = parts[1].to_string();
                let val_str = parts[3].trim_end_matches(')');
                if let Ok(val) = val_str.parse::<i64>() {
                    result.push((name, val));
                }
            }
        }
    }
    result
}

// -----------------------------------------------------------------------------
// Refinement verification (parameter refinements)
// -----------------------------------------------------------------------------
pub fn verify_refinement_with_ctx(
    condition: &ASTNode,
    var_name: &str,
    initial_value: i64,
    span: Span,
    _function_name: &str,
    _use_cache: bool,
) -> bool {
    let smt = format!(
        "(set-logic QF_LIA)\n\
         (declare-const {} Int)\n\
         (assert (= {} {}))\n\
         (assert (not {}))\n\
         (check-sat)\n",
        var_name,
        var_name,
        initial_value,
        translate_condition_to_smt(condition, var_name)
    );

    match run_z3(&smt) {
        Ok(Z3Result::Unsat) => true,
        Ok(Z3Result::Sat(model)) => {
            let val = model
                .iter()
                .find(|(n, _)| n == var_name)
                .map(|(_, v)| *v)
                .unwrap_or(initial_value);
            let cond_str = format_condition(condition, var_name);
            emit_diagnostic(
                &Diagnostic::error(format!(
                    "Refinement `{}` fails for variable `{}` with value {}",
                    cond_str, var_name, val
                ))
                .with_code("VX0300")
                .with_span(span),
            );
            false
        }
        Ok(Z3Result::Unknown) => {
            emit_diagnostic(
                &Diagnostic::warning("Z3 could not verify refinement; assuming true.")
                    .with_code("VX0301")
                    .with_span(span),
            );
            true
        }
        Err(e) => {
            emit_diagnostic(
                &Diagnostic::error(format!("Z3 execution failed: {}", e))
                    .with_code("VX0399")
                    .with_span(span),
            );
            false
        }
    }
}

pub fn verify_refinement(
    condition: &ASTNode,
    var_name: &str,
    initial_value: i64,
    span: Span,
) -> bool {
    verify_refinement_with_ctx(condition, var_name, initial_value, span, "global", true)
}

// -----------------------------------------------------------------------------
// Return refinement verification
// -----------------------------------------------------------------------------
pub fn verify_return_refinement(
    condition: &ASTNode,
    return_expr: &ASTNode,
    param_refinements: &[(String, Option<Box<ASTNode>>)],
    path_condition: Option<&ASTNode>,
    _function_name: &str,
    span: Span,
) -> bool {
    let mut smt = String::new();
    smt.push_str("(set-logic QF_LIA)\n");

    for (name, _) in param_refinements {
        smt.push_str(&format!("(declare-const {} Int)\n", name));
    }
    smt.push_str("(declare-const result Int)\n");

    for (name, refinement_opt) in param_refinements {
        if let Some(refinement_node) = refinement_opt {
            if let Some(cond) = extract_condition_from_refinement(refinement_node) {
                let cond_smt = translate_condition_to_smt(cond, name);
                smt.push_str(&format!("(assert {})\n", cond_smt));
            }
        }
    }

    if let Some(path) = path_condition {
        let path_smt = translate_condition_to_smt(path, "");
        smt.push_str(&format!("(assert {})\n", path_smt));
    }

    let return_smt = translate_expr_to_smt(return_expr, param_refinements);
    smt.push_str(&format!("(assert (= result {}))\n", return_smt));

    let refined_cond = translate_condition_to_smt(condition, "result");
    smt.push_str(&format!("(assert (not {}))\n", refined_cond));

    smt.push_str("(check-sat)\n");

    match run_z3(&smt) {
        Ok(Z3Result::Unsat) => true,
        Ok(Z3Result::Sat(model)) => {
            let mut counterexample = String::new();
            for (name, val) in &model {
                counterexample.push_str(&format!("{} = {}; ", name, val));
            }
            let cond_str = format_condition(condition, "result");
            emit_diagnostic(
                &Diagnostic::error(format!(
                    "Return refinement `{}` fails with counterexample: {}",
                    cond_str, counterexample
                ))
                .with_code("VX0455")
                .with_span(span),
            );
            false
        }
        Ok(Z3Result::Unknown) => {
            emit_diagnostic(
                &Diagnostic::warning("Z3 could not verify return refinement; assuming true.")
                    .with_code("VX0301")
                    .with_span(span),
            );
            true
        }
        Err(e) => {
            emit_diagnostic(
                &Diagnostic::error(format!("Z3 execution failed: {}", e))
                    .with_code("VX0399")
                    .with_span(span),
            );
            false
        }
    }
}

// -----------------------------------------------------------------------------
// SMT‑LIB translation functions
// -----------------------------------------------------------------------------

/// Translate a boolean condition to SMT‑LIB.
fn translate_condition_to_smt(node: &ASTNode, var_name: &str) -> String {
    match node {
        ASTNode::BinaryExpr {
            left, op, right, ..
        } => {
            use crate::frontend::token::TokenKind;
            match op {
                TokenKind::And => {
                    let left_smt = translate_condition_to_smt(left, var_name);
                    let right_smt = translate_condition_to_smt(right, var_name);
                    format!("(and {} {})", left_smt, right_smt)
                }
                TokenKind::Or => {
                    let left_smt = translate_condition_to_smt(left, var_name);
                    let right_smt = translate_condition_to_smt(right, var_name);
                    format!("(or {} {})", left_smt, right_smt)
                }
                TokenKind::Equal => {
                    let left_smt = translate_expr_to_smt(left, &[]);
                    let right_smt = translate_expr_to_smt(right, &[]);
                    format!("(= {} {})", left_smt, right_smt)
                }
                TokenKind::NotEqual => {
                    let left_smt = translate_expr_to_smt(left, &[]);
                    let right_smt = translate_expr_to_smt(right, &[]);
                    format!("(not (= {} {}))", left_smt, right_smt)
                }
                TokenKind::LessThan => {
                    let left_smt = translate_expr_to_smt(left, &[]);
                    let right_smt = translate_expr_to_smt(right, &[]);
                    format!("(< {} {})", left_smt, right_smt)
                }
                TokenKind::GreaterThan => {
                    let left_smt = translate_expr_to_smt(left, &[]);
                    let right_smt = translate_expr_to_smt(right, &[]);
                    format!("(> {} {})", left_smt, right_smt)
                }
                TokenKind::LessThanOrEqual => {
                    let left_smt = translate_expr_to_smt(left, &[]);
                    let right_smt = translate_expr_to_smt(right, &[]);
                    format!("(<= {} {})", left_smt, right_smt)
                }
                TokenKind::GreaterThanOrEqual => {
                    let left_smt = translate_expr_to_smt(left, &[]);
                    let right_smt = translate_expr_to_smt(right, &[]);
                    format!("(>= {} {})", left_smt, right_smt)
                }
                _ => "true".to_string(),
            }
        }
        ASTNode::Identifier(name, _) => format!("(not (= {} 0))", name),
        ASTNode::IntegerLiteral(val, _) => {
            if *val == 0 {
                "false".to_string()
            } else {
                "true".to_string()
            }
        }
        _ => "true".to_string(),
    }
}

/// Translate an integer expression to SMT‑LIB.
fn translate_expr_to_smt(
    node: &ASTNode,
    param_refinements: &[(String, Option<Box<ASTNode>>)],
) -> String {
    match node {
        ASTNode::Identifier(name, _) => name.clone(),
        ASTNode::IntegerLiteral(val, _) => val.to_string(),
        ASTNode::BinaryExpr {
            left, op, right, ..
        } => {
            use crate::frontend::token::TokenKind;
            let left_smt = translate_expr_to_smt(left, param_refinements);
            let right_smt = translate_expr_to_smt(right, param_refinements);
            match op {
                TokenKind::Plus => format!("(+ {} {})", left_smt, right_smt),
                TokenKind::Minus => format!("(- {} {})", left_smt, right_smt),
                TokenKind::Star => format!("(* {} {})", left_smt, right_smt),
                TokenKind::Div => format!("(div {} {})", left_smt, right_smt),
                _ => left_smt,
            }
        }
        _ => "0".to_string(),
    }
}

fn extract_condition_from_refinement(node: &ASTNode) -> Option<&ASTNode> {
    match node {
        ASTNode::RefinedType { condition, .. } => Some(condition.as_ref()),
        _ => Some(node),
    }
}

// -----------------------------------------------------------------------------
// Formatting helper
// -----------------------------------------------------------------------------
fn format_condition(condition: &ASTNode, var_name: &str) -> String {
    match condition {
        ASTNode::BinaryExpr {
            left, op, right, ..
        } => {
            let left_str = format_condition(left, var_name);
            let op_str = match op {
                crate::frontend::token::TokenKind::Equal => "==",
                crate::frontend::token::TokenKind::NotEqual => "!=",
                crate::frontend::token::TokenKind::LessThan => "<",
                crate::frontend::token::TokenKind::GreaterThan => ">",
                crate::frontend::token::TokenKind::LessThanOrEqual => "<=",
                crate::frontend::token::TokenKind::GreaterThanOrEqual => ">=",
                crate::frontend::token::TokenKind::And => "&&",
                crate::frontend::token::TokenKind::Or => "||",
                crate::frontend::token::TokenKind::Plus => "+",
                crate::frontend::token::TokenKind::Minus => "-",
                crate::frontend::token::TokenKind::Star => "*",
                crate::frontend::token::TokenKind::Div => "/",
                _ => "?",
            };
            let right_str = format_condition(right, var_name);
            format!("{} {} {}", left_str, op_str, right_str)
        }
        ASTNode::Identifier(name, _) => name.clone(),
        ASTNode::IntegerLiteral(val, _) => val.to_string(),
        _ => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::span::Span;
    use crate::frontend::token::TokenKind;
    use crate::parser::ASTNode;

    #[test]
    fn test_verify_true() {
        let cond = ASTNode::BinaryExpr {
            left: Box::new(ASTNode::Identifier("x".to_string(), Span::new(0, 0, 0, 0))),
            op: TokenKind::GreaterThan,
            right: Box::new(ASTNode::IntegerLiteral(0, Span::new(0, 0, 0, 0))),
            span: Span::new(0, 0, 0, 0),
        };
        let span = Span::new(0, 0, 0, 0);
        let result = verify_refinement(&cond, "x", 5, span);
        assert!(result);
    }

    #[test]
    fn test_verify_false() {
        let cond = ASTNode::BinaryExpr {
            left: Box::new(ASTNode::Identifier("x".to_string(), Span::new(0, 0, 0, 0))),
            op: TokenKind::GreaterThan,
            right: Box::new(ASTNode::IntegerLiteral(0, Span::new(0, 0, 0, 0))),
            span: Span::new(0, 0, 0, 0),
        };
        let span = Span::new(0, 0, 0, 0);
        let result = verify_refinement(&cond, "x", 0, span);
        assert!(!result);
    }

    #[test]
    fn test_arithmetic() {
        let cond = ASTNode::BinaryExpr {
            left: Box::new(ASTNode::BinaryExpr {
                left: Box::new(ASTNode::Identifier("x".to_string(), Span::new(0, 0, 0, 0))),
                op: TokenKind::Plus,
                right: Box::new(ASTNode::IntegerLiteral(5, Span::new(0, 0, 0, 0))),
                span: Span::new(0, 0, 0, 0),
            }),
            op: TokenKind::GreaterThan,
            right: Box::new(ASTNode::IntegerLiteral(10, Span::new(0, 0, 0, 0))),
            span: Span::new(0, 0, 0, 0),
        };
        let span = Span::new(0, 0, 0, 0);
        let result = verify_refinement(&cond, "x", 6, span);
        assert!(result);
    }

    #[test]
    fn test_logical_and() {
        let left = ASTNode::BinaryExpr {
            left: Box::new(ASTNode::Identifier("x".to_string(), Span::new(0, 0, 0, 0))),
            op: TokenKind::GreaterThan,
            right: Box::new(ASTNode::IntegerLiteral(0, Span::new(0, 0, 0, 0))),
            span: Span::new(0, 0, 0, 0),
        };
        let right = ASTNode::BinaryExpr {
            left: Box::new(ASTNode::Identifier("x".to_string(), Span::new(0, 0, 0, 0))),
            op: TokenKind::LessThan,
            right: Box::new(ASTNode::IntegerLiteral(10, Span::new(0, 0, 0, 0))),
            span: Span::new(0, 0, 0, 0),
        };
        let cond = ASTNode::BinaryExpr {
            left: Box::new(left),
            op: TokenKind::And,
            right: Box::new(right),
            span: Span::new(0, 0, 0, 0),
        };
        let span = Span::new(0, 0, 0, 0);
        let result = verify_refinement(&cond, "x", 5, span);
        assert!(result);
    }
}
