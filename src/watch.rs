// watch.rs – Polling file watcher (no notify crate)

use crate::diagnostic::{Diagnostic, debug_log, emit_diagnostic, emit_log};
use crate::std::walkdir::WalkDir;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn run_watcher(entry_path: &Path, _is_build: bool) -> Result<(), Box<dyn std::error::Error>> {
    let watch_root = if entry_path.is_file() {
        entry_path.parent().unwrap_or(Path::new("."))
    } else {
        entry_path
    };
    emit_log(format!("👀 Watching {:?} for changes...", watch_root));
    emit_log("Press Ctrl+C to stop.\n".to_string());

    let mut file_times = HashMap::new();
    update_file_times(watch_root, &mut file_times)?;

    let debounce_dur = Duration::from_millis(80);
    let poll_dur = Duration::from_secs(1);

    loop {
        std::thread::sleep(poll_dur);
        let mut changed = false;
        let mut new_times = HashMap::new();
        update_file_times(watch_root, &mut new_times)?;

        for (path, mtime) in &new_times {
            if let Some(old) = file_times.get(path) {
                if old != mtime {
                    debug_log(format!("File changed: {:?}", path));
                    changed = true;
                    break;
                }
            } else {
                debug_log(format!("New file: {:?}", path));
                changed = true;
                break;
            }
        }
        if !changed {
            for path in file_times.keys() {
                if !new_times.contains_key(path) {
                    debug_log(format!("File removed: {:?}", path));
                    changed = true;
                    break;
                }
            }
        }

        if changed {
            std::thread::sleep(debounce_dur);
            print!("\x1B[2J\x1B[1;1H");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            emit_log("🔄 Re‑checking due to file change...\n".to_string());
            let ok = crate::check_file(
                entry_path.to_str().unwrap(),
                false,
                &crate::host_triple(),
                &crate::CacheConfig::default(),
            );
            if ok {
                emit_log("✅ Check passed.".to_string());
            } else {
                emit_diagnostic(
                    &Diagnostic::error("Check failed after file change").with_code("VX1001"),
                );
            }
            emit_log("\n👀 Still watching...".to_string());
            file_times = new_times;
        }
    }
}

fn update_file_times(
    root: &Path,
    map: &mut HashMap<PathBuf, SystemTime>,
) -> Result<(), Box<dyn std::error::Error>> {
    map.clear();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("vx"))
    {
        let path_buf = entry.path().to_path_buf();
        let metadata = match std::fs::metadata(&path_buf) {
            Ok(m) => m,
            Err(e) => {
                emit_diagnostic(
                    &Diagnostic::warning(format!(
                        "Failed to read metadata for {:?}: {}",
                        path_buf, e
                    ))
                    .with_code("VX1002"),
                );
                continue;
            }
        };
        let modified = metadata.modified()?;
        map.insert(path_buf, modified);
    }
    Ok(())
}
