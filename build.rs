use std::{env, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");

    if env::var("PROFILE").as_deref() != Ok("debug") {
        return;
    }

    let output = Command::new("date")
        .arg("+%Y-%m-%d %H:%M")
        .output()
        .expect("date command required for debug build timestamp");
    assert!(output.status.success(), "date command failed");
    let timestamp = String::from_utf8(output.stdout).expect("date output must be UTF-8");
    println!(
        "cargo:rustc-env=AGENMUX_BUILD_TIMESTAMP={}",
        timestamp.trim()
    );
}
