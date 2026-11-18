//! The Rex backend against a fake `rex` CLI: what the scan reports, and that
//! agenmux never asks the CLI to start a server.
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

/// One scan as `rex_scan.lua` returns it, with a sidebar, an agent, shells
/// across two sessions, and a window whose blocks Lua encoded as `{}`.
const LISTING: &str = r#"{"sessions":[
 {"session_id":"session:1","label":"work","active_window_id":"window:1","windows":[
  {"window_id":"window:1","label":"Window 1","focused_block_id":"block:2","layout":{},"blocks":[
   {"block_id":"block:1","label":"agenmux","process":{"child":{"pid":11,"name":"agenmux","cwd":"/work"}},"title":{"title":""},"size":{"columns":30,"rows":40}},
   {"block_id":"block:2","label":"shell","process":{"child":{"pid":12,"name":"zsh","argv0":"-zsh","cwd":"/work/a"},"foreground":{"pid":12,"name":"zsh","argv0":"-zsh","cwd":"/work/a"}},"title":{"title":"build"},"size":{"columns":90,"rows":40}},
   {"block_id":"block:4","process":{"foreground":{"pid":14,"name":"sample-runner","cwd":"/work/a"}},"status":{"records":[{"state":"idle"}]}}]},
  {"window_id":"window:2","label":"logs","focused_block_id":"block:3","layout":{},"blocks":[
   {"block_id":"block:3","label":"tail","process":{"child":{"pid":13,"name":"zsh","cwd":"/work/b"},"foreground":{"pid":13,"name":"zsh","cwd":"/work/b"}},"title":{"title":""},"size":{"columns":120,"rows":40}}]},
  {"window_id":"window:3","label":"empty","layout":{},"blocks":{}}]},
 {"session_id":"session:2","label":"other","windows":[
  {"window_id":"window:4","blocks":[
   {"block_id":"block:5","process":{"foreground":{"pid":0,"name":"zsh","cwd":"/other/c"}}}]}]}]}"#;

const WORK_SHELL: &str = "block:2\twork:1.1\t-\t-\ta\tzsh\n";
const WORK_AGENT: &str = "block:4\twork:1.2\tsample\tidle\ta\t\n";
const WORK_LOGS: &str = "block:3\twork:2.1\t-\t-\tb\tzsh\n";
const OTHER_SHELL: &str = "block:5\tother:1.1\t-\t-\tc\tzsh\n";

struct Fake {
    dir: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!(
            "agenmux-rex-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(
            dir.join("agents/sample.conf"),
            "AGENT_BINS='sample-runner'\n",
        )
        .unwrap();
        std::fs::write(dir.join("listing.json"), LISTING).unwrap();
        let script = dir.join("rex");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
printf '%s %s %s\n' "$1" "$2" "$3" >> '{log}'
case " $* " in
  *" whoami "*) echo '{{}}' ;;
  *" do -e "*) cat '{listing}' ;;
  *" capture "*) printf '$ make\nok\n' ;;
  *) exit 1 ;;
esac
"#,
                log = dir.join("calls").display(),
                listing = dir.join("listing.json").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_agenmux"))
            .args(args)
            .env("AGENMUX_DIR", &self.dir)
            .env("AGENMUX_SELF", "")
            .env("AGENMUX_MUX", "rex")
            .env("AGENMUX_REX", self.dir.join("rex"))
            .env("REX_SESSION", "session:1")
            .env("XDG_STATE_HOME", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("AGENMUX_RUNTIME_DIR", self.dir.join("runtime"))
            .env_remove("REX_SERVER")
            .env_remove("TMUX_PANE")
            .env_remove("TMUX")
            .env_remove("REX_BLOCK")
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn rex_scan_lists_terminal_blocks_but_not_the_sidebar() {
    let fake = Fake::new("list");
    let listed = stdout(&fake.run(&["list", "--all"]));
    assert_eq!(
        listed,
        [WORK_SHELL, WORK_AGENT, WORK_LOGS, OTHER_SHELL].concat()
    );
    // one batched scan, no block-by-block forks
    let calls = fake.calls();
    assert_eq!(calls.matches(" do -e").count(), 1, "{calls}");
    assert!(!calls.contains(" block "), "{calls}");
}

#[test]
fn list_filters_only_narrow_the_selected_panes() {
    let fake = Fake::new("list-filters");
    for (args, expected) in [
        (&["list"][..], &[WORK_AGENT][..]),
        (&["list", "-s", "work"], &[WORK_AGENT]),
        (&["list", "--session", "other"], &[]),
        (
            &["list", "--session", "work", "--all"],
            &[WORK_SHELL, WORK_AGENT, WORK_LOGS],
        ),
        (&["list", "-c", "zsh"], &[]),
        (
            &["list", "--all", "--command", "zsh"],
            &[WORK_SHELL, WORK_LOGS, OTHER_SHELL],
        ),
        (&["list", "-c", "sample-runner"], &[WORK_AGENT]),
        (&["list", "-a", "sample"], &[WORK_AGENT]),
        (&["list", "--all", "--agent", "sample"], &[WORK_AGENT]),
        (&["list", "--all", "--agent", "-"], &[]),
        (
            &["scan", "-c", "zsh", "--all", "-s", "other"],
            &[OTHER_SHELL],
        ),
        (&["list", "-a", "sample", "--all", "-s", "other"], &[]),
    ] {
        assert_eq!(stdout(&fake.run(args)), expected.concat(), "{args:?}");
    }
    for args in [
        &["list", "-s", "missing"][..],
        &["list", "--all", "-s", "missing"],
    ] {
        let output = fake.run(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"agenmux: no session named missing\n");
    }
}

#[test]
fn rex_calls_never_start_a_server() {
    let fake = Fake::new("autostart");
    stdout(&fake.run(&["list"]));
    let calls = fake.calls();
    assert!(!calls.is_empty());
    for line in calls.lines() {
        assert!(line.starts_with("--autostart=false "), "{line}");
    }
}
