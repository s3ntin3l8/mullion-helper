fn main() {
    println!(
        "cargo:rustc-env=MULLION_TARGET_TRIPLE={}",
        std::env::var("TARGET").expect("TARGET is set by Cargo")
    );
    tauri_build::build()
}
