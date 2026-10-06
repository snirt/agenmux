#[macro_use]
mod diag;
mod app_config;
mod attention;
mod conf;
mod detect;
mod focus;
mod input;
mod notifications;
mod pane_writers;
mod panes;
mod procs;
mod release;
mod scan;
mod setup;
mod sidebar;
mod tmux;
mod toggle;

use std::path::{Path, PathBuf};

/// Mirrors the CLI section of docs/usage.md: shell commands first, then the
/// internal ones the tmux integration calls.
const USAGE: &str = "\
Usage: agenmux <command> [args]

Commands:
  list [session] [--type <name>]
                           List agent panes (TSV), or every pane in a session;
                           --type keeps one agent or command. scan is an alias
  status                   Print the tmux status-line segment
  config                   Show every configuration option
  config check [--effective [--all]]
                           Validate config.toml; --effective also reads tmux overrides
  config reload            Validate and apply config.toml to running sidebars
  detect <conf> <screen-file> [title]
                           Run an agent's detection rules against a saved screen
  update [latest|vX.Y.Z]   Install a release
  releases refresh         Refresh the cached release list
  -V, --version            Print the version
  -h, --help               Print this help

Internal (called by the tmux integration):
  sidebar, daemon, setup [--if-needed], toggle [split|popup] [client],
  key <name> [client], click <pane> <row> <client>, wheel <pane> <up|down>,
  pane-add [window], pane-orphan, pane-pin, teardown,
  notification-open <socket> <pane> <bundle>
";

pub(crate) fn compat_env(name: &str, legacy: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| std::env::var(legacy).ok())
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = match strs.as_slice() {
        ["--version"] | ["-V"] => {
            println!("agenmux {}", env!("CARGO_PKG_VERSION"));
            0
        }
        ["detect", conf_path, screen_file, rest @ ..] => {
            cmd_detect(conf_path, screen_file, rest.first().copied().unwrap_or(""))
        }
        ["config", "check"] => app_config::check(),
        ["config", "check", "--effective"] => app_config::effective_check(false),
        ["config", "check", "--effective", "--all"] => app_config::effective_check(true),
        ["config", "reload"] => app_config::reload(&plugin_dir()),
        ["config"] | ["config", "-h" | "--help" | "help"] => app_config::help(),
        // Installer contract: 0 enabled, 3 disabled, 1 I/O, 2 invalid.
        ["internal", "notification-eligible"] => match app_config::current(None) {
            Ok(config) => {
                if config.notifications {
                    0
                } else {
                    3
                }
            }
            Err(e) => {
                eprintln!("agenmux: {e}");
                e.exit_code()
            }
        },
        ["scan" | "list", rest @ ..] => match parse_list_args(rest) {
            Some((session, kind)) => cmd_list(session, kind),
            None => {
                eprint!("{USAGE}");
                2
            }
        },
        ["status"] => cmd_status(),
        ["sidebar"] => sidebar::run(plugin_dir(), scan_cache_path()),
        ["daemon"] => sidebar::run_daemon(plugin_dir(), scan_cache_path()),
        ["sidebar-pane"] => pane_writers::run_pane(),
        ["key", key] => sidebar::send_key(key, None),
        ["key", key, client] => sidebar::send_key(key, Some(client)),
        ["click", pane, y, client] => y.parse().map_or(2, |y| input::click(pane, y, client)),
        ["wheel", pane, "up"] => input::wheel(pane, input::Direction::Up),
        ["wheel", pane, "down"] => input::wheel(pane, input::Direction::Down),
        ["pane-add"] => panes::pane_add(None),
        ["pane-add", window] => panes::pane_add(Some(window)),
        ["pane-orphan"] => panes::pane_orphan(),
        ["pane-pin"] => panes::pane_pin(),
        ["teardown"] => match panes::lifecycle_lock() {
            Ok(_lock) => {
                panes::stop_daemon();
                panes::teardown()
            }
            Err(error) => {
                eprintln!("agenmux: cannot acquire lifecycle lock: {error}");
                1
            }
        },
        ["setup"] => setup::run(&plugin_dir()),
        ["setup", "--if-needed"] => setup::run_if_needed(&plugin_dir()),
        ["toggle"] => toggle::run(&plugin_dir(), None, None),
        ["toggle", mode] => toggle::run(&plugin_dir(), Some(mode), None),
        ["toggle", mode, client] => toggle::run(&plugin_dir(), Some(mode), Some(client)),
        ["releases", "refresh"] => release::refresh(&plugin_dir()),
        ["update"] => release::update(&plugin_dir(), "latest"),
        ["update", target] => release::update(&plugin_dir(), target),
        ["notification-open", socket, pane, bundle] => {
            notifications::open_pane(socket, pane, bundle)
        }
        ["-h" | "--help" | "help"] => {
            print!("{USAGE}");
            0
        }
        _ => {
            eprint!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

/// Repo root: the ancestor of the binary that contains agents/ (works from
/// target/release and target/debug); AGENMUX_DIR overrides.
fn plugin_dir() -> PathBuf {
    if let Some(d) = compat_env("AGENMUX_DIR", "AGENTS_MON_DIR") {
        return d.into();
    }
    if let Ok(exe) = std::env::current_exe() {
        for a in exe.ancestors().skip(1) {
            if a.join("agents").is_dir() {
                return a.to_path_buf();
            }
        }
    }
    ".".into()
}

fn scan_cache_path() -> PathBuf {
    std::env::temp_dir().join("agenmux-scan-cache")
}

fn self_pane() -> Option<String> {
    compat_env("AGENMUX_SELF", "AGENTS_MON_SELF").filter(|s| !s.is_empty())
}

fn run_scan() -> Result<scan::ScanSnapshot, tmux::TmuxError> {
    let confs = conf::load_all(&plugin_dir());
    let mut t = tmux::Tmux::connect()?;
    let mut cache = procs::IdentCache::new();
    let mut subj = scan::SubjectCache::new();
    scan::scan(
        &mut t,
        &confs,
        &mut cache,
        &mut subj,
        self_pane().as_deref(),
    )
}

/// `[session] [--type <name>]`, in either order.
fn parse_list_args<'a>(args: &[&'a str]) -> Option<(Option<&'a str>, Option<&'a str>)> {
    let (mut session, mut kind) = (None, None);
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        match arg {
            "--type" if kind.is_none() => kind = Some(*args.next()?),
            _ if session.is_none() && !arg.starts_with('-') => session = Some(arg),
            _ => return None,
        }
    }
    Some((session, kind))
}

