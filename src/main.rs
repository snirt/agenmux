mod conf;
mod detect;
mod procs;
mod scan;
mod sidebar;
mod tmux;

use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = match strs.as_slice() {
        ["detect", conf_path, screen_file, rest @ ..] => {
            cmd_detect(conf_path, screen_file, rest.first().copied().unwrap_or(""))
        }
        ["scan"] | ["list"] => cmd_scan(),
        ["status"] => cmd_status(),
        ["sidebar"] => sidebar::run(plugin_dir(), scan_cache_path()),
        _ => {
            eprintln!("usage: agents-mon [scan|status|sidebar|detect <conf> <screen-file> [title]]");
            2
        }
    };
    std::process::exit(code);
}

/// Repo root: the ancestor of the binary that contains agents/ (works from
/// target/release and target/debug); AGENTS_MON_DIR overrides.
fn plugin_dir() -> PathBuf {
    if let Ok(d) = std::env::var("AGENTS_MON_DIR") {
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
    std::env::temp_dir().join("agents-mon-scan-cache")
}

fn self_pane() -> Option<String> {
    std::env::var("AGENTS_MON_SELF").ok().filter(|s| !s.is_empty())
}

fn run_scan() -> Result<Vec<scan::PaneRow>, tmux::TmuxError> {
    let confs = conf::load_all(&plugin_dir());
    let mut t = tmux::Tmux::connect()?;
    let mut cache = procs::IdentCache::new();
    let mut subj = scan::SubjectCache::new();
    scan::scan(&mut t, &confs, &mut cache, &mut subj, self_pane().as_deref())
}

fn cmd_scan() -> i32 {
    match run_scan() {
        Ok(rows) => {
            print!("{}", scan::to_tsv(&rows));
            0
        }
        Err(e) => {
            eprintln!("agents-mon: {e}");
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
            eprintln!("agents-mon: {conf_path}: {e}");
            return 1;
        }
    };
    let screen = match std::fs::read_to_string(screen_file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agents-mon: {screen_file}: {e}");
            return 1;
        }
    };
    println!("{}", detect::detect_state(&c, title, &screen));
    0
}
