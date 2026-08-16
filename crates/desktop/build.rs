fn main() {
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR to build scripts"),
    );
    let icon = output.join("engineering-placeholder.ico");
    let bytes: [u8; 74] = [
        0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0, 52, 0, 0, 0, 22, 0, 0, 0, 40, 0, 0, 0, 1, 0, 0,
        0, 2, 0, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 224, 105, 23, 255, 0, 0, 0, 0,
    ];
    std::fs::write(&icon, bytes).expect("engineering icon must be written to OUT_DIR");
    let icon_json_path = icon.display().to_string().replace('\\', "\\\\");
    println!("cargo:rustc-env=TAURI_CONFIG={{\"bundle\":{{\"icon\":[\"{icon_json_path}\"]}}}}");
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(icon));
    tauri_build::try_build(attributes).expect("Tauri build metadata must be generated");
}
