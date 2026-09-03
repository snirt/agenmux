// State detection + sidebar subject extraction. Spec: scan.sh detect_state/scan.
use crate::conf::{AgentConf, Check};

/// Walk CHECK_ORDER against title + last 20 screen lines; first hit wins.
pub fn detect_state(conf: &AgentConf, title: &str, screen: &str) -> &'static str {
    let lines: Vec<&str> = screen.trim_end_matches('\n').lines().collect();
    let start = lines.len().saturating_sub(20);
    // NBSP -> space: agents pad prompt lines with U+00A0, which Rust's
    // ASCII-only [[:space:]] would miss (breaks the idle-prompt guard)
    let tail = lines[start..].join("\n").replace('\u{a0}', " ");
    for c in &conf.check_order {
        let hit = match c {
            Check::Bt => m(&conf.blocked_title, title).then_some("blocked"),
            Check::Bs => m(&conf.blocked_screen, &tail).then_some("blocked"),
            Check::Wt => m(&conf.working_title, title).then_some("working"),
            Check::Ws => m(&conf.working_screen, &tail).then_some("working"),
            Check::Is => m(&conf.idle_screen, &tail).then_some("idle"),
        };
        if let Some(s) = hit {
            return s;
        }
    }
    "idle"
}

fn m(r: &Option<regex::Regex>, s: &str) -> bool {
    r.as_ref().is_some_and(|r| r.is_match(s))
}

/// Sidebar subject line: strip agent decoration from the pane title, fall
/// back to scraping the screen (SUBJECT_SCREEN) or asking the conf
/// (SUBJECT_CMD, shell with $path = pane cwd).
pub fn subject(conf: &AgentConf, title: &str, screen: &str, path: &str) -> String {
    let cwd_base = path.rsplit('/').next().unwrap_or(path);
    let mut t = match &conf.title_strip {
        Some(re) => re.replace(title, "").into_owned(),
        None => title.to_string(),
    };
    // pi titles "name - dir"; drop the dir echo
    if let Some(s) = t.strip_suffix(&format!(" - {cwd_base}")) {
        t = s.to_string();
    }
    // blank when the title just echoes the dir or the agent name; agents
    // truncate long dir echoes ("MX-4122-fix-volumez-l..."), so a "..."/"…"
    // title that prefixes the dir counts too
    let trunc = t.strip_suffix("...").or_else(|| t.strip_suffix('…'));
    if t == cwd_base || t == conf.name || trunc.is_some_and(|p| cwd_base.starts_with(p)) {
        t.clear();
    }
    if t.is_empty() {
        if let Some(re) = &conf.subject_screen {
            // sed -nE 's,RE,\1,p' | tail -1: last matching line, matched span
            // replaced by capture 1
            for line in screen.lines() {
                if let Some(cap) = re.captures(line) {
                    let m0 = cap.get(0).unwrap();
                    let g1 = cap.get(1).map_or("", |g| g.as_str());
                    t = format!("{}{}{}", &line[..m0.start()], g1, &line[m0.end()..]);
                }
            }
        }
    }
    t.replace('\t', " ")
}

/// Run SUBJECT_CMD ($pane = tmux pane id, $path = pane cwd, $started = agent
/// process start as UTC YYYY-MM-DDTHH-MM-SS, empty when unknown). One bash
/// fork — callers must cache (scan.rs SubjectCache); forking per scan tick
/// stalls the sidebar loop.
pub fn subject_cmd(
    conf: &AgentConf,
    pane: &str,
    path: &str,
    started: Option<u64>,
) -> Option<String> {
    let cmd = conf.subject_cmd.as_ref()?;
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .env("pane", pane)
        .env("path", path)
        .env("started", started.map(utc_stamp).unwrap_or_default())
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .replace('\t', " "),
    )
}

/// Epoch secs -> "YYYY-MM-DDTHH-MM-SS" UTC, the prefix pi uses for session
/// file names, so confs can compare by plain string order.
fn utc_stamp(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // civil-from-days (H. Hinnant), proleptic Gregorian
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}-{:02}-{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conf::load_conf;

    #[test]
    fn utc_stamp_matches_pi_session_prefix() {
        assert_eq!(utc_stamp(0), "1970-01-01T00-00-00");
        // 2026-09-03T10:25:01Z
        assert_eq!(utc_stamp(1_788_431_101), "2026-09-03T10-25-01");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00-00-00");
    }
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn conf(body: &str) -> AgentConf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "agenmux-test-{}-{}.conf",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, body).unwrap();
        let c = load_conf(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        c
    }

    #[test]
    fn order_first_hit_wins() {
        let c = conf("WORKING_SCREEN='spin'\nBLOCKED_SCREEN='spin'\nCHECK_ORDER=\"ws bs\"\n");
        assert_eq!(detect_state(&c, "", "spin"), "working");
    }

    #[test]
    fn case_insensitive_like_grep_ei() {
        let c = conf("BLOCKED_SCREEN='Do You Want'\nCHECK_ORDER=\"bs\"\n");
        assert_eq!(detect_state(&c, "", "do you want to proceed?"), "blocked");
    }

    #[test]
    fn only_last_20_lines_matter() {
        let c = conf("WORKING_SCREEN='needle'\nCHECK_ORDER=\"ws\"\n");
        let screen = format!("needle\n{}", "x\n".repeat(25));
        assert_eq!(detect_state(&c, "", &screen), "idle");
    }

    #[test]
    fn trailing_blank_rows_do_not_hide_activity() {
        let c = conf("WORKING_SCREEN='needle'\nCHECK_ORDER=\"ws\"\n");
        let screen = format!("needle\n{}", "\n".repeat(21));
        assert_eq!(detect_state(&c, "", &screen), "working");
    }

    #[test]
    fn subject_screen_last_match_capture() {
        let c = conf("SUBJECT_SCREEN='^› (.+)$'\n");
        assert_eq!(
            subject(&c, "", "› first\nnoise\n› second one\n", "/tmp/x"),
            "second one"
        );
    }

    #[test]
    fn title_strip_and_dir_echo() {
        let c = conf("TITLE_STRIP='^π - '\n");
        assert_eq!(subject(&c, "π - myproj", "", "/a/myproj"), "");
        assert_eq!(subject(&c, "π - fix bug", "", "/a/myproj"), "fix bug");
    }

    #[test]
    fn truncated_dir_echo_blanks() {
        let c = conf("");
        assert_eq!(
            subject(&c, "MX-4122-fix-vol...", "", "/a/MX-4122-fix-volumez"),
            ""
        );
        assert_eq!(
            subject(&c, "MX-4122-fix-vol…", "", "/a/MX-4122-fix-volumez"),
            ""
        );
        // truncated real subject is not a dir echo — keep it
        assert_eq!(
            subject(&c, "fix the logo re...", "", "/a/MX-4122-fix-volumez"),
            "fix the logo re..."
        );
    }
}
