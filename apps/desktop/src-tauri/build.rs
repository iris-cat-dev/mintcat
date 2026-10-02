fn main() {
    println!("cargo:rustc-env=MINTCAT_TARGET={}", std::env::var("TARGET").expect("Cargo target"));
    tauri_build::build();
}
