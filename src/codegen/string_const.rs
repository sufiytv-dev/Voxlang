// string_const.rs - Management of string and binary constants in generated IR.

use crate::codegen::CodegenEngine;
use crate::parser::ASTNode;

impl CodegenEngine {
    /// Add a string constant to the pending list, or return an existing one.
    pub fn add_string_constant(&mut self, content: &str) -> String {
        if let Some(name) = self.string_map.get(content).cloned() {
            self.debug_log(&format!(
                "reusing existing string constant {} for '{}'",
                name, content
            ));
            return name;
        }
        let name = format!("@str_{}", self.string_counter);
        self.string_counter += 1;
        self.string_map.insert(content.to_string(), name.clone());
        self.debug_log(&format!("new string constant {} for '{}'", name, content));

        let escaped = content
            .replace("\\", "\\\\")
            .replace("\"", "\\\"")
            .replace("\n", "\\0A")
            .replace("\r", "\\0D")
            .replace("\t", "\\09");
        let full_literal = format!("{}\\00", escaped);
        let byte_len = content.len() + 1;
        self.pending_strings
            .push((name.clone(), full_literal, byte_len));
        self.string_len.insert(name.clone(), byte_len);
        name
    }

    /// Add a binary constant (e.g., GPU binary) to the pending list.
    pub fn add_binary_constant(&mut self, bytes: &[u8]) -> String {
        let name = format!("@device_binary_{}", self.string_counter);
        self.string_counter += 1;
        let mut escaped = String::new();
        for &b in bytes {
            escaped.push_str(&format!("\\{:02X}", b));
        }
        let len = bytes.len();
        self.pending_strings.push((name.clone(), escaped, len));
        self.string_len.insert(name.clone(), len);
        name
    }

    /// Emit all pending string/binary constants into the host IR.
    pub fn emit_string_constants(&mut self) {
        if self.pending_strings.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        for (name, content, len) in &self.pending_strings {
            let line = format!(
                "{} = private unnamed_addr constant [{} x i8] c\"{}\", align 1",
                name, len, content
            );
            lines.push(line);
        }
        for line in lines {
            self.debug_emit(&line);
        }
        if !self.pending_strings.is_empty() {
            self.debug_emit("");
        }
        self.pending_strings.clear();
    }

    /// Generate a fat pointer (i8* + length) for a string constant,
    /// and return the SSA register holding the {i8*, i64} value.
    pub fn get_string_fat_ptr(&mut self, content: &str) -> String {
        let name = self.add_string_constant(content);
        let len = *self.string_len.get(&name).unwrap();
        self.debug_log(&format!(
            "generating fat pointer for string constant {}",
            name
        ));

        // Get i8* pointer to the start of the array using modern opaque pointer syntax
        let array_ptr = self.next_register();
        self.debug_emit(&format!(
            "    {} = getelementptr i8, ptr {}, i64 0",
            array_ptr, name
        ));

        let fat_ptr_alloca = self.next_register();
        self.debug_emit(&format!("    {} = alloca {{ i8*, i64 }}", fat_ptr_alloca));

        let ptr_field = self.next_register();
        self.debug_emit(&format!(
            "    {} = getelementptr inbounds {{ i8*, i64 }}, {{ i8*, i64 }}* {}, i32 0, i32 0",
            ptr_field, fat_ptr_alloca
        ));
        self.debug_emit(&format!("    store i8* {}, i8** {}", array_ptr, ptr_field));

        let len_field = self.next_register();
        self.debug_emit(&format!(
            "    {} = getelementptr inbounds {{ i8*, i64 }}, {{ i8*, i64 }}* {}, i32 0, i32 1",
            len_field, fat_ptr_alloca
        ));
        self.debug_emit(&format!("    store i64 {}, i64* {}", len - 1, len_field));

        let result_reg = self.next_register();
        self.debug_emit(&format!(
            "    {} = load {{ i8*, i64 }}, {{ i8*, i64 }}* {}",
            result_reg, fat_ptr_alloca
        ));
        result_reg
    }

