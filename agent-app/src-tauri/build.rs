fn main() {
    // tauri-build only reruns when tauri.conf.json or capabilities change, so a regenerated icon
    // set would otherwise never reach the exe's embedded resources.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
