// desugar.rs - Syntactic desugaring pass for Voxlang.
// Converts syntactic sugar (for, if let, while let) into core AST.
// Runs immediately after parsing, before semantic analysis.

use crate::diagnostic::debug_log;
use crate::parser::{ASTNode, MatchArm, MatchPattern};
use std::cell::Cell;

thread_local! {
    static TMP_COUNTER: Cell<usize> = Cell::new(0);
}

fn fresh_tmp_var() -> String {
    TMP_COUNTER.with(|c| {
        let id = c.get();
        c.set(id + 1);
        format!("__vox_tmp_{}", id)
    })
}

/// Recursively desugar an AST node into the core language.
pub fn desugar(node: ASTNode) -> ASTNode {
    match node {
        ASTNode::Program(stmts, span) => {
            let new_stmts = stmts.into_iter().map(desugar).collect();
            ASTNode::Program(new_stmts, span)
        }

        ASTNode::Import {
            source,
            alias,
            hash,
            span,
        } => ASTNode::Import {
            source,
            alias,
            hash,
            span,
        },

        ASTNode::StructDef {
            name,
            generic_params,
            fields,
            span,
        } => ASTNode::StructDef {
            name,
            generic_params,
            fields,
            span,
        },

        ASTNode::EnumDef {
            name,
            params,
            variants,
            span,
        } => ASTNode::EnumDef {
            name,
            params,
            variants,
            span,
        },

        ASTNode::TypeAlias {
            name,
            target_type,
            span,
        } => ASTNode::TypeAlias {
            name,
            target_type,
            span,
        },

        ASTNode::UseDecl {
            path,
            alias,
            is_glob,
            span,
        } => ASTNode::UseDecl {
            path,
            alias,
            is_glob,
            span,
        },

        ASTNode::FunctionDef {
            name,
            generic_params,
            params,
            return_type,
            return_refinement,
            body,
            span,
        } => {
            let new_body = body.into_iter().map(desugar).collect();
            ASTNode::FunctionDef {
                name,
                generic_params,
                params,
                return_type,
                return_refinement,
                body: new_body,
                span,
            }
        }

        ASTNode::KernelFn {
            name,
            params,
            body,
            device_triple,
            span,
        } => {
            let new_body = body.into_iter().map(desugar).collect();
            ASTNode::KernelFn {
                name,
                params,
                body: new_body,
                device_triple,
                span,
            }
        }

        ASTNode::IfStatement {
            condition,
            then_branch,
            else_branch,
            span,
        } => {
            let new_cond = Box::new(desugar(*condition));
            let new_then = then_branch.into_iter().map(desugar).collect();
            let new_else = else_branch.map(|b| b.into_iter().map(desugar).collect());
            ASTNode::IfStatement {
                condition: new_cond,
                then_branch: new_then,
                else_branch: new_else,
                span,
            }
        }

        ASTNode::IfLetStatement {
            pattern,
            expr,
            then_branch,
            else_branch,
            span,
        } => {
            debug_log("Desugaring IfLetStatement into MatchExpr");
            let scrutinee = Box::new(desugar(*expr));
            let then_body = then_branch.into_iter().map(desugar).collect();
            let mut arms = vec![MatchArm {
                pattern,
                body: then_body,
                span,
            }];
            if let Some(else_body) = else_branch {
                let wildcard = MatchPattern::Wildcard(span);
                arms.push(MatchArm {
                    pattern: wildcard,
                    body: else_body.into_iter().map(desugar).collect(),
                    span,
                });
            }
            ASTNode::MatchExpr {
                value: scrutinee,
                arms,
                span,
            }
        }

        ASTNode::WhileStatement {
            condition,
            body,
            span,
        } => {
            let new_cond = Box::new(desugar(*condition));
            let new_body = body.into_iter().map(desugar).collect();
            ASTNode::WhileStatement {
                condition: new_cond,
                body: new_body,
                span,
            }
        }

        ASTNode::WhileLetStatement {
            pattern,
            expr,
            body,
            span,
        } => {
            debug_log("Desugaring WhileLetStatement into while + match");
            let scrutinee = desugar(*expr);
            let loop_body = body.into_iter().map(desugar).collect();
            let break_var = fresh_tmp_var();

            let break_decl = ASTNode::VariableDecl {
                name: break_var.clone(),
                ty: Some("i32".to_string()),
                refinement: None,
                value: Box::new(ASTNode::IntegerLiteral(0, span)),
                mutable: true,
                span,
            };

            let match_arm = MatchArm {
                pattern,
                body: loop_body,
                span,
            };
            let break_arm = MatchArm {
                pattern: MatchPattern::Wildcard(span),
                body: vec![ASTNode::Assignment {
                    lhs: Box::new(ASTNode::Identifier(break_var.clone(), span)),
                    value: Box::new(ASTNode::IntegerLiteral(1, span)),
                    span,
                }],
                span,
            };

            let match_expr = ASTNode::MatchExpr {
                value: Box::new(scrutinee),
                arms: vec![match_arm, break_arm],
                span,
            };

            let while_cond = ASTNode::BinaryExpr {
                left: Box::new(ASTNode::Identifier(break_var.clone(), span)),
                op: crate::frontend::token::TokenKind::Equal,
                right: Box::new(ASTNode::IntegerLiteral(0, span)),
                span,
            };

            let while_stmt = ASTNode::WhileStatement {
                condition: Box::new(while_cond),
                body: vec![match_expr],
                span,
            };

            ASTNode::Block {
                statements: vec![break_decl, while_stmt],
                span,
            }
        }

        ASTNode::ForLoop {
            iter_var,
            start,
            end,
            body,
            span,
        } => {
            debug_log("Desugaring ForLoop into while loop with counter");
            let start_expr = desugar(*start);
            let end_expr = desugar(*end);
            let counter_var = fresh_tmp_var();
            let end_var = fresh_tmp_var();

            let counter_decl = ASTNode::VariableDecl {
                name: counter_var.clone(),
                ty: Some("i32".to_string()),
                refinement: None,
                value: Box::new(start_expr),
                mutable: true,
                span,
            };

            let end_decl = ASTNode::VariableDecl {
                name: end_var.clone(),
                ty: Some("i32".to_string()),
                refinement: None,
                value: Box::new(end_expr),
                mutable: false,
                span,
            };

            let while_cond = ASTNode::BinaryExpr {
                left: Box::new(ASTNode::Identifier(counter_var.clone(), span)),
                op: crate::frontend::token::TokenKind::LessThan,
                right: Box::new(ASTNode::Identifier(end_var.clone(), span)),
                span,
            };

            let iter_binding = ASTNode::VariableDecl {
                name: iter_var.clone(),
                ty: None,
                refinement: None,
                value: Box::new(ASTNode::Identifier(counter_var.clone(), span)),
                mutable: false,
                span,
            };

            let increment = ASTNode::Assignment {
                lhs: Box::new(ASTNode::Identifier(counter_var.clone(), span)),
                value: Box::new(ASTNode::BinaryExpr {
                    left: Box::new(ASTNode::Identifier(counter_var.clone(), span)),
                    op: crate::frontend::token::TokenKind::Plus,
                    right: Box::new(ASTNode::IntegerLiteral(1, span)),
                    span,
                }),
                span,
            };

            let original_block = body[0].clone();
            let desugared_original_block = desugar(original_block);
            let desugared_iter_binding = desugar(iter_binding);
            let desugared_increment = desugar(increment);

            let while_body_block = ASTNode::Block {
                statements: vec![
                    desugared_iter_binding,
                    desugared_original_block,
                    desugared_increment,
                ],
                span,
            };

            let while_stmt = ASTNode::WhileStatement {
                condition: Box::new(while_cond),
                body: vec![while_body_block],
                span,
            };

            ASTNode::Block {
                statements: vec![counter_decl, end_decl, while_stmt],
                span,
            }
        }

        ASTNode::ParallelLoop {
            iter_var,
            start,
            end,
            body,
            span,
        } => {
            let new_start = Box::new(desugar(*start));
            let new_end = Box::new(desugar(*end));
            let new_body = body.into_iter().map(desugar).collect();
            ASTNode::ParallelLoop {
                iter_var,
                start: new_start,
                end: new_end,
                body: new_body,
                span,
            }
        }

        ASTNode::ComptimeBlock { body, span } => {
            let new_body = body.into_iter().map(desugar).collect();
            ASTNode::ComptimeBlock {
                body: new_body,
                span,
            }
        }

        ASTNode::ReturnStatement(expr, span) => {
            let new_expr = expr.map(|e| Box::new(desugar(*e)));
            ASTNode::ReturnStatement(new_expr, span)
        }

        ASTNode::VariableDecl {
            name,
            ty,
            refinement,
            value,
            mutable,
            span,
        } => {
            let new_val = Box::new(desugar(*value));
            ASTNode::VariableDecl {
                name,
                ty,
                refinement,
                value: new_val,
                mutable,
                span,
            }
        }

        ASTNode::DeviceVarDecl {
            name,
            ty,
            refinement,
            value,
            span,
        } => {
            let new_val = Box::new(desugar(*value));
            ASTNode::DeviceVarDecl {
                name,
                ty,
                refinement,
                value: new_val,
                span,
            }
        }

        ASTNode::Assignment { lhs, value, span } => {
            let new_lhs = Box::new(desugar(*lhs));
            let new_val = Box::new(desugar(*value));
            ASTNode::Assignment {
                lhs: new_lhs,
                value: new_val,
                span,
            }
        }

        ASTNode::BinaryExpr {
            left,
            op,
            right,
            span,
        } => {
            let new_left = Box::new(desugar(*left));
            let new_right = Box::new(desugar(*right));
            ASTNode::BinaryExpr {
                left: new_left,
                op,
                right: new_right,
                span,
            }
        }

        ASTNode::UnaryExpr { op, expr, span } => {
            let new_expr = Box::new(desugar(*expr));
            ASTNode::UnaryExpr {
                op,
                expr: new_expr,
                span,
            }
        }

        ASTNode::CastExpr {
            expr,
            target_type,
            span,
        } => {
            let new_expr = Box::new(desugar(*expr));
            ASTNode::CastExpr {
                expr: new_expr,
                target_type,
                span,
            }
        }

        ASTNode::CallExpr { callee, args, span } => {
            let new_args = args.into_iter().map(desugar).collect();
            ASTNode::CallExpr {
                callee,
                args: new_args,
                span,
            }
        }

        ASTNode::StructLiteral { name, fields, span } => {
            let new_fields = fields
                .into_iter()
                .map(|(fname, expr)| (fname, desugar(expr)))
                .collect();
            ASTNode::StructLiteral {
                name,
                fields: new_fields,
                span,
            }
        }

        ASTNode::BorrowExpr {
            mutable,
            expr,
            span,
        } => {
            let new_expr = Box::new(desugar(*expr));
            ASTNode::BorrowExpr {
                mutable,
                expr: new_expr,
                span,
            }
        }

        ASTNode::DerefExpr(expr, span) => {
            let new_expr = Box::new(desugar(*expr));
            ASTNode::DerefExpr(new_expr, span)
        }

        ASTNode::FieldAccess { expr, field, span } => {
            let new_expr = Box::new(desugar(*expr));
            ASTNode::FieldAccess {
                expr: new_expr,
                field,
                span,
            }
        }

        ASTNode::ArrayLiteral { elements, span } => {
            let new_elems = elements.into_iter().map(desugar).collect();
            ASTNode::ArrayLiteral {
                elements: new_elems,
                span,
            }
        }

        ASTNode::ArrayIndex { array, index, span } => {
            let new_array = Box::new(desugar(*array));
            let new_index = Box::new(desugar(*index));
            ASTNode::ArrayIndex {
                array: new_array,
                index: new_index,
                span,
            }
        }

        ASTNode::SliceExpr {
            base,
            start,
            end,
            span,
        } => {
            let new_base = Box::new(desugar(*base));
            let new_start = start.map(|s| Box::new(desugar(*s)));
            let new_end = end.map(|e| Box::new(desugar(*e)));
            ASTNode::SliceExpr {
                base: new_base,
                start: new_start,
                end: new_end,
                span,
            }
        }

        ASTNode::MatchExpr { value, arms, span } => {
            let new_value = Box::new(desugar(*value));
            let new_arms = arms
                .into_iter()
                .map(|arm| MatchArm {
                    pattern: arm.pattern,
                    body: arm.body.into_iter().map(desugar).collect(),
                    span: arm.span,
                })
                .collect();
            ASTNode::MatchExpr {
                value: new_value,
                arms: new_arms,
                span,
            }
        }

        ASTNode::TryExpr { expr, span } => ASTNode::TryExpr {
            expr: Box::new(desugar(*expr)),
            span,
        },

        ASTNode::Identifier(name, span) => ASTNode::Identifier(name, span),
        ASTNode::IntegerLiteral(v, s) => ASTNode::IntegerLiteral(v, s),
        ASTNode::FloatLiteral(v, s) => ASTNode::FloatLiteral(v, s),
        ASTNode::CharLiteral(c, s) => ASTNode::CharLiteral(c, s),
        ASTNode::StringLiteral(s, sp) => ASTNode::StringLiteral(s, sp),

        ASTNode::RefinedType {
            base,
            condition,
            span,
        } => {
            let new_base = Box::new(desugar(*base));
            let new_cond = Box::new(desugar(*condition));
            ASTNode::RefinedType {
                base: new_base,
                condition: new_cond,
                span,
            }
        }

        ASTNode::Lemma {
            name,
            params,
            return_type,
            proof,
            span,
        } => {
            let new_proof = proof.into_iter().map(desugar).collect();
            ASTNode::Lemma {
                name,
                params,
                return_type,
                proof: new_proof,
                span,
            }
        }

        ASTNode::Block { statements, span } => {
            let new_stmts = statements.into_iter().map(desugar).collect();
            ASTNode::Block {
                statements: new_stmts,
                span,
            }
        }

        ASTNode::Error => ASTNode::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::span::Span;

    fn dummy_span() -> Span {
        Span::new(0, 0, 0, 0)
    }

    #[test]
    fn test_desugar_if_let() {
        let pattern = MatchPattern::Binding {
            by_ref: false,
            enum_name: "Option".to_string(),
            variant: "Some".to_string(),
            bindings: vec!["x".to_string()],
            span: dummy_span(),
        };
        let expr = Box::new(ASTNode::Identifier("opt".to_string(), dummy_span()));
        let then_body = vec![ASTNode::CallExpr {
            callee: "print".to_string(),
            args: vec![ASTNode::Identifier("x".to_string(), dummy_span())],
            span: dummy_span(),
        }];
        let if_let = ASTNode::IfLetStatement {
            pattern,
            expr,
            then_branch: then_body,
            else_branch: None,
            span: dummy_span(),
        };
        let desugared = desugar(if_let);
        match desugared {
            ASTNode::MatchExpr { value, arms, .. } => {
                assert_eq!(arms.len(), 1);
                match &arms[0].pattern {
                    MatchPattern::Binding { variant, .. } => assert_eq!(variant, "Some"),
                    _ => panic!("Expected binding pattern"),
                }
                assert!(matches!(*value, ASTNode::Identifier(..)));
            }
            _ => panic!("Expected MatchExpr"),
        }
    }

    #[test]
    fn test_desugar_while_let() {
        let pattern = MatchPattern::Binding {
            by_ref: false,
            enum_name: "Option".to_string(),
            variant: "Some".to_string(),
            bindings: vec!["x".to_string()],
            span: dummy_span(),
        };
        let expr = Box::new(ASTNode::Identifier("iter".to_string(), dummy_span()));
        let body = vec![ASTNode::CallExpr {
            callee: "use".to_string(),
            args: vec![ASTNode::Identifier("x".to_string(), dummy_span())],
            span: dummy_span(),
        }];
        let while_let = ASTNode::WhileLetStatement {
            pattern,
            expr,
            body,
            span: dummy_span(),
        };
        let desugared = desugar(while_let);
        match desugared {
            ASTNode::Block { statements, .. } => {
                assert_eq!(statements.len(), 2);
                match &statements[0] {
                    ASTNode::VariableDecl { name, .. } => assert!(name.starts_with("__vox_tmp_")),
                    _ => panic!("Expected variable declaration"),
                }
                match &statements[1] {
                    ASTNode::WhileStatement {
                        condition, body, ..
                    } => {
                        assert!(matches!(&**condition, ASTNode::BinaryExpr { .. }));
                        assert_eq!(body.len(), 1);
                        match &body[0] {
                            ASTNode::MatchExpr { arms, .. } => assert_eq!(arms.len(), 2),
                            _ => panic!("Expected match expression"),
                        }
                    }
                    _ => panic!("Expected while statement"),
                }
            }
            _ => panic!("Expected block"),
        }
    }

    #[test]
    fn test_desugar_for_loop() {
        let start = Box::new(ASTNode::IntegerLiteral(0, dummy_span()));
        let end = Box::new(ASTNode::IntegerLiteral(10, dummy_span()));
        let body_block = ASTNode::Block {
            statements: vec![ASTNode::CallExpr {
                callee: "print".to_string(),
                args: vec![ASTNode::Identifier("i".to_string(), dummy_span())],
                span: dummy_span(),
            }],
            span: dummy_span(),
        };
        let for_loop = ASTNode::ForLoop {
            iter_var: "i".to_string(),
            start,
            end,
            body: vec![body_block],
            span: dummy_span(),
        };
        let desugared = desugar(for_loop);
        match desugared {
            ASTNode::Block { statements, .. } => {
                assert_eq!(statements.len(), 3);
                match &statements[0] {
                    ASTNode::VariableDecl { name, value, .. } => {
                        assert!(name.starts_with("__vox_tmp_"));
                        assert!(matches!(**value, ASTNode::IntegerLiteral(0, _)));
                    }
                    _ => panic!("Expected counter declaration"),
                }
                match &statements[1] {
                    ASTNode::VariableDecl { name, value, .. } => {
                        assert!(name.starts_with("__vox_tmp_"));
                        assert!(matches!(**value, ASTNode::IntegerLiteral(10, _)));
                    }
                    _ => panic!("Expected end declaration"),
                }
                match &statements[2] {
                    ASTNode::WhileStatement {
                        condition, body, ..
                    } => {
                        match &**condition {
                            ASTNode::BinaryExpr {
                                left, op, right, ..
                            } => {
                                assert!(matches!(op, crate::frontend::token::TokenKind::LessThan));
                                assert!(matches!(&**left, ASTNode::Identifier(_, _)));
                                assert!(matches!(&**right, ASTNode::Identifier(_, _)));
                            }
                            _ => panic!("Expected binary condition"),
                        }
                        assert_eq!(body.len(), 1);
                        match &body[0] {
                            ASTNode::Block { statements, .. } => {
                                assert_eq!(statements.len(), 3);
                                match &statements[0] {
                                    ASTNode::VariableDecl { name, value, .. } => {
                                        assert_eq!(name, "i");
                                        assert!(matches!(&**value, ASTNode::Identifier(_, _)));
                                    }
                                    _ => panic!("Expected binding declaration"),
                                }
                                match &statements[1] {
                                    ASTNode::Block { statements, .. } => {
                                        assert_eq!(statements.len(), 1);
                                        match &statements[0] {
                                            ASTNode::CallExpr { callee, .. } => {
                                                assert_eq!(callee, "print");
                                            }
                                            _ => panic!("Expected call expression"),
                                        }
                                    }
                                    _ => panic!("Expected original block"),
                                }
                                match &statements[2] {
                                    ASTNode::Assignment { lhs, value, .. } => {
                                        assert!(matches!(&**lhs, ASTNode::Identifier(_, _)));
                                        match &**value {
                                            ASTNode::BinaryExpr { op, .. } => {
                                                assert!(matches!(
                                                    op,
                                                    crate::frontend::token::TokenKind::Plus
                                                ));
                                            }
                                            _ => panic!("Expected increment addition"),
                                        }
                                    }
                                    _ => panic!("Expected increment assignment"),
                                }
                            }
                            _ => panic!("Expected Block as while body"),
                        }
                    }
                    _ => panic!("Expected while statement"),
                }
            }
            _ => panic!("Expected top-level Block"),
        }
    }
}
