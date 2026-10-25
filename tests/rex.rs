//! The Rex backend against a fake `rex` CLI: what the scan reports, and that
//! agenmux never asks the CLI to start a server.
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

/// One scan as `rex_scan.lua` returns it: a sidebar block, a shell with a
/// label of its own and no title, and a window Lua encoded as `{}`.
const LISTING: &str = r#"{"sessions":[
 {"session_id":"session:1","label":"work","active_window_id":"window:1","windows":[
  {"window_id":"window:1","label":"Window 1","focused_block_id":"block:2","layout":{},"blocks":[
   {"block_id":"block:1","label":"agenmux","process":{"child":{"pid":11,"name":"agenmux","cwd":"/work"}},"title":{"title":""},"size":{"columns":30,"rows":40}},
   {"block_id":"block:2","label":"shell","process":{"child":{"pid":12,"name":"zsh","argv0":"-zsh","cwd":"/work/a"},"foreground":{"pid":12,"name":"zsh","argv0":"-zsh","cwd":"/work/a"}},"title":{"title":"build"},"size":{"columns":90,"rows":40}}]},
  {"window_id":"window:2","label":"logs","focused_block_id":"block:3","layout":{},"blocks":[
   {"block_id":"block:3","label":"tail","process":{"child":{"pid":13,"name":"zsh","cwd":"/work/b"},"foreground":{"pid":13,"name":"zsh","cwd":"/work/b"}},"title":{"title":""},"size":{"columns":120,"rows":40}}]},
  {"window_id":"window:3","label":"empty","layout":{},"blocks":{}}]}]}"#;

struct Fake {
    dir: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!("agenmux-rex-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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
            .env("AGENMUX_MUX", "rex")
            .env("AGENMUX_REX", self.dir.join("rex"))
            .env("REX_SESSION", "session:1")
            .env("XDG_STATE_HOME", &self.dir)
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
    let listed = stdout(&fake.run(&["list", "-s", "work"]));
    assert_eq!(
        listed,
        concat!(
            "block:2\twork:1.1\t-\t-\ta\tzsh\n",
            "block:3\twork:2.1\t-\t-\tb\tzsh\n",
        )
    );
    // one batched scan, no block-by-block forks
    let calls = fake.calls();
    assert_eq!(calls.matches(" do -e").count(), 1, "{calls}");
    assert!(!calls.contains(" block "), "{calls}");
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
