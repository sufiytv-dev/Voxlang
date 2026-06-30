// build.rs – create macOS bundle and embed/copy Windows dark‑mode manifest
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "macos" {
        // --- macOS: create .app bundle (unchanged) ---
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let profile = env::var("PROFILE").unwrap();
        let app_name = env::var("CARGO_PKG_NAME").unwrap();

        let target_dir = manifest_dir.join("target").join(&profile);
        let bundle_dir = target_dir.join(format!("{}.app", app_name));
        let contents = bundle_dir.join("Contents");
        let macos = contents.join("MacOS");
        let resources = contents.join("Resources");

        fs::create_dir_all(&macos).unwrap();
        fs::create_dir_all(&resources).unwrap();

        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>vox</string>
    <key>CFBundleDisplayName</key>
    <string>vox</string>
    <key>CFBundleIdentifier</key>
    <string>com.yourcompany.vox</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>vox</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.13</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>"#;
        fs::write(contents.join("Info.plist"), plist).unwrap();
        println!("cargo:rerun-if-changed=build.rs");
    } else if target_os == "windows" {
        // --- Windows: write manifest, embed it, and copy it ---
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let manifest_path = manifest_dir.join("vox.exe.manifest");

        let manifest_content = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="vox" version="1.0.0.0" processorArchitecture="*"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v2">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">permonitorv2</dpiAwareness>
      <darkMode xmlns="http://schemas.microsoft.com/SMI/2023/WindowsSettings">true</darkMode>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>"#;

        fs::write(&manifest_path, manifest_content).unwrap();

        // Copy manifest to the output directory (external fallback)
        let profile = env::var("PROFILE").unwrap();
        let target_dir = manifest_dir.join("target").join(&profile);
        let dest = target_dir.join("vox.exe.manifest");
        let _ = fs::copy(&manifest_path, &dest);

        // --- Embed the manifest using linker flags ---
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
            manifest_path.display()
        );
        println!("cargo:rustc-link-arg=/MANIFESTUAC:level='asInvoker' uiAccess='false'");

        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=vox.exe.manifest");
    }
}
