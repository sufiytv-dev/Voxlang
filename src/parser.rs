// parser.rs - Parses token streams into an AST.
//
// Handles Voxlang syntax: functions, structs, enums, type aliases, use statements,
// control flow (`if`, `while`, `for`, `parallel`), pattern matching (`match`, `if let`, `while let`),
// and expressions with precedence. Blocks are delimited by `:` and closed by `}`.
//
// NEW: Kernel attribute `@kernel(block=(x,y,z))` and kernel launch syntax `launch name(...)(...)`.

use crate::diagnostic::{Diagnostic, emit_diagnostic, global_debug};
use crate::frontend::span::Span;
use crate::frontend::token::{CompilerDirective, Token, TokenKind};
use std::mem::discriminant;

// -----------------------------------------------------------------------------
// AST node definitions
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum HashAlgorithm {
    Sha256,
    Blake3,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HashValue {
    pub algorithm: HashAlgorithm,
    pub digest: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportSource {
    LocalPath(String),
    RemoteUrl(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    pub name: String,
    pub ty: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchPattern {
    UnitVariant {
        by_ref: bool,
        enum_name: String,
        variant: String,
        span: Span,
    },
    Wildcard(Span),
    Binding {
        by_ref: bool,
        enum_name: String,
        variant: String,
        bindings: Vec<String>,
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub body: Vec<ASTNode>,
    pub span: Span,
}

/// Kernel block configuration attribute.
#[derive(Debug, Clone, PartialEq)]
pub struct KernelAttr {
    pub block: (u32, u32, u32), // (x, y, z)
}

impl Default for KernelAttr {
    fn default() -> Self {
        Self {
            block: (256, 1, 1),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ASTNode {
    Program(Vec<ASTNode>, Span),

    Import {
        source: ImportSource,
        alias: Option<String>,
        hash: Option<HashValue>,
        span: Span,
    },

    StructDef {
        name: String,
        generic_params: Vec<String>,
        fields: Vec<StructField>,
        span: Span,
    },

    EnumDef {
        name: String,
        params: Vec<String>,
        variants: Vec<EnumVariant>,
        span: Span,
    },

    TypeAlias {
        name: String,
        target_type: String,
        span: Span,
    },

    UseDecl {
        path: Vec<String>,
        alias: Option<String>,
        is_glob: bool,
        span: Span,
    },

    FunctionDef {
        name: String,
        generic_params: Vec<String>,
        params: Vec<Param>,
        return_type: String,
        return_refinement: Option<Box<ASTNode>>,
        body: Vec<ASTNode>,
        span: Span,
    },

    KernelFn {
        name: String,
        params: Vec<Param>,
        body: Vec<ASTNode>,
        device_triple: String,
        attr: KernelAttr,      // NEW: store block dimensions
        span: Span,
    },

    KernelLaunch {
        kernel: Box<ASTNode>,                      // identifier of kernel
        grid: (Box<ASTNode>, Box<ASTNode>, Box<ASTNode>), // (x,y,z) expressions
        args: Vec<ASTNode>,                       // actual arguments to kernel
        span: Span,
    },

    IfStatement {
        condition: Box<ASTNode>,
        then_branch: Vec<ASTNode>,
        else_branch: Option<Vec<ASTNode>>,
        span: Span,
    },

    IfLetStatement {
        pattern: MatchPattern,
        expr: Box<ASTNode>,
        then_branch: Vec<ASTNode>,
        else_branch: Option<Vec<ASTNode>>,
        span: Span,
    },

    WhileStatement {
        condition: Box<ASTNode>,
        body: Vec<ASTNode>,
        span: Span,
    },

    WhileLetStatement {
        pattern: MatchPattern,
        expr: Box<ASTNode>,
        body: Vec<ASTNode>,
        span: Span,
    },

    ForLoop {
        iter_var: String,
        start: Box<ASTNode>,
        end: Box<ASTNode>,
        body: Vec<ASTNode>,
        span: Span,
    },

    ParallelLoop {
        iter_var: String,
        start: Box<ASTNode>,
        end: Box<ASTNode>,
        body: Vec<ASTNode>,
        span: Span,
    },

    ComptimeBlock {
        body: Vec<ASTNode>,
        span: Span,
    },

    ReturnStatement(Option<Box<ASTNode>>, Span),

    VariableDecl {
        name: String,
        ty: Option<String>,
        refinement: Option<Box<ASTNode>>,
        value: Box<ASTNode>,
        mutable: bool,
        span: Span,
    },

    DeviceVarDecl {
        name: String,
        ty: Option<String>,
        refinement: Option<Box<ASTNode>>,
        value: Box<ASTNode>,
        span: Span,
    },

    Assignment {
        lhs: Box<ASTNode>,
        value: Box<ASTNode>,
        span: Span,
    },

    BinaryExpr {
        left: Box<ASTNode>,
        op: TokenKind,
        right: Box<ASTNode>,
        span: Span,
    },

    UnaryExpr {
        op: TokenKind,
        expr: Box<ASTNode>,
        span: Span,
    },

    CastExpr {
        expr: Box<ASTNode>,
        target_type: String,
        span: Span,
    },

    CallExpr {
        callee: String,
        args: Vec<ASTNode>,
        span: Span,
    },

    StructLiteral {
        name: String,
        fields: Vec<(String, ASTNode)>,
        span: Span,
    },

    BorrowExpr {
        mutable: bool,
        expr: Box<ASTNode>,
        span: Span,
    },

    DerefExpr(Box<ASTNode>, Span),

    FieldAccess {
        expr: Box<ASTNode>,
        field: String,
        span: Span,
    },

    ArrayLiteral {
        elements: Vec<ASTNode>,
        span: Span,
    },

    ArrayIndex {
        array: Box<ASTNode>,
        index: Box<ASTNode>,
        span: Span,
    },

    SliceExpr {
        base: Box<ASTNode>,
        start: Option<Box<ASTNode>>,
        end: Option<Box<ASTNode>>,
        span: Span,
    },

    MatchExpr {
        value: Box<ASTNode>,
        arms: Vec<MatchArm>,
        span: Span,
    },

    TryExpr {
        expr: Box<ASTNode>,
        span: Span,
    },

    Identifier(String, Span),
    IntegerLiteral(i64, Span),
    FloatLiteral(f64, Span),
    CharLiteral(u32, Span),
    StringLiteral(String, Span),

    RefinedType {
        base: Box<ASTNode>,
        condition: Box<ASTNode>,
        span: Span,
    },

    Lemma {
        name: String,
        params: Vec<Param>,
        return_type: String,
        proof: Vec<ASTNode>,
        span: Span,
    },

    Block {
        statements: Vec<ASTNode>,
        span: Span,
    },

    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub refinement: Option<Box<ASTNode>>,
    pub span: Span,
}

pub struct Parser<'a> {
    pub tokens: &'a [Token],
    pub pos: usize,
    debug: bool,
    pub has_errors: bool,
    block_depth: usize,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens,
            pos: 0,
            debug: global_debug(),
            has_errors: false,
            block_depth: 0,
        }
    }

    pub fn set_debug(&mut self, enabled: bool) {
        self.debug = enabled;
    }

    pub fn has_errors(&self) -> bool {
        self.has_errors
    }

    fn debug_log(&self, msg: &str) {
        if self.debug {
            crate::diagnostic::debug_log(format!("DEBUG PARSER: {}", msg));
        }
    }

    fn emit_error(&mut self, diag: &Diagnostic) {
        self.has_errors = true;
        emit_diagnostic(diag);
    }

    fn parse_error(&mut self, message: &str, token: &Token, code: &str) -> ASTNode {
        self.emit_error(
            &Diagnostic::error(message)
                .with_code(code)
                .with_span(token.span),
        );
        ASTNode::Error
    }

    fn span_until(&self, start_span: Span, end_token: &Token) -> Span {
        Span::new(
            start_span.start,
            end_token.span.end,
            start_span.line,
            start_span.col,
        )
    }

    fn current_span(&self) -> Span {
        self.peek().map(|t| t.span).unwrap_or_else(Span::dummy)
    }

    fn skip_newlines(&mut self) {
        while self.match_token(TokenKind::Newline) {}
    }

    fn skip_to_block_end(&mut self) {
        let start_depth = self.block_depth;
        while !self.is_at_end() {
            if self.match_token(TokenKind::ScopeClose) {
                self.block_depth -= 1;
                if self.block_depth == start_depth - 1 {
                    break;
                }
            } else if self.match_token(TokenKind::ScopeOpen) {
                self.block_depth += 1;
            } else {
                self.advance();
            }
        }
    }

    pub fn parse(&mut self) -> ASTNode {
        self.debug_log("parse: start");
        let start_span = self.current_span();
        let mut nodes = Vec::new();
        while !self.is_at_end() {
            self.consume_newlines();
            while let Some(&TokenKind::ScopeClose) = self.peek_kind() {
                self.advance();
                self.consume_newlines();
            }
            if self.is_at_end() || matches!(self.peek_kind(), Some(&TokenKind::EOF)) {
                break;
            }
            match self.parse_statement() {
                ASTNode::Error => self.skip_to_statement_boundary(),
                node => nodes.push(node),
            }
            if self.block_depth != 0 {
                self.debug_log(&format!(
                    "Resetting block_depth from {} to 0",
                    self.block_depth
                ));
                self.block_depth = 0;
            }
            self.consume_newlines();
            while let Some(&TokenKind::ScopeClose) = self.peek_kind() {
                self.advance();
                self.consume_newlines();
            }
        }
        let end_span = self.current_span();
        self.debug_log(&format!("parse: done, {} nodes", nodes.len()));
        ASTNode::Program(
            nodes,
            self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        )
    }

    fn skip_to_statement_boundary(&mut self) {
        while !self.is_at_end() {
            match self.peek_kind() {
                Some(&TokenKind::Newline) => {
                    self.advance();
                    break;
                }
                Some(&TokenKind::ScopeClose) | Some(&TokenKind::EOF) => break,
                Some(&TokenKind::Fn)
                | Some(&TokenKind::Import)
                | Some(&TokenKind::Struct)
                | Some(&TokenKind::Enum)
                | Some(&TokenKind::Type)
                | Some(&TokenKind::Use)
                | Some(&TokenKind::Directive(_)) => break,
                _ => {
                    self.advance();
                }
            }
        }
        self.consume_newlines();
    }

    fn parse_statement(&mut self) -> ASTNode {
        self.consume_newlines();
        if self.is_at_end() || matches!(self.peek_kind(), Some(&TokenKind::EOF)) {
            return self.parse_error(
                "Unexpected end of file while parsing statement.",
                self.peek().unwrap(),
                "VX0100",
            );
        }

        // Detect missing closing brace: top-level keyword inside a block
        if self.block_depth > 0 {
            if let Some(kind) = self.peek_kind() {
                match kind {
                    TokenKind::Fn
                    | TokenKind::Struct
                    | TokenKind::Enum
                    | TokenKind::Import
                    | TokenKind::Type
                    | TokenKind::Use => {
                        let token = self.peek().unwrap();
                        let keyword = match kind {
                            TokenKind::Fn => "fn",
                            TokenKind::Struct => "struct",
                            TokenKind::Enum => "enum",
                            TokenKind::Import => "import",
                            TokenKind::Type => "type",
                            TokenKind::Use => "use",
                            _ => unreachable!(),
                        };
                        let msg = format!(
                            "Unexpected '{}' inside a block. Did you forget to close the previous block with '}}'?",
                            keyword
                        );
                        self.emit_error(
                            &Diagnostic::error(msg)
                                .with_code("VX9001")
                                .with_span(token.span),
                        );
                        self.skip_to_block_end();
                        return ASTNode::Error;
                    }
                    _ => {}
                }
            }
        }

        if let Some(&TokenKind::ScopeClose) = self.peek_kind() {
            self.advance();
            return ASTNode::Error;
        }

        if let Some(&TokenKind::Directive(directive)) = self.peek_kind() {
            self.advance();
            self.consume_newlines();
            let result = match directive {
                CompilerDirective::Kernel => {
                    // Parse optional attribute `(block=(x,y,z))`
                    let attr = self.parse_kernel_attribute();
                    if let ASTNode::FunctionDef {
                        name,
                        params,
                        body,
                        span: func_span,
                        ..
                    } = self.parse_function_definition()
                    {
                        ASTNode::KernelFn {
                            name,
                            params,
                            body,
                            device_triple: "nvptx64-nvidia-cuda".to_string(),
                            attr,
                            span: func_span,
                        }
                    } else {
                        self.parse_error(
                            "@kernel must be followed by a function definition",
                            self.peek().unwrap_or_else(|| self.tokens.last().unwrap()),
                            "VX0101",
                        )
                    }
                }
                CompilerDirective::Device => {
                    if let ASTNode::VariableDecl {
                        name,
                        ty,
                        refinement,
                        value,
                        span,
                        mutable,
                    } = self.parse_variable_declaration()
                    {
                        if mutable {
                            self.emit_error(
                                &Diagnostic::error("@device variables cannot be declared as `mut`")
                                    .with_code("VX0103")
                                    .with_span(span),
                            );
                            ASTNode::Error
                        } else {
                            ASTNode::DeviceVarDecl {
                                name,
                                ty,
                                refinement,
                                value,
                                span,
                            }
                        }
                    } else {
                        self.parse_error(
                            "@device must be followed by a variable declaration",
                            self.peek().unwrap_or_else(|| self.tokens.last().unwrap()),
                            "VX0102",
                        )
                    }
                }
                CompilerDirective::Lemma => self.parse_lemma(),
                _ => self.parse_error(
                    "Unsupported directive in statement context",
                    self.peek().unwrap(),
                    "VX0999",
                ),
            };
            if matches!(result, ASTNode::Error) {
                self.skip_to_statement_boundary();
            }
            return result;
        }

        // if let / while let lookahead
        if let Some(&TokenKind::If) = self.peek_kind() {
            let saved_pos = self.pos;
            self.advance();
            let is_if_let = self.match_token(TokenKind::Let);
            self.pos = saved_pos;
            if is_if_let {
                self.debug_log("parsing if-let statement");
                return self.parse_if_let_statement();
            }
        }
        if let Some(&TokenKind::While) = self.peek_kind() {
            let saved_pos = self.pos;
            self.advance();
            let is_while_let = self.match_token(TokenKind::Let);
            self.pos = saved_pos;
            if is_while_let {
                self.debug_log("parsing while-let statement");
                return self.parse_while_let_statement();
            }
        }

        if let Some(&TokenKind::For) = self.peek_kind() {
            self.debug_log("parsing for loop");
            return self.parse_for_loop();
        }

        match self.peek_kind() {
            Some(&TokenKind::Import) => {
                self.debug_log("parsing import");
                self.parse_import_statement()
            }
            Some(&TokenKind::Struct) => {
                self.debug_log("parsing struct");
                self.parse_struct_definition()
            }
            Some(&TokenKind::Enum) => {
                self.debug_log("parsing enum");
                self.parse_enum_definition()
            }
            Some(&TokenKind::Type) => {
                self.debug_log("parsing type alias");
                self.parse_type_alias()
            }
            Some(&TokenKind::Use) => {
                self.debug_log("parsing use");
                self.parse_use_statement()
            }
            Some(&TokenKind::Fn) => {
                self.debug_log("parsing function");
                self.parse_function_definition()
            }
            Some(&TokenKind::If) => {
                self.debug_log("parsing if");
                self.parse_if_statement()
            }
            Some(&TokenKind::While) => {
                self.debug_log("parsing while");
                self.parse_while_statement()
            }
            Some(&TokenKind::Parallel) => {
                self.debug_log("parsing parallel loop");
                self.parse_parallel_loop()
            }
            Some(&TokenKind::Return) => {
                self.debug_log("parsing return");
                self.parse_return_statement()
            }
            Some(&TokenKind::Let) => {
                self.debug_log("parsing let");
                self.parse_let_declaration()
            }
            _ => {
                self.debug_log("parsing expression or assignment");
                self.parse_expression_or_assignment()
            }
        }
    }

    /// Parse kernel attribute: `(block=(x,y,z))` or nothing.
    fn parse_kernel_attribute(&mut self) -> KernelAttr {
        if !self.match_token(TokenKind::LeftParen) {
            return KernelAttr::default();
        }
        self.skip_newlines();
        // Check if the next token is an identifier (any identifier)
        if !matches!(self.peek_kind(), Some(TokenKind::Identifier(_))) {
            self.emit_error(
                &Diagnostic::error("Expected 'block' in kernel attribute")
                    .with_code("VX0180")
                    .with_span(self.current_span()),
            );
            self.skip_to_expression_end();
            return KernelAttr::default();
        }
        let ident = self.advance().unwrap();
        if let TokenKind::Identifier(ref s) = ident.kind {
            if s != "block" {
                self.emit_error(
                    &Diagnostic::error(&format!("Expected 'block', found '{}'", s))
                        .with_code("VX0181")
                        .with_span(ident.span),
                );
                self.skip_to_expression_end();
                return KernelAttr::default();
            }
        } else {
            self.emit_error(
                &Diagnostic::error("Expected 'block' in kernel attribute")
                    .with_code("VX0182")
                    .with_span(ident.span),
            );
            self.skip_to_expression_end();
            return KernelAttr::default();
        }
        self.expect(TokenKind::Assign);
        self.expect(TokenKind::LeftParen);
        let x = self.parse_u32_literal();
        self.expect(TokenKind::Comma);
        let y = self.parse_u32_literal();
        self.expect(TokenKind::Comma);
        let z = self.parse_u32_literal();
        self.expect(TokenKind::RightParen);
        self.expect(TokenKind::RightParen);
        KernelAttr { block: (x, y, z) }
    }

    fn parse_u32_literal(&mut self) -> u32 {
        if let Some(&TokenKind::IntegerLiteral(val)) = self.peek_kind() {
            self.advance();
            if val < 0 || val > u32::MAX as i64 {
                self.emit_error(
                    &Diagnostic::error("Block dimension must be a positive 32-bit integer")
                        .with_code("VX0183")
                        .with_span(self.current_span()),
                );
                return 1;
            }
            val as u32
        } else {
            self.emit_error(
                &Diagnostic::error("Expected integer literal for block dimension")
                    .with_code("VX0184")
                    .with_span(self.current_span()),
            );
            1
        }
    }

    fn skip_to_expression_end(&mut self) {
        // Skip until we hit a token that ends an expression or statement boundary
        while !self.is_at_end() && !self.check(TokenKind::Newline) && !self.check(TokenKind::ScopeClose)
        {
            self.advance();
        }
    }

    fn parse_type_alias(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::Type);
        let name_token = self.expect_identifier();
        let name = match &name_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return self.parse_error("Expected identifier after 'type'", name_token, "VX0300"),
        };
        self.expect(TokenKind::Assign);
        let (target_type, _) = self.parse_type();
        self.expect_statement_end();
        let end_span = self.current_span();
        ASTNode::TypeAlias {
            name,
            target_type,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_use_statement(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::Use);

        let mut path = Vec::new();
        let mut is_glob = false;
        let mut alias = None;

        let first_token = self.expect_identifier();
        let first_seg = match &first_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return self.parse_error("Expected identifier in use path", first_token, "VX0301"),
        };
        path.push(first_seg);

        while self.match_token(TokenKind::ColonColon) {
            if let Some(&TokenKind::Star) = self.peek_kind() {
                self.advance();
                is_glob = true;
                break;
            }
            let seg_token = self.expect_identifier();
            let seg = match &seg_token.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => {
                    return self.parse_error(
                        "Expected identifier after '::' in use path",
                        seg_token,
                        "VX0302",
                    );
                }
            };
            path.push(seg);
        }

        if self.match_token(TokenKind::As) {
            let alias_token = self.expect_identifier();
            alias = match &alias_token.kind {
                TokenKind::Identifier(s) => Some(s.clone()),
                _ => {
                    return self.parse_error(
                        "Expected identifier after 'as'",
                        alias_token,
                        "VX0303",
                    );
                }
            };
        }

        if is_glob && alias.is_some() {
            let token = self.peek().unwrap_or_else(|| self.tokens.last().unwrap());
            self.emit_error(
                &Diagnostic::error("Glob import (`::*`) cannot have an alias")
                    .with_code("VX0304")
                    .with_span(token.span),
            );
            return ASTNode::Error;
        }

        self.expect_statement_end();
        let end_span = self.current_span();
        ASTNode::UseDecl {
            path,
            alias,
            is_glob,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_for_loop(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::For);
        let iter_token = self.expect_identifier();
        let iter_var = match &iter_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => {
                return self.parse_error(
                    "Expected loop variable after 'for'.",
                    iter_token,
                    "VX0300",
                );
            }
        };
        self.expect(TokenKind::In);
        let start = Box::new(self.parse_expression());
        self.expect(TokenKind::DotDot);
        let end = Box::new(self.parse_expression());
        self.expect(TokenKind::Colon);
        self.consume_newlines();
        self.block_depth += 1;
        let body_block = self.parse_block_node();
        let end_span = self.current_span();
        ASTNode::ForLoop {
            iter_var,
            start,
            end,
            body: vec![body_block],
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_if_let_statement(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::If);
        self.expect(TokenKind::Let);
        let pattern = match self.parse_match_pattern() {
            Some(p) => p,
            None => return ASTNode::Error,
        };
        self.expect(TokenKind::Assign);
        let expr = Box::new(self.parse_expression());
        self.expect(TokenKind::Colon);
        self.consume_newlines();
        self.block_depth += 1;
        let then_block = self.parse_block_node();
        let mut else_branch = None;
        self.consume_newlines();
        if self.match_token(TokenKind::Else) {
            self.expect(TokenKind::Colon);
            self.consume_newlines();
            self.block_depth += 1;
            else_branch = Some(vec![self.parse_block_node()]);
        }
        let end_span = self.current_span();
        ASTNode::IfLetStatement {
            pattern,
            expr,
            then_branch: vec![then_block],
            else_branch,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_while_let_statement(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::While);
        self.expect(TokenKind::Let);
        let pattern = match self.parse_match_pattern() {
            Some(p) => p,
            None => return ASTNode::Error,
        };
        self.expect(TokenKind::Assign);
        let expr = Box::new(self.parse_expression());
        self.expect(TokenKind::Colon);
        self.consume_newlines();
        self.block_depth += 1;
        let body_block = self.parse_block_node();
        let end_span = self.current_span();
        ASTNode::WhileLetStatement {
            pattern,
            expr,
            body: vec![body_block],
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_match_pattern(&mut self) -> Option<MatchPattern> {
        let by_ref = self.match_token(TokenKind::Ampersand);
        if self.match_token(TokenKind::Underscore) {
            return Some(MatchPattern::Wildcard(self.current_span()));
        }
        let first_tok = self.expect_identifier();
        let first_name = match &first_tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => {
                self.emit_error(
                    &Diagnostic::error("Expected enum/variant name in pattern")
                        .with_code("VX0144")
                        .with_span(first_tok.span),
                );
                return None;
            }
        };
        let mut segments = vec![first_name];
        while self.match_token(TokenKind::ColonColon) {
            let next_tok = self.expect_identifier();
            let seg = match &next_tok.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => {
                    self.emit_error(
                        &Diagnostic::error("Expected identifier after '::'")
                            .with_code("VX0145")
                            .with_span(next_tok.span),
                    );
                    return None;
                }
            };
            segments.push(seg);
        }
        let enum_name = if segments.len() == 1 {
            String::new()
        } else {
            segments[..segments.len() - 1].join("::")
        };
        let variant = segments.last().unwrap().clone();
        if self.match_token(TokenKind::LeftParen) {
            let mut bindings = Vec::new();
            loop {
                let ident = self.expect_identifier();
                let binding_name = match &ident.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    TokenKind::Underscore => "_".to_string(),
                    _ => {
                        self.emit_error(
                            &Diagnostic::error("Expected identifier or `_` in pattern binding")
                                .with_code("VX0146")
                                .with_span(ident.span),
                        );
                        return None;
                    }
                };
                bindings.push(binding_name);
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RightParen);
            Some(MatchPattern::Binding {
                by_ref,
                enum_name,
                variant,
                bindings,
                span: first_tok.span,
            })
        } else {
            Some(MatchPattern::UnitVariant {
                by_ref,
                enum_name,
                variant,
                span: first_tok.span,
            })
        }
    }

    fn parse_let_declaration(&mut self) -> ASTNode {
        self.expect(TokenKind::Let);
        self.parse_variable_declaration()
    }

    fn expect_string_literal(&mut self) -> (String, Span) {
        match self.peek() {
            Some(token) if matches!(token.kind, TokenKind::StringLiteral(_)) => {
                let t = self.advance().unwrap();
                (
                    match &t.kind {
                        TokenKind::StringLiteral(s) => s.clone(),
                        _ => unreachable!(),
                    },
                    t.span,
                )
            }
            _ => {
                let token = self.peek().unwrap_or_else(|| self.tokens.last().unwrap());
                self.emit_error(
                    &Diagnostic::error("Expected string literal")
                        .with_code("VX0818")
                        .with_span(token.span),
                );
                (String::new(), token.span)
            }
        }
    }

    fn parse_import_statement(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::Import);
        self.consume_newlines();
        let (path_str, path_span) = self.expect_string_literal();
        if path_str.is_empty() {
            return ASTNode::Error;
        }
        let is_remote = path_str.starts_with("http://")
            || path_str.starts_with("https://")
            || path_str.starts_with("github.com/")
            || path_str.starts_with("gitlab.com/");
        let source = if is_remote {
            ImportSource::RemoteUrl(path_str.clone())
        } else {
            ImportSource::LocalPath(path_str.clone())
        };
        let mut alias = None;
        if self.match_token(TokenKind::As) {
            let alias_token = self.expect_identifier();
            alias = match &alias_token.kind {
                TokenKind::Identifier(s) => Some(s.clone()),
                _ => {
                    return self.parse_error(
                        "Expected identifier after 'as'",
                        alias_token,
                        "VX0802",
                    );
                }
            };
        }
        let mut hash = None;
        if let Some(&TokenKind::Identifier(ref algo_name)) = self.peek_kind() {
            if algo_name == "sha256" || algo_name == "blake3" {
                let algo_token = self.advance().unwrap();
                if !self.match_token(TokenKind::Colon) {
                    return self.parse_error(
                        &format!("Expected ':' after {}", algo_name),
                        algo_token,
                        "VX0803",
                    );
                }
                let (digest, digest_span) = self.expect_string_literal();
                if digest.is_empty() {
                    return ASTNode::Error;
                }
                let algorithm = match algo_name.as_str() {
                    "sha256" => HashAlgorithm::Sha256,
                    "blake3" => HashAlgorithm::Blake3,
                    _ => unreachable!(),
                };
                let expected_len = match algorithm {
                    HashAlgorithm::Sha256 => 64,
                    HashAlgorithm::Blake3 => 128,
                };
                if digest.len() != expected_len {
                    return self.parse_error(
                        &format!(
                            "Invalid digest length for {}: expected {} hex chars, got {}",
                            algo_name,
                            expected_len,
                            digest.len()
                        ),
                        &Token {
                            kind: TokenKind::StringLiteral(digest.clone()),
                            span: digest_span,
                        },
                        "VX0805",
                    );
                }
                hash = Some(HashValue {
                    algorithm,
                    digest,
                    span: digest_span,
                });
            }
        }
        if is_remote && hash.is_none() {
            return self.parse_error(
                "Remote import requires a hash (e.g., sha256:\"...\")",
                &Token {
                    kind: TokenKind::StringLiteral(path_str),
                    span: path_span,
                },
                "VX0806",
            );
        }
        let end_span = self.current_span();
        self.expect_statement_end();
        ASTNode::Import {
            source,
            alias,
            hash,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_struct_definition(&mut self) -> ASTNode {
        let start_span = self.current_span();
        if self.match_token(TokenKind::Struct) {
        } else if let Some(TokenKind::Identifier(s)) = self.peek_kind() {
            if s == "struct" {
                self.advance();
            } else {
                return self.parse_error(
                    "Expected 'struct' keyword",
                    self.peek().unwrap(),
                    "VX0120",
                );
            }
        } else {
            return self.parse_error("Expected 'struct' keyword", self.peek().unwrap(), "VX0120");
        }

        let name_token = self.expect_identifier();
        let name = match &name_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => {
                return self.parse_error(
                    "Expected identifier after 'struct'",
                    name_token,
                    "VX0120",
                );
            }
        };

        let mut generic_params = Vec::new();
        if self.match_token(TokenKind::LessThan) {
            loop {
                let param_token = self.expect_identifier();
                let param_name = match &param_token.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => {
                        return self.parse_error(
                            "Expected type parameter name",
                            param_token,
                            "VX0149",
                        );
                    }
                };
                generic_params.push(param_name);
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::GreaterThan);
        }

        self.expect(TokenKind::Colon);
        self.consume_newlines();

        let mut fields = Vec::new();
        while !self.is_at_end() && !self.check(TokenKind::ScopeClose) {
            self.consume_newlines();
            if self.check(TokenKind::ScopeClose) {
                break;
            }
            let field_name_token = self.expect_identifier();
            let field_name = match &field_name_token.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => return self.parse_error("Expected field name", field_name_token, "VX0122"),
            };
            self.expect(TokenKind::Colon);
            let (ty, _) = self.parse_type();
            fields.push(StructField {
                name: field_name,
                ty,
                span: self.current_span(),
            });
            self.expect_statement_end();
        }
        if !self.match_token(TokenKind::ScopeClose) {
            return self.parse_error(
                "Expected '}' to close struct definition",
                self.peek().unwrap(),
                "VX0121",
            );
        }
        let end_span = self.current_span();
        ASTNode::StructDef {
            name,
            generic_params,
            fields,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_enum_definition(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::Enum);
        let name_token = self.expect_identifier();
        let name = match &name_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return self.parse_error("Expected identifier after 'enum'", name_token, "VX0130"),
        };

        let mut generic_params = Vec::new();
        if self.match_token(TokenKind::LessThan) {
            loop {
                let param_token = self.expect_identifier();
                let param_name = match &param_token.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => {
                        return self.parse_error(
                            "Expected type parameter name",
                            param_token,
                            "VX0147",
                        );
                    }
                };
                generic_params.push(param_name);
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::GreaterThan);
        }

        self.expect(TokenKind::Colon);
        self.consume_newlines();

        let mut variants = Vec::new();
        while !self.is_at_end() && !self.check(TokenKind::ScopeClose) {
            self.consume_newlines();
            if self.check(TokenKind::ScopeClose) {
                break;
            }
            let variant_token = self.expect_identifier();
            let variant_name = match &variant_token.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => return self.parse_error("Expected variant name", variant_token, "VX0132"),
            };
            variants.push(EnumVariant {
                name: variant_name,
                span: variant_token.span,
            });
            self.expect_statement_end();
        }
        if !self.match_token(TokenKind::ScopeClose) {
            return self.parse_error(
                "Expected '}' to close enum definition",
                self.peek().unwrap(),
                "VX0131",
            );
        }
        let end_span = self.current_span();
        ASTNode::EnumDef {
            name,
            params: generic_params,
            variants,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_lemma(&mut self) -> ASTNode {
        let start_span = self.current_span();
        let name_token = self.expect_identifier();
        let name = match &name_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => {
                return self.parse_error(
                    "Expected identifier for lemma name.",
                    name_token,
                    "VX0114",
                );
            }
        };
        self.expect(TokenKind::LeftParen);
        let params = self.parse_parameter_list();
        self.expect(TokenKind::RightParen);
        self.expect(TokenKind::Arrow);
        let return_type = self.parse_type().0;
        self.expect(TokenKind::Colon);
        self.consume_newlines();
        self.block_depth += 1;
        let proof = self.parse_block_stmts();
        let end_span = self.current_span();
        ASTNode::Lemma {
            name,
            params,
            return_type,
            proof,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_function_definition(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.debug_log("parse_function_definition: enter");
        self.expect(TokenKind::Fn);
        let name_token = self.expect_identifier();
        let name = match &name_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => {
                return self.parse_error(
                    "Expected identifier for function name.",
                    name_token,
                    "VX0104",
                );
            }
        };
        self.debug_log(&format!("function name = {}", name));

        let mut generic_params = Vec::new();
        if self.match_token(TokenKind::LessThan) {
            loop {
                let param_token = self.expect_identifier();
                let param_name = match &param_token.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => {
                        return self.parse_error(
                            "Expected type parameter name",
                            param_token,
                            "VX0148",
                        );
                    }
                };
                generic_params.push(param_name);
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::GreaterThan);
        }

        self.expect(TokenKind::LeftParen);
        let params = self.parse_parameter_list();
        self.expect(TokenKind::RightParen);
        let (return_type, return_refinement) = if self.match_token(TokenKind::Arrow) {
            self.parse_type()
        } else {
            ("void".to_string(), None)
        };
        self.expect(TokenKind::Colon);
        self.consume_newlines();
        self.block_depth += 1;
        let body = self.parse_block_stmts();
        self.debug_log(&format!("function body has {} statements", body.len()));
        let end_span = self.current_span();
        ASTNode::FunctionDef {
            name,
            generic_params,
            params,
            return_type,
            return_refinement,
            body,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_variable_declaration(&mut self) -> ASTNode {
        let start_span = self.current_span();
        let mutable = self.match_token(TokenKind::Mut);
        let name_token = self.expect_identifier();
        let name = match &name_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => {
                return self.parse_error(
                    "Expected identifier for variable name.",
                    name_token,
                    "VX0105",
                );
            }
        };

        let (ty, refinement) = if self.match_token(TokenKind::Colon) {
            self.parse_type()
        } else {
            (String::new(), None)
        };
        let ty_opt = if ty.is_empty() { None } else { Some(ty) };

        self.expect(TokenKind::Assign);
        let value = Box::new(self.parse_expression());
        let end_span = self.current_span();
        self.expect_statement_end();

        ASTNode::VariableDecl {
            name,
            ty: ty_opt,
            refinement,
            value,
            mutable,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_if_statement(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::If);
        let condition = Box::new(self.parse_expression());
        self.expect(TokenKind::Colon);
        self.consume_newlines();
        self.block_depth += 1;
        let then_block = self.parse_block_node();
        let mut else_branch = None;
        self.consume_newlines();
        if self.match_token(TokenKind::Else) {
            self.expect(TokenKind::Colon);
            self.consume_newlines();
            self.block_depth += 1;
            else_branch = Some(vec![self.parse_block_node()]);
        }
        let end_span = self.current_span();
        ASTNode::IfStatement {
            condition,
            then_branch: vec![then_block],
            else_branch,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_while_statement(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::While);
        let condition = Box::new(self.parse_expression());
        self.expect(TokenKind::Colon);
        self.consume_newlines();
        self.block_depth += 1;
        let body_block = self.parse_block_node();
        let end_span = self.current_span();
        ASTNode::WhileStatement {
            condition,
            body: vec![body_block],
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_parallel_loop(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::Parallel);
        self.expect(TokenKind::For);
        let iter_token = self.expect_identifier();
        let iter_var = match &iter_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => {
                return self.parse_error(
                    "Expected loop variable after 'for'.",
                    iter_token,
                    "VX0106",
                );
            }
        };
        self.expect(TokenKind::In);
        let start = Box::new(self.parse_expression());
        self.expect(TokenKind::DotDot);
        let end = Box::new(self.parse_expression());
        self.expect(TokenKind::Colon);
        self.consume_newlines();
        self.block_depth += 1;
        let body_block = self.parse_block_node();
        let end_span = self.current_span();
        ASTNode::ParallelLoop {
            iter_var,
            start,
            end,
            body: vec![body_block],
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_return_statement(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::Return);
        let value = if matches!(
            self.peek_kind(),
            Some(&TokenKind::Newline) | Some(&TokenKind::ScopeClose) | Some(&TokenKind::EOF)
        ) {
            None
        } else {
            self.skip_newlines();
            Some(Box::new(self.parse_expression()))
        };
        let end_span = self.current_span();
        self.expect_statement_end();
        ASTNode::ReturnStatement(
            value,
            self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        )
    }

    fn parse_expression_or_assignment(&mut self) -> ASTNode {
        let start_span = self.current_span();
        let lhs = self.parse_expression();
        if matches!(lhs, ASTNode::Error) {
            return lhs;
        }
        if self.match_token(TokenKind::Assign) {
            let value = Box::new(self.parse_expression());
            let end_span = self.current_span();
            self.expect_statement_end();
            return ASTNode::Assignment {
                lhs: Box::new(lhs),
                value,
                span: self.span_until(
                    start_span,
                    &Token {
                        kind: TokenKind::EOF,
                        span: end_span,
                    },
                ),
            };
        }
        self.expect_statement_end();
        lhs
    }

    fn parse_type(&mut self) -> (String, Option<Box<ASTNode>>) {
        let start_span = self.current_span();
        if self.match_token(TokenKind::LeftBracket) {
            if self.check(TokenKind::RightBracket) {
                self.advance();
                let (elem_ty, _) = self.parse_type();
                return (format!("[]{}", elem_ty), None);
            }
            let len = if let Some(&TokenKind::IntegerLiteral(val)) = self.peek_kind() {
                self.advance();
                val.to_string()
            } else if let Some(&TokenKind::Identifier(ref s)) = self.peek_kind() {
                if s == "?" {
                    self.advance();
                    "?".to_string()
                } else {
                    self.emit_error(
                        &Diagnostic::error("Expected integer length or '?' for array size")
                            .with_code("VX0123")
                            .with_span(self.current_span()),
                    );
                    "?".to_string()
                }
            } else {
                self.emit_error(
                    &Diagnostic::error("Expected integer length or '?' for array size")
                        .with_code("VX0123")
                        .with_span(self.current_span()),
                );
                "?".to_string()
            };
            let x_token = self.expect_identifier();
            if let TokenKind::Identifier(s) = &x_token.kind {
                if s != "x" {
                    self.emit_error(
                        &Diagnostic::error("Expected 'x' between array length and element type")
                            .with_code("VX0124")
                            .with_span(x_token.span),
                    );
                }
            }
            let (elem_ty, _) = self.parse_type();
            self.expect(TokenKind::RightBracket);
            return (format!("[{} x {}]", len, elem_ty), None);
        }
        if self.match_token(TokenKind::Ampersand) {
            let is_mut = self.match_token(TokenKind::Mut);
            if let Some(&TokenKind::Identifier(ref name)) = self.peek_kind() {
                if name == "str" {
                    self.advance();
                    let base = if is_mut {
                        "&mut str".to_string()
                    } else {
                        "&str".to_string()
                    };
                    if self.match_token(TokenKind::Where) {
                        let condition = Box::new(self.parse_expression());
                        let end_span = self.current_span();
                        let refined = ASTNode::RefinedType {
                            base: Box::new(ASTNode::Identifier(base.clone(), start_span)),
                            condition,
                            span: self.span_until(
                                start_span,
                                &Token {
                                    kind: TokenKind::EOF,
                                    span: end_span,
                                },
                            ),
                        };
                        return (base, Some(Box::new(refined)));
                    }
                    return (base, None);
                }
            }
            let (inner, _) = self.parse_base_type_generic();
            let base = if is_mut {
                format!("&mut {}", inner)
            } else {
                format!("& {}", inner)
            };
            if self.match_token(TokenKind::Where) {
                let condition = Box::new(self.parse_expression());
                let end_span = self.current_span();
                let refined = ASTNode::RefinedType {
                    base: Box::new(ASTNode::Identifier(base.clone(), start_span)),
                    condition,
                    span: self.span_until(
                        start_span,
                        &Token {
                            kind: TokenKind::EOF,
                            span: end_span,
                        },
                    ),
                };
                return (base, Some(Box::new(refined)));
            }
            return (base, None);
        }

        let (base, refinement) = self.parse_base_type_generic();
        if self.match_token(TokenKind::Where) {
            let condition = Box::new(self.parse_expression());
            let end_span = self.current_span();
            let refined = ASTNode::RefinedType {
                base: Box::new(ASTNode::Identifier(base.clone(), start_span)),
                condition,
                span: self.span_until(
                    start_span,
                    &Token {
                        kind: TokenKind::EOF,
                        span: end_span,
                    },
                ),
            };
            (base, Some(Box::new(refined)))
        } else {
            (base, refinement)
        }
    }

    fn parse_base_type_generic(&mut self) -> (String, Option<Box<ASTNode>>) {
        let ty_token = self.expect_identifier();
        let mut type_str = match &ty_token.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => {
                self.emit_error(
                    &Diagnostic::error("Expected type specifier.")
                        .with_code("VX0107")
                        .with_span(ty_token.span),
                );
                return ("i32".to_string(), None);
            }
        };
        if self.match_token(TokenKind::LessThan) {
            let mut args = Vec::new();
            loop {
                let (arg_ty, _) = self.parse_type();
                args.push(arg_ty);
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::GreaterThan);
            type_str = format!("{}<{}>", type_str, args.join(","));
        }
        while self.match_token(TokenKind::Star) {
            type_str.push('*');
        }
        (type_str, None)
    }

    fn parse_block_node(&mut self) -> ASTNode {
        let start_span = self.current_span();
        let statements = self.parse_block_stmts();
        let end_span = self.current_span();
        ASTNode::Block {
            statements,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_block_stmts(&mut self) -> Vec<ASTNode> {
        let start_depth = self.block_depth;
        self.debug_log(&format!("parse_block_stmts: enter, depth={}", start_depth));
        let mut statements = Vec::new();

        while !self.is_at_end() {
            self.consume_newlines();
            if self.is_at_end() {
                break;
            }
            if self.check(TokenKind::ScopeClose) {
                self.advance();
                self.block_depth -= 1;
                self.debug_log(&format!(
                    "parse_block_stmts: consumed '}}', new depth={}",
                    self.block_depth
                ));
                if self.block_depth == start_depth - 1 {
                    self.debug_log("parse_block_stmts: closed current block, breaking");
                    break;
                }
                continue;
            }
            match self.parse_statement() {
                ASTNode::Error => self.skip_to_statement_boundary(),
                stmt => {
                    statements.push(stmt);
                }
            }
        }
        self.debug_log(&format!(
            "parse_block_stmts: exit, {} statements",
            statements.len()
        ));
        statements
    }

    fn parse_expression(&mut self) -> ASTNode {
        self.parse_logical_or()
    }

    fn parse_logical_or(&mut self) -> ASTNode {
        let mut expr = self.parse_logical_and();
        while self.match_token(TokenKind::Or) {
            let right = self.parse_logical_and();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op: TokenKind::Or,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_logical_and(&mut self) -> ASTNode {
        let mut expr = self.parse_bitwise_or();
        while self.match_token(TokenKind::And) {
            let right = self.parse_bitwise_or();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op: TokenKind::And,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_bitwise_or(&mut self) -> ASTNode {
        let mut expr = self.parse_bitwise_xor();
        while self.match_token(TokenKind::Pipe) {
            let right = self.parse_bitwise_xor();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op: TokenKind::Pipe,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_bitwise_xor(&mut self) -> ASTNode {
        let mut expr = self.parse_bitwise_and();
        while self.match_token(TokenKind::Caret) {
            let right = self.parse_bitwise_and();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op: TokenKind::Caret,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_bitwise_and(&mut self) -> ASTNode {
        let mut expr = self.parse_shift();
        while self.match_token(TokenKind::Ampersand) {
            let right = self.parse_shift();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op: TokenKind::Ampersand,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_shift(&mut self) -> ASTNode {
        let mut expr = self.parse_comparison();
        loop {
            let op = if self.match_token(TokenKind::Shl) {
                TokenKind::Shl
            } else if self.match_token(TokenKind::Shr) {
                TokenKind::Shr
            } else {
                break;
            };
            let right = self.parse_comparison();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_comparison(&mut self) -> ASTNode {
        let mut expr = self.parse_additive();
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Equal) => TokenKind::Equal,
                Some(TokenKind::NotEqual) => TokenKind::NotEqual,
                Some(TokenKind::LessThan) => TokenKind::LessThan,
                Some(TokenKind::GreaterThan) => TokenKind::GreaterThan,
                Some(TokenKind::LessThanOrEqual) => TokenKind::LessThanOrEqual,
                Some(TokenKind::GreaterThanOrEqual) => TokenKind::GreaterThanOrEqual,
                _ => break,
            };
            self.advance();
            let right = self.parse_additive();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_additive(&mut self) -> ASTNode {
        let mut expr = self.parse_multiplicative();
        loop {
            let op = if self.match_token(TokenKind::Plus) {
                TokenKind::Plus
            } else if self.match_token(TokenKind::Minus) {
                TokenKind::Minus
            } else {
                break;
            };
            let right = self.parse_multiplicative();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_multiplicative(&mut self) -> ASTNode {
        let mut expr = self.parse_unary();
        loop {
            let op = if self.match_token(TokenKind::Star) {
                TokenKind::Star
            } else if self.match_token(TokenKind::Div) {
                TokenKind::Div
            } else if self.match_token(TokenKind::Mod) {
                TokenKind::Mod
            } else {
                break;
            };
            let right = self.parse_unary();
            let span = self.current_span();
            expr = ASTNode::BinaryExpr {
                left: Box::new(expr),
                op,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn parse_unary(&mut self) -> ASTNode {
        if self.match_token(TokenKind::Not) {
            let start_span = self.current_span();
            let expr = Box::new(self.parse_unary());
            let end_span = self.current_span();
            ASTNode::UnaryExpr {
                op: TokenKind::Not,
                expr,
                span: self.span_until(
                    start_span,
                    &Token {
                        kind: TokenKind::EOF,
                        span: end_span,
                    },
                ),
            }
        } else if self.match_token(TokenKind::Minus) {
            let start_span = self.current_span();
            let expr = Box::new(self.parse_unary());
            let end_span = self.current_span();
            ASTNode::UnaryExpr {
                op: TokenKind::Minus,
                expr,
                span: self.span_until(
                    start_span,
                    &Token {
                        kind: TokenKind::EOF,
                        span: end_span,
                    },
                ),
            }
        } else if self.match_token(TokenKind::Ampersand) {
            let start_span = self.current_span();
            let mutable = self.match_token(TokenKind::Mut);
            let expr = Box::new(self.parse_unary());
            let end_span = self.current_span();
            ASTNode::BorrowExpr {
                mutable,
                expr,
                span: self.span_until(
                    start_span,
                    &Token {
                        kind: TokenKind::EOF,
                        span: end_span,
                    },
                ),
            }
        } else if self.match_token(TokenKind::Star) {
            let start_span = self.current_span();
            let expr = Box::new(self.parse_unary());
            let end_span = self.current_span();
            ASTNode::DerefExpr(
                expr,
                self.span_until(
                    start_span,
                    &Token {
                        kind: TokenKind::EOF,
                        span: end_span,
                    },
                ),
            )
        } else {
            self.parse_cast()
        }
    }

    fn parse_cast(&mut self) -> ASTNode {
        let mut expr = self.parse_primary();
        while self.match_token(TokenKind::As) {
            let target_type = self.parse_type().0;
            let span = self.current_span();
            expr = ASTNode::CastExpr {
                expr: Box::new(expr),
                target_type,
                span,
            };
        }
        expr
    }

    fn parse_primary(&mut self) -> ASTNode {
        let expr = self.parse_atom();
        self.parse_postfix(expr)
    }

    fn parse_atom(&mut self) -> ASTNode {
        match self.peek_kind() {
            Some(&TokenKind::IntegerLiteral(_)) => {
                let t = self.advance().unwrap();
                ASTNode::IntegerLiteral(
                    if let TokenKind::IntegerLiteral(val) = t.kind {
                        val
                    } else {
                        unreachable!()
                    },
                    t.span,
                )
            }
            Some(&TokenKind::FloatLiteral(_)) => {
                let t = self.advance().unwrap();
                ASTNode::FloatLiteral(
                    if let TokenKind::FloatLiteral(val) = t.kind {
                        val
                    } else {
                        unreachable!()
                    },
                    t.span,
                )
            }
            Some(&TokenKind::CharLiteral(_)) => {
                let t = self.advance().unwrap();
                ASTNode::CharLiteral(
                    if let TokenKind::CharLiteral(val) = t.kind {
                        val
                    } else {
                        unreachable!()
                    },
                    t.span,
                )
            }
            Some(&TokenKind::StringLiteral(_)) => {
                let t = self.advance().unwrap();
                ASTNode::StringLiteral(
                    if let TokenKind::StringLiteral(s) = &t.kind {
                        s.clone()
                    } else {
                        unreachable!()
                    },
                    t.span,
                )
            }
            Some(&TokenKind::Match) => self.parse_match_expr(),
            Some(&TokenKind::Launch) => {
                // New kernel launch syntax: launch kernel_name(grid_x,grid_y,grid_z)(args...)
                let start_span = self.current_span();
                self.advance(); // consume 'launch'
                // Parse kernel name (identifier)
                let kernel_token = self.expect_identifier();
                let kernel_name = match &kernel_token.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => {
                        return self.parse_error(
                            "Expected kernel name after 'launch'",
                            kernel_token,
                            "VX0185",
                        );
                    }
                };
                let kernel = Box::new(ASTNode::Identifier(kernel_name, kernel_token.span));

                // Parse grid dimensions: ( expr , expr , expr )
                self.expect(TokenKind::LeftParen);
                let grid_x = Box::new(self.parse_expression());
                self.expect(TokenKind::Comma);
                let grid_y = Box::new(self.parse_expression());
                self.expect(TokenKind::Comma);
                let grid_z = Box::new(self.parse_expression());
                self.expect(TokenKind::RightParen);

                // Parse argument list: ( ... )
                self.expect(TokenKind::LeftParen);
                let mut args = Vec::new();
                if !self.check(TokenKind::RightParen) {
                    args.push(self.parse_expression());
                    while self.match_token(TokenKind::Comma) {
                        args.push(self.parse_expression());
                    }
                }
                self.expect(TokenKind::RightParen);

                let end_span = self.current_span();
                let span = self.span_until(
                    start_span,
                    &Token {
                        kind: TokenKind::EOF,
                        span: end_span,
                    },
                );
                ASTNode::KernelLaunch {
                    kernel,
                    grid: (grid_x, grid_y, grid_z),
                    args,
                    span,
                }
            }
            Some(&TokenKind::Identifier(_)) => {
                let mut segments = Vec::new();
                let first_tok = self.advance().unwrap();
                let name = match &first_tok.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => String::new(),
                };
                segments.push(name.clone());
                while self.match_token(TokenKind::ColonColon) {
                    let next_tok = self.expect_identifier();
                    let seg = match &next_tok.kind {
                        TokenKind::Identifier(s) => s.clone(),
                        _ => String::new(),
                    };
                    segments.push(seg);
                }
                ASTNode::Identifier(segments.join("::"), first_tok.span)
            }
            Some(&TokenKind::LeftParen) => {
                self.advance();
                self.skip_newlines();
                let expr = self.parse_expression();
                self.skip_newlines();
                self.expect(TokenKind::RightParen);
                expr
            }
            Some(&TokenKind::LeftBracket) => {
                let start_span = self.current_span();
                self.advance();
                let mut elements = Vec::new();
                self.skip_newlines();
                if !self.check(TokenKind::RightBracket) {
                    elements.push(self.parse_expression());
                    while self.match_token(TokenKind::Comma) {
                        self.skip_newlines();
                        elements.push(self.parse_expression());
                    }
                }
                self.skip_newlines();
                self.expect(TokenKind::RightBracket);
                let end_span = self.current_span();
                ASTNode::ArrayLiteral {
                    elements,
                    span: self.span_until(
                        start_span,
                        &Token {
                            kind: TokenKind::EOF,
                            span: end_span,
                        },
                    ),
                }
            }
            Some(&TokenKind::Directive(ref d)) => match d {
                CompilerDirective::Comptime => {
                    self.advance();
                    self.consume_newlines();
                    if self.match_token(TokenKind::Colon) {
                        self.consume_newlines();
                        self.block_depth += 1;
                        let body = self.parse_block_stmts();
                        let span = self.current_span();
                        ASTNode::ComptimeBlock { body, span }
                    } else {
                        let expr = self.parse_expression();
                        let span = expr.span();
                        ASTNode::ComptimeBlock {
                            body: vec![expr],
                            span,
                        }
                    }
                }
                _ => self.parse_error(
                    "Unsupported directive in expression",
                    self.peek().unwrap(),
                    "VX0999",
                ),
            },
            Some(&TokenKind::ScopeClose) => {
                let t = self.advance().unwrap();
                ASTNode::Identifier(String::new(), t.span)
            }
            _ => self.parse_error(
                "Invalid primary expression",
                self.peek().unwrap_or_else(|| self.tokens.last().unwrap()),
                "VX0109",
            ),
        }
    }

    fn parse_match_expr(&mut self) -> ASTNode {
        let start_span = self.current_span();
        self.expect(TokenKind::Match);
        let value = Box::new(self.parse_expression());
        self.expect(TokenKind::Colon);
        self.consume_newlines();

        let mut arms = Vec::new();

        while !self.is_at_end() && !self.check(TokenKind::ScopeClose) {
            self.consume_newlines();
            let pattern = match self.parse_match_pattern() {
                Some(p) => p,
                None => return ASTNode::Error,
            };
            self.expect(TokenKind::Arrow);
            self.consume_newlines();

            let body = if self.check(TokenKind::ScopeOpen) {
                self.advance();
                self.block_depth += 1;
                let stmts = self.parse_block_stmts();
                stmts
            } else {
                let expr = self.parse_expression();
                self.consume_newlines();
                vec![expr]
            };

            arms.push(MatchArm {
                pattern,
                body,
                span: self.current_span(),
            });
            self.consume_newlines();
            if self.check(TokenKind::ScopeClose) {
                break;
            }
        }

        if !self.match_token(TokenKind::ScopeClose) {
            return self.parse_error(
                "Expected '}' to close match expression",
                self.peek().unwrap(),
                "VX0143",
            );
        }

        let end_span = self.current_span();
        ASTNode::MatchExpr {
            value,
            arms,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_postfix(&mut self, mut expr: ASTNode) -> ASTNode {
        loop {
            if self.match_token(TokenKind::LeftParen) {
                let start_span = expr.span();
                self.skip_newlines();

                let is_named_struct_literal = if self.check(TokenKind::RightParen) {
                    false
                } else {
                    let saved_pos = self.pos;
                    let is_named = if let Some(id_token) = self.peek() {
                        if let TokenKind::Identifier(_) = id_token.kind {
                            let after_id_pos = self.pos + 1;
                            if let Some(next_token) = self.tokens.get(after_id_pos) {
                                matches!(next_token.kind, TokenKind::Colon)
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    self.pos = saved_pos;
                    is_named
                };

                if is_named_struct_literal {
                    let struct_name = match &expr {
                        ASTNode::Identifier(name, _) => name.clone(),
                        _ => {
                            return self.parse_error(
                                "Struct literal must be called on an identifier",
                                self.peek().unwrap(),
                                "VX0125",
                            );
                        }
                    };
                    expr = self.parse_named_struct_literal(struct_name, start_span);
                } else {
                    let mut args = Vec::new();
                    if let ASTNode::FieldAccess {
                        expr: receiver,
                        field,
                        span: _,
                    } = &expr
                    {
                        args.push((**receiver).clone());
                        while self.match_token(TokenKind::Comma) {
                            self.skip_newlines();
                            args.push(self.parse_expression());
                            self.skip_newlines();
                        }
                        self.skip_newlines();
                        self.expect(TokenKind::RightParen);
                        let callee = field.clone();
                        expr = ASTNode::CallExpr {
                            callee,
                            args,
                            span: self.span_until(start_span, self.peek().unwrap()),
                        };
                    } else {
                        if !self.check(TokenKind::RightParen) {
                            args.push(self.parse_expression());
                            while self.match_token(TokenKind::Comma) {
                                self.skip_newlines();
                                args.push(self.parse_expression());
                            }
                        }
                        self.skip_newlines();
                        self.expect(TokenKind::RightParen);
                        let callee = match &expr {
                            ASTNode::Identifier(name, _) => name.clone(),
                            _ => {
                                self.parse_error(
                                    "Expected function name",
                                    self.peek().unwrap(),
                                    "VX0116",
                                );
                                String::new()
                            }
                        };
                        expr = ASTNode::CallExpr {
                            callee,
                            args,
                            span: self.span_until(start_span, self.peek().unwrap()),
                        };
                    }
                }
            } else if self.match_token(TokenKind::Dot) {
                let start_span = expr.span();
                let field_token = self.expect_identifier();
                let field = match &field_token.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => {
                        self.parse_error("Expected field name after '.'", field_token, "VX0117");
                        String::new()
                    }
                };
                let end_span = self.current_span();

                // Keep the old kernel.launch syntax for compatibility (optional)
                if field == "launch" && self.match_token(TokenKind::LeftParen) {
                    // Parse grid dimensions: (expr, expr, expr)
                    self.skip_newlines();
                    let grid_x = Box::new(self.parse_expression());
                    self.skip_newlines();
                    self.expect(TokenKind::Comma);
                    self.skip_newlines();
                    let grid_y = Box::new(self.parse_expression());
                    self.skip_newlines();
                    self.expect(TokenKind::Comma);
                    self.skip_newlines();
                    let grid_z = Box::new(self.parse_expression());
                    self.skip_newlines();
                    self.expect(TokenKind::RightParen);
                    // Now parse the argument list: (args...)
                    self.skip_newlines();
                    self.expect(TokenKind::LeftParen);
                    let mut args = Vec::new();
                    self.skip_newlines();
                    if !self.check(TokenKind::RightParen) {
                        args.push(self.parse_expression());
                        while self.match_token(TokenKind::Comma) {
                            self.skip_newlines();
                            args.push(self.parse_expression());
                        }
                    }
                    self.skip_newlines();
                    self.expect(TokenKind::RightParen);
                    expr = ASTNode::KernelLaunch {
                        kernel: Box::new(expr),
                        grid: (grid_x, grid_y, grid_z),
                        args,
                        span: self.span_until(start_span, self.peek().unwrap()),
                    };
                } else {
                    expr = ASTNode::FieldAccess {
                        expr: Box::new(expr),
                        field,
                        span: self.span_until(
                            start_span,
                            &Token {
                                kind: TokenKind::EOF,
                                span: end_span,
                            },
                        ),
                    };
                }
            } else if self.match_token(TokenKind::LeftBracket) {
                let start_span = expr.span();
                self.skip_newlines();

                let first =
                    if !self.check(TokenKind::DotDot) && !self.check(TokenKind::RightBracket) {
                        Some(Box::new(self.parse_expression()))
                    } else {
                        None
                    };
                self.skip_newlines();

                if self.match_token(TokenKind::DotDot) {
                    self.skip_newlines();
                    let second = if !self.check(TokenKind::RightBracket) {
                        Some(Box::new(self.parse_expression()))
                    } else {
                        None
                    };
                    self.skip_newlines();
                    self.expect(TokenKind::RightBracket);
                    expr = ASTNode::SliceExpr {
                        base: Box::new(expr),
                        start: first,
                        end: second,
                        span: self.span_until(start_span, self.peek().unwrap()),
                    };
                } else {
                    let index = match first {
                        Some(expr) => expr,
                        None => {
                            return self.parse_error(
                                "Expected expression inside array index brackets",
                                self.peek().unwrap(),
                                "VX0118",
                            );
                        }
                    };
                    self.expect(TokenKind::RightBracket);
                    expr = ASTNode::ArrayIndex {
                        array: Box::new(expr),
                        index,
                        span: self.span_until(start_span, self.peek().unwrap()),
                    };
                }
            } else if self.match_token(TokenKind::Question) {
                let span = self.current_span();
                expr = ASTNode::TryExpr {
                    expr: Box::new(expr),
                    span,
                };
            } else {
                break;
            }
        }
        expr
    }

    fn parse_named_struct_literal(&mut self, struct_name: String, start_span: Span) -> ASTNode {
        let mut fields = Vec::new();
        self.skip_newlines();
        while !self.check(TokenKind::RightParen) {
            let field_name_token = self.expect_identifier();
            let field_name = match &field_name_token.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => {
                    return self.parse_error(
                        "Expected field name in struct literal",
                        field_name_token,
                        "VX0126",
                    );
                }
            };
            self.expect(TokenKind::Colon);
            self.skip_newlines();
            let value = self.parse_expression();
            fields.push((field_name, value));
            self.skip_newlines();
            if !self.match_token(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RightParen);
        let end_span = self.current_span();
        ASTNode::StructLiteral {
            name: struct_name,
            fields,
            span: self.span_until(
                start_span,
                &Token {
                    kind: TokenKind::EOF,
                    span: end_span,
                },
            ),
        }
    }

    fn parse_parameter_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        if !matches!(self.peek_kind(), Some(&TokenKind::RightParen)) {
            loop {
                let name_token = self.expect_identifier();
                self.expect(TokenKind::Colon);
                let (ty, refinement) = self.parse_type();
                let name = match &name_token.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => {
                        self.emit_error(
                            &Diagnostic::error("Expected identifier in parameter list.")
                                .with_code("VX0110")
                                .with_span(name_token.span),
                        );
                        "".to_string()
                    }
                };
                params.push(Param {
                    name,
                    ty,
                    refinement,
                    span: self.current_span(),
                });
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
        }
        params
    }

    fn expect_identifier(&mut self) -> &'a Token {
        match self.peek() {
            Some(t)
                if discriminant(&t.kind) == discriminant(&TokenKind::Identifier(String::new())) =>
            {
                self.advance().unwrap()
            }
            Some(t) if matches!(t.kind, TokenKind::Underscore) => self.advance().unwrap(),
            _ => {
                let token = self.peek().unwrap_or_else(|| self.tokens.last().unwrap());
                self.emit_error(
                    &Diagnostic::error("Expected identifier.")
                        .with_code("VX0111")
                        .with_span(token.span),
                );
                token
            }
        }
    }

    fn expect(&mut self, expected: TokenKind) -> &'a Token {
        match self.peek() {
            Some(t) if discriminant(&t.kind) == discriminant(&expected) => self.advance().unwrap(),
            _ => {
                let token = self.peek().unwrap_or_else(|| self.tokens.last().unwrap());
                self.emit_error(
                    &Diagnostic::error(&format!("Expected {:?}, found {:?}", expected, token.kind))
                        .with_code("VX0112")
                        .with_span(token.span),
                );
                token
            }
        }
    }

    fn expect_statement_end(&mut self) {
        if self.match_token(TokenKind::Newline) {
            return;
        }
        if matches!(
            self.peek_kind(),
            Some(&TokenKind::EOF) | Some(&TokenKind::ScopeClose)
        ) {
            return;
        }
        if self.is_statement_start() {
            return;
        }
        let token = self.peek().unwrap();
        self.emit_error(
            &Diagnostic::error("Statement termination expected (newline, '}', or EOF).")
                .with_code("VX0113")
                .with_span(token.span),
        );
    }

    fn match_token(&mut self, kind: TokenKind) -> bool {
        if let Some(k) = self.peek_kind() {
            if discriminant(k) == discriminant(&kind) {
                self.advance();
                return true;
            }
        }
        false
    }

    fn consume_newlines(&mut self) {
        while self.match_token(TokenKind::Newline) {}
    }

    fn check(&self, kind: TokenKind) -> bool {
        matches!(self.peek_kind(), Some(k) if discriminant(k) == discriminant(&kind))
    }

    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn peek_kind(&self) -> Option<&'a TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    fn advance(&mut self) -> Option<&'a Token> {
        if self.pos < self.tokens.len() {
            let t = &self.tokens[self.pos];
            self.debug_log(&format!("advance: {:?}", t.kind));
            self.pos += 1;
            Some(t)
        } else {
            None
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || matches!(self.peek_kind(), Some(&TokenKind::EOF))
    }

    fn is_statement_start(&self) -> bool {
        matches!(
            self.peek_kind(),
            Some(TokenKind::Import)
                | Some(TokenKind::Struct)
                | Some(TokenKind::Enum)
                | Some(TokenKind::Type)
                | Some(TokenKind::Use)
                | Some(TokenKind::Fn)
                | Some(TokenKind::If)
                | Some(TokenKind::While)
                | Some(TokenKind::Parallel)
                | Some(TokenKind::Return)
                | Some(TokenKind::Let)
                | Some(TokenKind::For)
                | Some(TokenKind::Directive(_))
        )
    }
}

impl ASTNode {
    pub fn span(&self) -> Span {
        match self {
            ASTNode::Program(_, s) => *s,
            ASTNode::Import { span, .. } => *span,
            ASTNode::StructDef { span, .. } => *span,
            ASTNode::EnumDef { span, .. } => *span,
            ASTNode::TypeAlias { span, .. } => *span,
            ASTNode::UseDecl { span, .. } => *span,
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
            ASTNode::Identifier(_, s) => *s,
            ASTNode::IntegerLiteral(_, s) => *s,
            ASTNode::FloatLiteral(_, s) => *s,
            ASTNode::CharLiteral(_, s) => *s,
            ASTNode::StringLiteral(_, s) => *s,
            ASTNode::RefinedType { span, .. } => *span,
            ASTNode::Lemma { span, .. } => *span,
            ASTNode::Block { span, .. } => *span,
            ASTNode::Error => Span::dummy(),
        }
    }
}
