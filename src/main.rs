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
        ["scan"] | ["list"] => cmd_scan(),
        ["status"] => cmd_status(),
        ["sidebar"] => sidebar::run(plugin_dir(), scan_cache_path()),
        ["daemon"] => sidebar::run_daemon(plugin_dir(), scan_cache_path()),
        ["key", key] => sidebar::send_key(key),
        ["click", pane, y, client] => y.parse().map_or(2, |y| input::click(pane, y, client)),
        ["wheel", pane, "up"] => input::wheel(pane, input::Direction::Up),
        ["wheel", pane, "down"] => input::wheel(pane, input::Direction::Down),
        ["pane-add"] => panes::pane_add(None),
        ["pane-add", window] => panes::pane_add(Some(window)),
        ["pane-orphan"] => panes::pane_orphan(),
        ["pane-pin"] => panes::pane_pin(),
        ["teardown"] => panes::teardown(),
        ["setup"] => setup::run(&plugin_dir()),
        ["toggle"] => toggle::run(&plugin_dir(), None, None),
        ["toggle", mode] => toggle::run(&plugin_dir(), Some(mode), None),
        ["toggle", mode, client] => toggle::run(&plugin_dir(), Some(mode), Some(client)),
        ["releases", "refresh"] => release::refresh(&plugin_dir()),
        ["update"] => release::update(&plugin_dir(), "latest"),
        ["update", target] => release::update(&plugin_dir(), target),
        ["notification-open", socket, pane, bundle] => {
            notifications::open_pane(socket, pane, bundle)
        }
        _ => {
            eprintln!(
                "usage: agenmux [--version|scan|list|status|sidebar|daemon|key <name>|click <pane> <row> <client>|wheel <pane> <up|down>|pane-add [window]|pane-orphan|pane-pin|teardown|setup|toggle [split|popup] [client]|releases refresh|update [latest|vX.Y.Z]|detect <conf> <screen-file> [title]|notification-open <socket> <pane> <bundle>]"
            );
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

fn run_scan() -> Result<Vec<scan::PaneRow>, tmux::TmuxError> {
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

fn cmd_scan() -> i32 {
    match run_scan() {
        Ok(rows) => {
            print!("{}", scan::to_tsv(&rows));
            0
        }
        Err(e) => {
            eprintln!("agenmux: {e}");
            1
        }
    }
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
            Ok(rows) => rows,
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
