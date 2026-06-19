// tests/integration.rs

use std::path::Path;
use std::process::Command;

#[test]
fn test_examples() {
    let vox_bin = env!("CARGO_BIN_EXE_vox");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    // The examples are always located at src/Examples relative to the project root.
    let examples_dir = Path::new(manifest_dir).join("src/Examples");
    assert!(
        examples_dir.exists(),
        "Examples directory not found at {}",
        examples_dir.display()
    );

    for entry in std::fs::read_dir(&examples_dir).expect("Failed to read examples directory") {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("vx") {
            let file_name = path.file_name().unwrap().to_string_lossy();
            if file_name.contains("gpu") {
                eprintln!("Skipping GPU example: {}", file_name);
                continue;
            }

            let output = Command::new(vox_bin)
                .arg("run")
                .arg(&path)
                .output()
                .expect("Failed to execute vox");

            assert!(
                output.status.success(),
                "Example '{}' failed with stderr:\n{}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