/// Without a session: every agent pane. With one: every pane in that session,
/// agent columns `-` for non-agent panes. `--type` matches the agent name, or
/// the running command for a non-agent pane.
fn cmd_list(session: Option<&str>, kind: Option<&str>) -> i32 {
    let snapshot = match run_scan() {
        Ok(snapshot) => snapshot,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return 1;
        }
    };
    let rows: Vec<scan::PaneRow> = match session {
        None => snapshot.agents,
        Some(name) => {
            if !snapshot.panes.iter().any(|p| p.session_name == name) {
                eprintln!("agenmux: no session named {name}");
                return 1;
            }
            snapshot
                .panes
                .iter()
                .filter(|p| p.session_name == name)
                .map(|p| match p.agent_index {
                    Some(i) => snapshot.agents[i].clone(),
                    None => scan::PaneRow {
                        pane: p.pane.clone(),
                        loc: format!("{}:{}.{}", p.session_name, p.window_index, p.pane_index),
                        agent: "-".into(),
                        state: "-".into(),
                        cwd: p.path.rsplit('/').next().unwrap_or(&p.path).to_string(),
                        title: p.command.clone(),
                    },
                })
                .collect()
        }
    };
    let rows: Vec<scan::PaneRow> = rows
        .into_iter()
        .filter(|r| {
            kind.is_none_or(|k| {
                if r.agent == "-" {
                    r.title == k
                } else {
                    r.agent == k
                }
            })
        })
        .collect();
    print!("{}", scan::to_tsv(&rows));
    0
}

fn cmd_status() -> i32 {
    // sidebar refreshes the cache every ~2s — reuse it instead of scanning
    let cache = scan_cache_path();
    let fresh = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < 6);
    let rows = if fresh {
        scan::from_tsv(&std::fs::read_to_string(&cache).unwrap_or_default())
    } else {
        match run_scan() {
            Ok(snapshot) => snapshot.agents,
            Err(_) => return 0, // no server -> empty segment, like bash
        }
    };
    print!("{}", scan::status_segment(&rows));
    0
}

fn cmd_detect(conf_path: &str, screen_file: &str, title: &str) -> i32 {
    let c = match conf::load_conf(Path::new(conf_path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("agenmux: {conf_path}: {e}");
            return 1;
        }
    };
    let screen = match std::fs::read_to_string(screen_file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agenmux: {screen_file}: {e}");
            return 1;
        }
    };
    println!("{}", detect::detect_state(&c, title, &screen));
    0
}

#[cfg(test)]
mod tests {
    use super::parse_list_args;

    #[test]
    fn list_takes_an_optional_session_and_type_in_any_order() {
        assert_eq!(parse_list_args(&[]), Some((None, None)));
        assert_eq!(parse_list_args(&["work"]), Some((Some("work"), None)));
        assert_eq!(
            parse_list_args(&["--type", "claude", "work"]),
            Some((Some("work"), Some("claude")))
        );
        for bad in [
            &["--type"][..],
            &["a", "b"],
            &["--bogus"],
            &["--type", "x", "--type", "y"],
        ] {
            assert_eq!(parse_list_args(bad), None, "{bad:?}");
        }
    }
}
