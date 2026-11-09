//! Write `TernvaleControlClient.swift` next to the schema and into the app.

fn main() {
    let generated = ternvale_api::generate_swift_client();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crate_path = root.join("generated/TernvaleControlClient.swift");
    let app_path = root.join("../app/Ternvale/Ternvale/Services/GeneratedControlClient.swift");
    if let Some(parent) = crate_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&crate_path, &generated).expect("write crate swift");
    if let Some(parent) = app_path.parent() {
        if parent.is_dir() {
            std::fs::write(&app_path, &generated).expect("write app swift");
        }
    }
    eprintln!("wrote {}", crate_path.display());
}
