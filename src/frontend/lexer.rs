// lexer.rs - Lexical analysis for Voxlang.
//
// Converts source text into a sequence of tokens. Handles literals, keywords,
// operators, and compiler directives. Uses indentation only for readability;
// explicit '}' terminates blocks. Tabs are rejected.

use crate::diagnostic::{Diagnostic, Suggestion, debug_log, emit_diagnostic};
use crate::frontend::span::Span;
use crate::frontend::token::{CompilerDirective, Token, TokenKind};
use std::str;

pub struct Lexer<'a> {
    source: &'a str,
    byte_index: usize,
    current_line: usize,
    current_col: usize,
    line_start_byte: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        let source = source.trim_start_matches('\u{feff}');
        Self {
            source,
            byte_index: 0,
            current_line: 1,
            current_col: 1,
            line_start_byte: 0,
        }
    }

    fn current_char(&self) -> Option<char> {
        self.source[self.byte_index..].chars().next()
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.byte_index..].chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek_char()?;
        self.byte_index += c.len_utf8();
        self.current_col += 1;
        debug_log(format!("[LEX] advance: consumed '{}'", c.escape_default()));
        Some(c)
    }

    fn make_span(&self) -> Span {
        Span {
            start: self.byte_index,
            end: self.byte_index,
            line: self.current_line,
            col: self.current_col,
        }
    }

    fn make_span_from(&self, start: usize, line: usize, col: usize) -> Span {
        Span {
            start,
            end: self.byte_index,
            line,
            col,
        }
    }

    /// Skip spaces at line start; reject tabs.
    fn skip_leading_spaces(&mut self) -> Result<(), ()> {
        while let Some(c) = self.current_char() {
            match c {
                ' ' => {
                    self.advance();
                }
                '\t' => {
                    let span = Span {
                        start: self.byte_index,
                        end: self.byte_index + 1,
                        line: self.current_line,
                        col: self.current_col,
                    };
                    emit_diagnostic(
                        &Diagnostic::error("Tabs are not allowed (use spaces).")
                            .with_code("VX0001")
                            .with_span(span)
                            .with_suggestion(Suggestion {
                                message: "Replace tabs with spaces.".to_string(),
                                span: Some(span),
                            }),
                    );
                    return Err(());
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Skip from '#' to end of line (inline comment). Returns true if a comment was skipped.
    fn skip_inline_comment(&mut self) -> bool {
        if self.current_char() == Some('#') {
            while let Some(c) = self.current_char() {
                if c == '\n' || c == '\r' {
                    break;
                }
                self.advance();
            }
            true
        } else {
            false
        }
    }

    fn handle_newline(&mut self, tokens: &mut Vec<Token>) -> Result<(), ()> {
        debug_log("[LEX] handle_newline called");
        let start_pos = self.byte_index;
        let start_line = self.current_line;
        let start_col = self.current_col;
        self.advance(); // consume '\n'
        let span = self.make_span_from(start_pos, start_line, start_col);
        tokens.push(Token {
            kind: TokenKind::Newline,
            span,
        });

        self.line_start_byte = self.byte_index;
        self.current_line += 1;
        self.current_col = 1;
        Ok(())
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, ()> {
        let mut tokens = Vec::new();

        while let Some(c) = self.current_char() {
            if c == '\r' {
                self.advance();
                continue;
            }
            if c == '\n' {
                self.handle_newline(&mut tokens)?;
                continue;
            }

            // At line start, skip leading spaces (no semantic meaning)
            if self.current_col == 1 {
                self.skip_leading_spaces()?;
            }

            self.tokenize_line(&mut tokens)?;
        }

        tokens.push(Token {
            kind: TokenKind::EOF,
            span: self.make_span(),
        });
        Ok(tokens)
    }

    fn tokenize_line(&mut self, tokens: &mut Vec<Token>) -> Result<(), ()> {
        debug_log(format!("[LEX] tokenize_line line {}", self.current_line));

        while let Some(c) = self.current_char() {
            if c == '\r' {
                self.advance();
                continue;
            }
            if c == '\n' {
                break;
            }
            if c.is_whitespace() {
                self.advance();
                continue;
            }

            // Inline comment skips to end of line
            if c == '#' {
                self.skip_inline_comment();
                break;
            }

            let start_pos = self.byte_index;
            let start_line = self.current_line;
            let start_col = self.current_col;

            // --- Identifiers and keywords ---
            if c.is_alphabetic() || c == '_' {
                let mut ident = String::new();
                while let Some(ch) = self.current_char() {
                    if ch == '\r' {
                        self.advance();
                        continue;
                    }
                    if ch.is_alphanumeric() || ch == '_' {
                        ident.push(ch);
                        self.advance();
                    } else {
                        break;
                    }
                }
                let kind = match ident.as_str() {
                    "struct" => TokenKind::Struct,
                    "enum" => TokenKind::Enum,
                    "type" => TokenKind::Type,
                    "use" => TokenKind::Use,
                    "match" => TokenKind::Match,
                    "fn" => TokenKind::Fn,
                    "if" => TokenKind::If,
                    "while" => TokenKind::While,
                    "else" => TokenKind::Else,
                    "import" => TokenKind::Import,
                    "return" => TokenKind::Return,
                    "unsafe" => TokenKind::Unsafe,
                    "parallel" => TokenKind::Parallel,
                    "for" => TokenKind::For,
                    "in" => TokenKind::In,
                    "ref" => TokenKind::Ref,
                    "mut" => TokenKind::Mut,
                    "as" => TokenKind::As,
                    "where" => TokenKind::Where,
                    "let" => TokenKind::Let,
                    "and" => TokenKind::And,
                    "or" => TokenKind::Or,
                    "not" => TokenKind::Not,
                    "launch" => TokenKind::Launch,
                    "_" => TokenKind::Underscore,
                    _ => TokenKind::Identifier(ident),
                };
                let span = self.make_span_from(start_pos, start_line, start_col);
                tokens.push(Token { kind, span });
                continue;
            }

            // --- Numeric literals ---
            if c.is_digit(10) {
                let mut number_str = String::new();
                let mut is_float = false;

                while let Some(ch) = self.current_char() {
                    if ch == '\r' {
                        self.advance();
                        continue;
                    }
                    if ch.is_digit(10) {
                        number_str.push(ch);
                        self.advance();
                    } else {
                        break;
                    }
                }

                if let Some('.') = self.current_char() {
                    let temp_index = self.byte_index;
                    if let Some(next) = self.source[temp_index + 1..].chars().next() {
                        if next.is_digit(10) {
                            is_float = true;
                            number_str.push('.');
                            self.advance();
                            while let Some(ch) = self.current_char() {
                                if ch == '\r' {
                                    self.advance();
                                    continue;
                                }
                                if ch.is_digit(10) {
                                    number_str.push(ch);
                                    self.advance();
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                }

                if let Some('e') | Some('E') = self.current_char() {
                    is_float = true;
                    number_str.push('e');
                    self.advance();
                    if let Some('+') | Some('-') = self.current_char() {
                        number_str.push(self.current_char().unwrap());
                        self.advance();
                    }
                    while let Some(ch) = self.current_char() {
                        if ch == '\r' {
                            self.advance();
                            continue;
                        }
                        if ch.is_digit(10) {
                            number_str.push(ch);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }

                let span = self.make_span_from(start_pos, start_line, start_col);
                if is_float {
                    match number_str.parse::<f64>() {
                        Ok(v) => tokens.push(Token {
                            kind: TokenKind::FloatLiteral(v),
                            span,
                        }),
                        Err(_) => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Invalid float literal '{}'",
                                    number_str
                                ))
                                .with_code("VX0012")
                                .with_span(span),
                            );
                            return Err(());
                        }
                    }
                } else {
                    match number_str.parse::<i64>() {
                        Ok(v) => tokens.push(Token {
                            kind: TokenKind::IntegerLiteral(v),
                            span,
                        }),
                        Err(_) => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!(
                                    "Invalid integer literal '{}'",
                                    number_str
                                ))
                                .with_code("VX0002")
                                .with_span(span),
                            );
                            return Err(());
                        }
                    }
                }
                continue;
            }

            // --- String literal ---
            if c == '"' {
                self.advance();
                let mut content = String::new();
                while let Some(ch) = self.current_char() {
                    if ch == '"' {
                        self.advance();
                        break;
                    }
                    if ch == '\\' {
                        self.advance();
                        let esc = match self.current_char() {
                            Some(e) => e,
                            None => {
                                let span = self.make_span();
                                emit_diagnostic(
                                    &Diagnostic::error("Unterminated escape sequence")
                                        .with_code("VX0020")
                                        .with_span(span),
                                );
                                return Err(());
                            }
                        };
                        let escaped = match esc {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            '\\' => '\\',
                            '"' => '"',
                            '\'' => '\'',
                            _ => {
                                let span = self.make_span();
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Invalid escape sequence '\\{}'",
                                        esc
                                    ))
                                    .with_code("VX0013")
                                    .with_span(span),
                                );
                                return Err(());
                            }
                        };
                        content.push(escaped);
                        self.advance();
                    } else {
                        content.push(ch);
                        self.advance();
                    }
                }
                let span = self.make_span_from(start_pos, start_line, start_col);
                tokens.push(Token {
                    kind: TokenKind::StringLiteral(content),
                    span,
                });
                continue;
            }

            // --- Character literal ---
            if c == '\'' {
                self.advance();
                let mut char_str = String::new();
                let mut escaped = false;
                let mut closed = false;

                while let Some(ch) = self.current_char() {
                    if ch == '\r' {
                        self.advance();
                        continue;
                    }
                    if escaped {
                        let esc_char = match ch {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            '\\' => '\\',
                            '\'' => '\'',
                            '"' => '"',
                            _ => {
                                let span = self.make_span();
                                emit_diagnostic(
                                    &Diagnostic::error(&format!(
                                        "Invalid escape sequence '\\{}'",
                                        ch
                                    ))
                                    .with_code("VX0013")
                                    .with_span(span),
                                );
                                return Err(());
                            }
                        };
                        char_str.push(esc_char);
                        self.advance();
                        escaped = false;
                        continue;
                    }
                    if ch == '\\' {
                        self.advance();
                        escaped = true;
                        continue;
                    }
                    if ch == '\'' {
                        self.advance();
                        closed = true;
                        break;
                    }
                    char_str.push(ch);
                    self.advance();
                }

                let span = self.make_span_from(start_pos, start_line, start_col);
                if !closed {
                    emit_diagnostic(
                        &Diagnostic::error("Unterminated character literal")
                            .with_code("VX0014")
                            .with_span(span),
                    );
                    return Err(());
                }
                if char_str.chars().count() != 1 {
                    emit_diagnostic(
                        &Diagnostic::error("Character literal must contain exactly one character")
                            .with_code("VX0015")
                            .with_span(span),
                    );
                    return Err(());
                }
                let ch = char_str.chars().next().unwrap();
                tokens.push(Token {
                    kind: TokenKind::CharLiteral(ch as u32),
                    span,
                });
                continue;
            }

            // --- Multi-character operators and punctuation ---
            match c {
                '/' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::Div,
                        span,
                    });
                }
                '%' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::Mod,
                        span,
                    });
                }
                '^' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::Caret,
                        span,
                    });
                }
                '&' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some('&') = self.current_char() {
                        emit_diagnostic(
                            &Diagnostic::error("Use 'and' keyword instead of '&&'")
                                .with_code("VX0016")
                                .with_span(span)
                                .with_suggestion(Suggestion {
                                    message: "Write 'and' for logical AND.".to_string(),
                                    span: Some(span),
                                }),
                        );
                        return Err(());
                    }
                    tokens.push(Token {
                        kind: TokenKind::Ampersand,
                        span,
                    });
                }
                '|' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some('|') = self.current_char() {
                        emit_diagnostic(
                            &Diagnostic::error("Use 'or' keyword instead of '||'")
                                .with_code("VX0017")
                                .with_span(span)
                                .with_suggestion(Suggestion {
                                    message: "Write 'or' for logical OR.".to_string(),
                                    span: Some(span),
                                }),
                        );
                        return Err(());
                    }
                    tokens.push(Token {
                        kind: TokenKind::Pipe,
                        span,
                    });
                }
                '.' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some('.') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::DotDot,
                            span,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Dot,
                            span,
                        });
                    }
                }
                '<' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some('<') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Shl,
                            span,
                        });
                    } else if let Some('=') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::LessThanOrEqual,
                            span,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::LessThan,
                            span,
                        });
                    }
                }
                '>' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some('>') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Shr,
                            span,
                        });
                    } else if let Some('=') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::GreaterThanOrEqual,
                            span,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::GreaterThan,
                            span,
                        });
                    }
                }
                '=' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some('=') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Equal,
                            span,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Assign,
                            span,
                        });
                    }
                }
                '!' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some('=') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::NotEqual,
                            span,
                        });
                    } else {
                        emit_diagnostic(
                            &Diagnostic::error(
                                "Unexpected '!' (use 'not' keyword for logical NOT)",
                            )
                            .with_code("VX0018")
                            .with_span(span)
                            .with_suggestion(Suggestion {
                                message: "Write 'not' for logical NOT, or '!=' for not equal."
                                    .to_string(),
                                span: Some(span),
                            }),
                        );
                        return Err(());
                    }
                }
                '+' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::Plus,
                        span,
                    });
                }
                '-' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some('>') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Arrow,
                            span,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Minus,
                            span,
                        });
                    }
                }
                '*' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::Star,
                        span,
                    });
                }
                ':' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    if let Some(':') = self.current_char() {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::ColonColon,
                            span,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Colon,
                            span,
                        });
                    }
                }
                ',' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::Comma,
                        span,
                    });
                }
                '(' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::LeftParen,
                        span,
                    });
                }
                ')' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::RightParen,
                        span,
                    });
                }
                '[' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::LeftBracket,
                        span,
                    });
                }
                ']' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::RightBracket,
                        span,
                    });
                }
                '{' => {
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    emit_diagnostic(
                        &Diagnostic::error(
                            "Explicit opening brace '{' is not allowed; use indentation and '}' to close.",
                        )
                        .with_code("VX0010")
                        .with_span(span),
                    );
                    return Err(());
                }
                '}' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::ScopeClose,
                        span,
                    });
                }
                '?' => {
                    self.advance();
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    tokens.push(Token {
                        kind: TokenKind::Question,
                        span,
                    });
                }
                '@' => {
                    self.advance();
                    let mut ident = String::new();
                    while let Some(ch) = self.current_char() {
                        if ch == '\r' {
                            self.advance();
                            continue;
                        }
                        if ch.is_alphanumeric() || ch == '_' {
                            ident.push(ch);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    let directive = match ident.as_str() {
                        "comptime" => CompilerDirective::Comptime,
                        "kernel" => CompilerDirective::Kernel,
                        "device" => CompilerDirective::Device,
                        "lemma" => CompilerDirective::Lemma,
                        _ => {
                            emit_diagnostic(
                                &Diagnostic::error(&format!("Unknown directive '@{}'", ident))
                                    .with_code("VX0019")
                                    .with_span(span),
                            );
                            return Err(());
                        }
                    };
                    tokens.push(Token {
                        kind: TokenKind::Directive(directive),
                        span,
                    });
                }
                _ => {
                    let span = self.make_span_from(start_pos, start_line, start_col);
                    emit_diagnostic(
                        &Diagnostic::error(&format!("Unexpected character '{}'", c))
                            .with_code("VX0007")
                            .with_span(span),
                    );
                    return Err(());
                }
            }
        }
        Ok(())
    }
}
