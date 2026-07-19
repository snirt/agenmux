use std::process::Command;

#[test]
fn version_comes_from_cargo_manifest() {
    let output = Command::new(env!("CARGO_BIN_EXE_agents-mon"))
        .arg("--version")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("agents-mon {}\n", env!("CARGO_PKG_VERSION"))
    );
}
