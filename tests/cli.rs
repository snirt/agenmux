use std::process::{Command, Output};

fn run_without_server(command: &str) -> Output {
    let socket = std::env::temp_dir().join(format!(
        "agents-mon-no-server-{}-{command}",
        std::process::id()
    ));
    Command::new(env!("CARGO_BIN_EXE_agents-mon"))
        .arg(command)
        .env("TMUX", format!("{},0,0", socket.display()))
        .output()
        .unwrap()
}

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

#[test]
fn scan_is_an_exact_alias_for_list_without_a_server() {
    let scan = run_without_server("scan");
    let list = run_without_server("list");

    assert_ne!(scan.status.code(), Some(2), "scan fell through to usage");
    assert_eq!(scan.status.code(), list.status.code());
    assert_eq!(scan.stdout, list.stdout);
    assert_eq!(scan.stderr, list.stderr);
}
