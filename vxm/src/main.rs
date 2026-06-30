use std::env;
use std::fs;
use std::path::{Path, PathBuf};
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

    // 3. Cargo Build (Release)
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

    // 4. Install CLI binary
    let exe_name = if cfg!(target_os = "windows") {
        "vox.exe"
    } else {
        "vox"
    };
    let source_bin = src_dir.join("target").join("release").join(exe_name);
    let target_bin = bin_dir.join(exe_name);

    println!("📦 Installing CLI binary to {:?}", target_bin);
    fs::copy(&source_bin, &target_bin).expect("Failed to copy executable");

    // 5. macOS specific: build and install the .app bundle
    if cfg!(target_os = "macos") {
        println!("🍎 Building macOS application bundle...");

        let bundle_script = src_dir.join("bundle.sh");
        if bundle_script.exists() {
            // Ensure the script is executable
            let _ = Command::new("chmod").arg("+x").arg(&bundle_script).status();

            let status = Command::new(&bundle_script)
                .current_dir(&src_dir)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .expect("Failed to run bundle.sh");

            if !status.success() {
                eprintln!("⚠️  bundle.sh failed, but continuing...");
            } else {
                // Locate the generated .app (check common locations)
                if let Some(app_path) = find_app(&src_dir) {
                    install_app_bundle(&app_path);
                } else {
                    eprintln!(
                        "⚠️  Could not find vox.app after bundle.sh. Skipping app installation."
                    );
                }
            }
        } else {
            eprintln!("⚠️  bundle.sh not found in repository. Skipping app bundle build.");
        }
    }

    // 6. Final instructions
    println!("\n✅ Voxlang installed successfully!");
    println!("--------------------------------------------------");
    println!("Make sure to add the bin directory to your PATH:");

    if cfg!(target_os = "windows") {
        println!("  setx PATH \"%PATH%;{}\"", bin_dir.display());
    } else {
        println!("  export PATH=\"{}:$PATH\"", bin_dir.display());
        println!("  (Add this to your ~/.bashrc or ~/.zshrc)");
    }
    if cfg!(target_os = "macos") {
        println!("\nThe Vox application has been installed to your Applications folder.");
        println!("You can launch it from Launchpad or by running `open /Applications/vox.app`");
    }
    println!("--------------------------------------------------");
    println!("Run `vox --help` to get started.");
}

/// Check common locations for the generated `vox.app` bundle.
fn find_app(repo_root: &Path) -> Option<PathBuf> {
    let candidates = [
        repo_root.join("vox.app"),
        repo_root.join("target/release/vox.app"),
        repo_root.join("target/release/bundle/vox.app"),
        repo_root.join("target/debug/vox.app"),
    ];

    for path in candidates {
        if path.exists() && path.is_dir() {
            return Some(path);
        }
    }
    None
}

/// Install the .app bundle to /Applications (or ~/Applications as fallback).
fn install_app_bundle(app_path: &Path) {
    let app_name = app_path.file_name().expect("Invalid app path");
    let target_apps = PathBuf::from("/Applications").join(app_name);

    println!("📦 Installing app to {:?}", target_apps);

    // Try to copy to /Applications using sudo (may ask for password)
    let sudo_status = Command::new("sudo")
        .args(["cp", "-R", app_path.to_str().unwrap(), "/Applications/"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match sudo_status {
        Ok(status) if status.success() => {
            println!("✅ App installed to /Applications.");
            return;
        }
        _ => {
            eprintln!("⚠️  Could not copy to /Applications (sudo may be required).");
        }
    }

    // Fallback: install to ~/Applications
    let home = env::var("HOME").expect("HOME not set");
    let user_apps = PathBuf::from(&home).join("Applications");
    if !user_apps.exists() {
        if let Err(e) = fs::create_dir_all(&user_apps) {
            eprintln!("❌ Failed to create ~/Applications: {}", e);
            return;
        }
    }
    let user_target = user_apps.join(app_name);
    match fs::copy(app_path, &user_target) {
        Ok(_) => println!("✅ App installed to {:?}", user_target),
        Err(e) => eprintln!("❌ Failed to copy app to ~/Applications: {}", e),
    }
}
