use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const REPO_URL: &str = "https://github.com/sufiytv-dev/Voxlang.git";

fn main() {
    println!("🚀 Bootstrapping Voxlang Environment (vxm v0.1)...");

    // 1. Setup directories
    let home_dir = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .expect("Could not find home directory");

    let vox_dir = PathBuf::from(&home_dir).join(".vox");
    let bin_dir = vox_dir.join("bin");
    let src_dir = vox_dir.join("src");

    if !vox_dir.exists() {
        fs::create_dir_all(&bin_dir).expect("Failed to create .vox/bin directory");
    }

    // 2. Clone or Pull
    if src_dir.exists() {
        println!("🔄 Updating existing Voxlang repository...");
        let status = Command::new("git")
            .current_dir(&src_dir)
            .args(["pull", "--rebase"])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect("Failed to execute git pull");

        if !status.success() {
            eprintln!("❌ Git pull failed. Please check your repository state.");
            std::process::exit(1);
        }
    } else {
        println!("📥 Cloning Voxlang repository...");
        let status = Command::new("git")
            .args(["clone", REPO_URL, src_dir.to_str().unwrap()])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect("Failed to execute git clone");

        if !status.success() {
            eprintln!("❌ Git clone failed. Is git installed?");
            std::process::exit(1);
        }
    }

    // 3. Cargo Build
    println!("🔨 Compiling Voxlang (Release mode)...");
    let status = Command::new("cargo")
        .current_dir(&src_dir)
        .args(["build", "--release"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect("Failed to execute cargo build");

    if !status.success() {
        eprintln!("❌ Cargo build failed. Ensure you have the Rust toolchain installed.");
        std::process::exit(1);
    }

    // 4. Move Binary
    let exe_name = if cfg!(target_os = "windows") { "vox.exe" } else { "vox" };
    let source_bin = src_dir.join("target").join("release").join(exe_name);
    let target_bin = bin_dir.join(exe_name);

    println!("📦 Installing binary to {:?}", target_bin);
    fs::copy(&source_bin, &target_bin).expect("Failed to copy executable");

    // 5. Final Instructions
    println!("\n✅ Voxlang installed successfully!");
    println!("--------------------------------------------------");
    println!("Make sure to add the bin directory to your PATH:");

    if cfg!(target_os = "windows") {
        println!("  setx PATH \"%PATH%;{}\"", bin_dir.display());
    } else {
        println!("  export PATH=\"{}:$PATH\"", bin_dir.display());
        println!("  (Add this to your ~/.bashrc or ~/.zshrc)");
    }
    println!("--------------------------------------------------");
    println!("Run `vox --help` to get started.");
}
