use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/kill-codex.ico");

    if env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("kill-codex.res");

    let status = Command::new("zig")
        .current_dir(&manifest_dir)
        .args(["rc", "/fo"])
        .arg(&output)
        .arg("app.rc")
        .status()
        .expect("Zig 0.16+ is required to compile Windows resources");

    assert!(status.success(), "Zig failed to compile app.rc");
    println!("cargo:rustc-link-arg-bin=kill-codex={}", output.display());
}
