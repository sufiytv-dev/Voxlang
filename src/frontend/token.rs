// token.rs - Token specification for Voxlang
//
// Defines lexical tokens: keywords, operators, literals, and compiler directives.

use crate::frontend::span::Span;

/// Compiler directives prefixed with '@'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerDirective {
    Comptime, // @comptime – compile-time evaluation
    Kernel,   // @kernel   – GPU kernel entry
    Device,   // @device   – GPU device variable
    Lemma,    // @lemma    – Z3 verification lemma
}

#[derive(Debug, Clone, PartialEq)] // No Eq because of f64 in FloatLiteral
pub enum TokenKind {
    // Structural
    ScopeOpen,  // indentation increase
    ScopeClose, // explicit '}'
    Newline,
    EOF,
    ColonColon, // ::

    // Keywords
    Fn,
    If,
    While,
    Else,
    For,
    In,
    Return,
    Struct,
    Unsafe,
    Parallel,
    Import,
    As,
    Where,
    Enum,
    Type,
    Use,
    Match,
    Underscore,
    Let,
    Mut,
    Ref,
    Launch,     // GPU kernel launch keyword

    // Logical keywords
    And,
    Or,
    Not,

    // Directives
    Directive(CompilerDirective),

    // Literals and identifiers
    Identifier(String),
    StringLiteral(String),
    IntegerLiteral(i64),
    FloatLiteral(f64),
    CharLiteral(u32), // Unicode scalar

    // Punctuation
    Colon,
    Arrow,
    Comma,
    Dot,
    DotDot,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Assign,

    // Relational
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,

    // Arithmetic
    Plus,
    Minus,
    Star,
    Div,
    Mod,

    // Bitwise
    Ampersand,
    Pipe,
    Caret,
    Shl,
    Shr,

    // Error propagation / optional chaining
    Question,
}

/// A token with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
