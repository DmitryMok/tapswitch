//! Ресурсы exe: иконки (1 — программа, 2 — телефон подключён) и сведения о версии,
//! которые показывают Проводник, диспетчер задач и окно SmartScreen.
//! Компилирует их `windres` из MSYS2 — его кладёт в PATH `build.cmd`.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let assets = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for name in ["app.ico", "app_connected.ico"] {
        println!("cargo:rerun-if-changed=assets/{name}");
    }
    println!("cargo:rerun-if-changed=build.rs");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numbers: Vec<&str> = version.split(['.', '-']).take(3).collect();
    let comma = format!("{},{},{},0", numbers[0], numbers[1], numbers[2]);
    let path = |name: &str| assets.join(name).display().to_string().replace('\\', "/");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{app}"
2 ICON "{connected}"
1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "Tapswitch"
      VALUE "ProductName", "Tapswitch"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "tapswitch.exe"
      VALUE "InternalName", "tapswitch"
      VALUE "LegalCopyright", "Dmitry Mokachev"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        app = path("app.ico"),
        connected = path("app_connected.ico"),
    );
    let rc_path = out.join("resources.rc");
    std::fs::write(&rc_path, rc).unwrap();
    let object = out.join("resources.o");
    let status = Command::new("windres")
        .args(["--input-format=rc", "--output-format=coff", "-c", "65001", "-i"])
        .arg(&rc_path)
        .arg("-o")
        .arg(&object)
        .status()
        .expect("windres не найден: запускайте сборку через build.cmd (он добавляет MSYS2 в PATH)");
    assert!(status.success(), "windres завершился с ошибкой");
    println!("cargo:rustc-link-arg-bins={}", object.display());

    embed_folders(&out);
}

/// Список встроенных иконок ролей и сценариев: всё, что лежит в `app/icons` и `app/scenarios`.
fn embed_folders(out: &std::path::Path) {
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let list = |folder: &str, extension: &str, macro_name: &str| -> String {
        println!("cargo:rerun-if-changed={folder}");
        let mut paths: Vec<PathBuf> = std::fs::read_dir(crate_dir.join(folder))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == extension))
            .collect();
        paths.sort();
        paths
            .iter()
            .map(|p| {
                let stem = p.file_stem().unwrap().to_string_lossy();
                let full = p.canonicalize().unwrap().display().to_string().replace(r"\\?\", "");
                format!("    ({stem:?}, {macro_name}!({full:?})),\n")
            })
            .collect()
    };
    let code = format!(
        "/// Иконки ролей из app/icons.\npub const ICONS: &[(&str, &[u8])] = &[\n{}];\n\
         /// Сценарии из app/scenarios.\npub const SCENARIOS: &[(&str, &str)] = &[\n{}];\n\
         /// Словари строк интерфейса из app-rs/locales: язык → JSON.\npub const LOCALES: &[(&str, &str)] = &[\n{}];\n",
        list("../app/icons", "svg", "include_bytes"),
        list("../app/scenarios", "yaml", "include_str"),
        list("locales", "json", "include_str"),
    );
    std::fs::write(out.join("embedded.rs"), code).unwrap();
}
