// src/shell/mod.rs – Main entry point for the shell

mod editor;
mod lsp;
mod runner;
pub mod terminal;

#[cfg(not(target_os = "windows"))]
mod tty; // only needed for TUI

#[cfg(target_os = "windows")]
mod windows_gui;
#[cfg(target_os = "windows")]
pub use windows_gui::run; // now has signature: pub fn run(hide_console: bool) -> Result<(), String>

#[cfg(not(target_os = "windows"))]
mod tui;
#[cfg(not(target_os = "windows"))]
pub use tui::run; // signature: pub fn run(_hide_console: bool) -> Result<(), String>