    /// Generate a raw i8* pointer to a string constant (without length).
    /// Returns `(register_name, instruction_line)` – the caller must emit the instruction line.
    pub fn get_string_ptr(&mut self, content: &str) -> (String, String) {
        let name = self.add_string_constant(content);
        let reg = self.next_register();
        let inst = format!("    {} = getelementptr i8, ptr {}, i64 0", reg, name);
        (reg, inst)
    }

    /// Generate an i8* pointer to a binary constant given its constant name.
    /// Returns `(register_name, instruction_line)` – the caller must emit the instruction line.
    pub fn get_binary_ptr(&mut self, const_name: &str) -> (String, String) {
        let reg = self.next_register();
        let inst = format!("    {} = getelementptr i8, ptr {}, i64 0", reg, const_name);
        (reg, inst)
    }

    // The rest unchanged...
    pub fn collect_strings(&mut self, node: &ASTNode) {
        match node {
            ASTNode::Program(stmts, _) => {
                for stmt in stmts {
                    self.collect_strings(stmt);
                }
            }
            ASTNode::StructDef { fields, .. } => for _field in fields {},
            ASTNode::FunctionDef { body, .. } => {
                for stmt in body {
                    self.collect_strings(stmt);
                }
            }
            ASTNode::KernelFn { body, .. } => {
                for stmt in body {
                    self.collect_strings(stmt);
                }
            }
            ASTNode::VariableDecl { value, .. } => {
                self.collect_strings(value);
            }
            ASTNode::DeviceVarDecl { value, .. } => {
                self.collect_strings(value);
            }
            ASTNode::Assignment { lhs, value, .. } => {
                self.collect_strings(lhs);
                self.collect_strings(value);
            }
            ASTNode::IfStatement {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                self.collect_strings(condition);
                for stmt in then_branch {
                    self.collect_strings(stmt);
                }
                if let Some(b) = else_branch {
                    for stmt in b {
                        self.collect_strings(stmt);
                    }
                }
            }
            ASTNode::WhileStatement {
                condition, body, ..
            } => {
                self.collect_strings(condition);
                for stmt in body {
                    self.collect_strings(stmt);
                }
            }
            ASTNode::ParallelLoop {
                start, end, body, ..
            } => {
                self.collect_strings(start);
                self.collect_strings(end);
                for stmt in body {
                    self.collect_strings(stmt);
                }
            }
            ASTNode::ComptimeBlock { body, .. } => {
                for stmt in body {
                    self.collect_strings(stmt);
                }
            }
            ASTNode::ReturnStatement(Some(expr), _) => {
                self.collect_strings(expr);
            }
            ASTNode::CastExpr { expr, .. } => {
                self.collect_strings(expr);
            }
            ASTNode::CallExpr { args, .. } => {
                for arg in args {
                    self.collect_strings(arg);
                }
            }
            ASTNode::StructLiteral { fields, .. } => {
                for (_, expr) in fields {
                    self.collect_strings(expr);
                }
            }
            ASTNode::BorrowExpr { expr, .. } => {
                self.collect_strings(expr);
            }
            ASTNode::DerefExpr(expr, _) => {
                self.collect_strings(expr);
            }
            ASTNode::FieldAccess { expr, .. } => {
                self.collect_strings(expr);
            }
            ASTNode::ArrayIndex { array, index, .. } => {
                self.collect_strings(array);
                self.collect_strings(index);
            }
            ASTNode::ArrayLiteral { elements, .. } => {
                for elem in elements {
                    self.collect_strings(elem);
                }
            }
            ASTNode::UnaryExpr { expr, .. } => {
                self.collect_strings(expr);
            }
            ASTNode::BinaryExpr { left, right, .. } => {
                self.collect_strings(left);
                self.collect_strings(right);
            }
            ASTNode::StringLiteral(s, _) => {
                self.add_string_constant(s);
            }
            ASTNode::SliceExpr {
                base, start, end, ..
            } => {
                self.collect_strings(base);
                if let Some(s) = start {
                    self.collect_strings(s);
                }
                if let Some(e) = end {
                    self.collect_strings(e);
                }
            }
            _ => {}
        }
    }
}
