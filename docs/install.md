# Installation

**Voxlang** is a verified heterogeneous systems language. Prebuilt binaries are available for Windows and macOS (Linux coming in v0.3). **Dependencies must be installed separately.**

---

## Prerequisites (All Platforms)

Regardless of how you obtain the compiler, you **must** have the following installed:

- **Z3 Prover** – Required for refinement type verification.
  - Windows: Download from [Z3 releases](https://github.com/Z3Prover/z3/releases) and add `z3.exe` to your `PATH`.
  - macOS: `port install z3` (MacPorts) or `brew install z3` (Homebrew).
  - Ensure `z3` is accessible from the command line.

- **LLVM tools** (`clang`, `llc`) – Used for code generation and linking.
  - On Windows, they are included with **MSVC** (see below).
  - On macOS, they come with **Xcode Command Line Tools** (see below).

---

## Windows

### 1. Install MSVC (required)

Voxlang requires the Microsoft C++ toolchain. The easiest way is via **Scoop**:

```powershell
scoop install msvc
Alternatively, install Visual Studio Build Tools (C++ workload) or Visual Studio Community.

2. Install Z3
Download the latest z3-<version>-win64.zip from Z3 releases, extract it, and add the folder containing z3.exe to your PATH.

3. Install the Voxlang compiler
Open a new PowerShell window (to pick up PATH changes) and run:

powershell
powershell -c "irm https://raw.githubusercontent.com/sufiytv-dev/Voxlang-website/main/install.ps1 | iex"
The binary will be placed in ~/.vox/bin and added to your PATH. Verify:

powershell
vox --version
Important: All vox commands must be run inside a Visual Studio Developer Command Prompt (or a terminal where clang/lld can find the Windows SDK). The installer does not set up this environment.

## macOS

1. Install Xcode Command Line Tools
bash
xcode-select --install
This provides clang, llc, and other LLVM tools.

2. Install MacPorts (recommended) or Homebrew
MacPorts is the recommended package manager for Voxlang on macOS. Install it from macports.org.

3. Install Z3
bash
sudo port install z3
(If you prefer Homebrew: brew install z3)

4. Install the Voxlang compiler
Open a terminal and run:

bash
curl -fsSL https://raw.githubusercontent.com/sufiytv-dev/Voxlang-website/main/install.sh | sh
The binary will be placed in ~/.vox/bin and added to your PATH. Verify:

bash
vox --version

## Linux (Coming in v0.3)

Full Linux support is planned for version 0.3. In the meantime, you can build from source (see Building from Source below).

Building from Source (All Platforms)
If you prefer to build the compiler yourself (e.g., to contribute or test unreleased changes):

Prerequisites (already covered)
Rust (stable) – install via rustup

Z3

LLVM tools (clang, llc)

Clone and Build
bash
git clone https://github.com/sufiytv-dev/Voxlang
cd Voxlang
cargo build --release
The binary will be at target/release/vox (or vox.exe on Windows). Copy it to a directory in your PATH or run cargo install --path ..

Next Steps
Once installed, check the Language Syntax guide and try your first program. For GPU development, see the GPU Kernels documentation.
