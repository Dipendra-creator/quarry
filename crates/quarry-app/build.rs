//! Embeds the app icon (resource 1, which GPUI uses for its windows) and the
//! version information shown by Explorer and installers.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = dir.join("resources").join("quarry.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut nums: Vec<u32> = version.split(['.', '-']).filter_map(|p| p.parse().ok()).take(3).collect();
    nums.resize(3, 0);
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Quarry contributors"
      VALUE "FileDescription", "Quarry"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "quarry"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "quarry.exe"
      VALUE "ProductName", "Quarry"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
        a = nums[0],
        b = nums[1],
        c = nums[2],
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("quarry.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
