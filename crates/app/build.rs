//! Gives lithe.exe its icon and its version information.
//!
//! This is the Windows half of what `scripts/bundle.sh` does for macOS. A
//! bare .exe is a working editor, but Explorer draws it with the generic
//! application icon, the taskbar does the same, and Properties ▸ Details
//! is blank — which is what makes a program look like something that was
//! left in a folder rather than installed.
//!
//! Everything here is optional on purpose. The icon is generated rather
//! than committed (`scripts/make-icon.py --ico`), and a resource compiler
//! is part of the Windows SDK rather than of Rust, so either can be
//! missing. Both cases are a warning and an .exe without an icon, never a
//! failed build.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    windows_resources();
}

#[cfg(windows)]
fn windows_resources() {
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let icon = root.join("assets/lithe.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    if !icon.is_file() {
        println!(
            "cargo:warning=no assets/lithe.ico, so lithe.exe gets no icon; \
             run scripts/make-icon.py --ico to draw one"
        );
        return;
    }

    // Forward slashes: a backslash starts an escape inside an .rc string.
    let icon = icon.display().to_string().replace('\\', "/");
    let version = env!("CARGO_PKG_VERSION");
    let mut numeric: Vec<&str> = version.split('.').collect();
    numeric.resize(4, "0");
    let numeric = numeric.join(",");

    let script = format!(
        r#"#include <winver.h>

// The first icon in the file is the one Explorer and the taskbar draw.
1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS VOS__WINDOWS32
FILETYPE VFT_APP
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "FileDescription", "lithe - a local-only code editor"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "lithe"
            VALUE "LegalCopyright", "MIT"
            VALUE "OriginalFilename", "lithe.exe"
            VALUE "ProductName", "lithe"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        // US English, Unicode: the pair named by the block above.
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );

    let out = Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("lithe.rc");
    if let Err(e) = std::fs::write(&out, script) {
        println!("cargo:warning=could not write {}: {e}", out.display());
        return;
    }
    let result = embed_resource::compile(&out, embed_resource::NONE);
    if let Some(message) = result.manifest_optional().err() {
        println!("cargo:warning=lithe.exe will have no icon: {message}");
    }
}
