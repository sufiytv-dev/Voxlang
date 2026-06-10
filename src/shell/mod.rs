// src/shell/mod.rs – Voxlang REPL (no rustyline)

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use crate::CacheConfig;
use crate::{compile_source, get_output_dir, host_triple};

struct ReplState {
    global_buffer: Vec<String>,
    main_buffer: Vec<String>,
    config: CacheConfig,
    target_triple: String,
}

impl ReplState {
    fn new(target: &str) -> Self {
        let config = CacheConfig {
            no_cache: true,
            reuse_proofs: false,
            reuse_bitcode: false,
            offline: true,
            trust_modules: false,
        };
        Self {
            global_buffer: Vec::new(),
            main_buffer: Vec::new(),
            config,
            target_triple: target.to_string(),
        }
    }

    fn is_global(line: &str) -> bool {
        let trimmed = line.trim_start();
        trimmed.starts_with("fn ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("@kernel")
            || trimmed.starts_with("@device")
            || trimmed.starts_with("@comptime")
    }

    fn add_line(&mut self, line: &str) {
        if Self::is_global(line) {
            self.global_buffer.push(line.to_string());
        } else {
            self.main_buffer.push(line.to_string());
        }
    }

    fn clear_main(&mut self) {
        self.main_buffer.clear();
    }

    fn synthesize(&self) -> String {
        let mut src = String::new();
        for line in &self.global_buffer {
            src.push_str(line);
            src.push('\n');
        }
        src.push('\n');
        src.push_str("fn main() -> i32:\n");
        for line in &self.main_buffer {
            src.push_str("    ");
            src.push_str(line);
            src.push('\n');
        }
        src.push_str("    return 0\n");
        src.push_str("}\n");
        src
    }

    fn compile_and_run(&self) -> Result<(), String> {
        let src = self.synthesize();
        let cache_dir = crate::get_cache_dir();
        let temp_file = cache_dir.join("repl_session.vx");
        fs::write(&temp_file, &src).map_err(|e| format!("Failed to write temp file: {}", e))?;

        let result = compile_source(
            temp_file.to_str().unwrap(),
            false,
            &self.target_triple,
            None,
            None,
            &self.config,
        )?;

        if !result.semantic_ok {
            let _ = fs::remove_file(&temp_file);
            return Err("Semantic errors in REPL input".to_string());
        }

        let exe_name = temp_file.file_stem().unwrap().to_str().unwrap();
        let out_dir = get_output_dir("debug");
        let exe_path = if cfg!(windows) {
            out_dir.join(format!("{}.exe", exe_name))
        } else {
            out_dir.join(exe_name)
        };
        let ll_path = out_dir.join(format!("{}.ll", exe_name));
        fs::write(&ll_path, &result.llvm_ir).map_err(|e| e.to_string())?;

        let llvm_tools = crate::discovery::find_llvm_tools().map_err(|e| e.to_string())?;
        let linker = llvm_tools.clang;
        let mut cmd = Command::new(&linker);
        cmd.arg(&ll_path).arg("-o").arg(&exe_path);

        let runtime_file = find_runtime_file()
            .ok_or_else(|| "Missing runtime file (vox_rt.c or vox_rt.cpp) in current directory or project root. REPL requires a compiled runtime.")?;
        cmd.arg(&runtime_file);

        if self.target_triple.contains("windows") {
            cmd.arg("-luser32");
        } else {
            cmd.arg("-lm");
        }

        let status = cmd.status().map_err(|e| format!("Linker error: {}", e))?;
        if !status.success() {
            return Err("Linking failed".to_string());
        }

        let output = Command::new(&exe_path)
            .output()
            .map_err(|e| format!("Execution failed: {}", e))?;
        if !output.stdout.is_empty() {
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        }

        let _ = fs::remove_file(&temp_file);
        let _ = fs::remove_file(&ll_path);
        // Optionally keep the executable for debugging (or remove it)
        // let _ = fs::remove_file(&exe_path);
        Ok(())
    }
}

fn find_runtime_file() -> Option<PathBuf> {
    let candidates = ["vox_rt.c", "vox_rt.cpp"];
    for candidate in candidates {
        if std::path::Path::new(candidate).exists() {
            return Some(PathBuf::from(candidate));
        }
        let parent = std::path::Path::new("..").join(candidate);
        if parent.exists() {
            return Some(parent);
        }
    }
    None
}

pub fn run() -> Result<(), String> {
    let target = host_triple();
    let mut state = ReplState::new(&target);

    let mut block_depth: i32 = 0;
    let mut current_block = String::new();

    println!("Voxlang REPL (type :help for help, :quit to exit)");
    println!("Note: Auto‑printing of naked expressions is not yet implemented.");
    println!("      Multiline: lines ending with ':' start a block; '}}' closes it.");

    loop {
        let prompt = if block_depth == 0 { ">>> " } else { "... " };
        print!("{}", prompt);
        io::stdout().flush().map_err(|e| e.to_string())?;

        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        let line = line.trim_end_matches('\n');

        if line == ":quit" || line == ":exit" {
            println!("Goodbye!");
            break;
        } else if line == ":help" {
            println!("Commands: :quit, :help, :reset");
            println!("Multiline: lines ending with ':' start a block; '}}' closes it.");
            continue;
        } else if line == ":reset" {
            state.clear_main();
            println!("Main buffer cleared.");
            continue;
        }

        current_block.push_str(line);
        current_block.push('\n');

        let trimmed_line = line.trim_start();
        if trimmed_line.ends_with(':') {
            block_depth += 1;
        }
        if trimmed_line.contains('}') {
            block_depth = block_depth.saturating_sub(trimmed_line.matches('}').count() as i32);
        }

        if block_depth == 0 && !current_block.trim().is_empty() {
            for l in current_block.lines() {
                state.add_line(l);
            }
            match state.compile_and_run() {
                Ok(()) => state.clear_main(),
                Err(e) => {
                    eprintln!("Error: {}", e);
                    state.clear_main();
                }
            }
            current_block.clear();
        }
    }
    Ok(())
}
