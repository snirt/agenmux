// Pane enumeration + state detection over the control-mode pipe.
// Output contract: byte-identical to `scan.sh list` / `scan.sh status`.
use crate::conf::AgentConf;
use crate::procs::{self, IdentCache, Snapshot};
use crate::tmux::{Tmux, TmuxError};
use std::collections::HashMap;

/// pane -> (cwd, SUBJECT_CMD output). Startup seeds may hold cwd basename;
/// entries live while the pane sits idle and drop on any state change (a new
/// prompt flips the pane to working), so the fork re-runs only when the
/// subject could actually have changed.
/// ponytail: assumes new sessions always bounce through a non-idle state
pub type SubjectCache = HashMap<String, (String, String)>;

pub struct PaneRow {
    pub pane: String,
    pub loc: String,
    pub agent: String,
    pub state: String,
    pub cwd: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneMeta {
    pub pane: String,
    pub pane_index: u32,
    pub pane_title: String,
    pub command: String,
    pub path: String,
    pub window_id: String,
    pub window_index: u32,
    pub window_name: String,
    pub session_id: String,
    pub session_name: String,
    pub agent_index: Option<usize>,
}

pub struct ScanSnapshot {
    pub panes: Vec<PaneMeta>,
    pub agents: Vec<PaneRow>,
}

struct ParsedPane {
    meta: PaneMeta,
    pid: u32,
}

const LIST_FMT: &str = "list-panes -a -F '#{session_id}\t#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{pane_id}\t#{pane_index}\t#{pane_pid}\t#{pane_current_command}\t#{pane_current_path}\t#{pane_title}\t#{@agenmux}'";

fn valid_tmux_id(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_panes(rows: &str, self_pane: Option<&str>) -> Vec<ParsedPane> {
    rows.lines()
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            let [session_id, session_name, window_id, window_index, window_name, pane, pane_index, pid, command, path, pane_title, marked] =
                fields.as_slice()
            else {
                return None;
            };
            if !valid_tmux_id(session_id, '$')
                || !valid_tmux_id(window_id, '@')
                || !valid_tmux_id(pane, '%')
            {
                return None;
            }
            let window_index = window_index.parse().ok()?;
            let pane_index = pane_index.parse().ok()?;
            let pid = pid.parse().ok()?;
            if self_pane == Some(*pane)
                || *marked == "1"
                || (pid == 0 && *pane_title == "agenmux")
            {
                return None;
            }
            Some(ParsedPane {
                meta: PaneMeta {
                    pane: (*pane).to_string(),
                    pane_index,
                    pane_title: (*pane_title).to_string(),
                    command: (*command).to_string(),
                    path: (*path).to_string(),
                    window_id: (*window_id).to_string(),
                    window_index,
                    window_name: (*window_name).to_string(),
                    session_id: (*session_id).to_string(),
                    session_name: (*session_name).to_string(),
                    agent_index: None,
                },
                pid,
            })
        })
        .collect()
}

pub fn scan(
    tmux: &mut Tmux,
    confs: &[AgentConf],
    cache: &mut IdentCache,
    subj: &mut SubjectCache,
    self_pane: Option<&str>,
) -> Result<ScanSnapshot, TmuxError> {
    tmux.sync()?;
    let rows = tmux.run(LIST_FMT)?;
    let mut snap: Option<Snapshot> = None;
    let mut panes = Vec::new();
    let mut agents = Vec::new();
    let mut seen = IdentCache::new();
    let buf = format!("agenmux-{}", std::process::id());
    let cap = std::env::temp_dir().join(&buf);
    let mut captured_any = false;
    for parsed in parse_panes(&rows, self_pane) {
        let ParsedPane { mut meta, pid } = parsed;
        let pane = meta.pane.as_str();
        let cmd = meta.command.as_str();
        let path = meta.path.as_str();
        let title = meta.pane_title.as_str();
        let key = (pane.to_string(), pid, cmd.to_string());
        let name = cache
            .get(&key)
            .cloned()
            .or_else(|| {
                (pid != 0)
                    .then(|| procs::identify(confs, &mut snap, pid, cmd))
                    .flatten()
                    .map(|i| confs[i].name.clone())
            });
        let Some(name) = name else {
            panes.push(meta);
            continue;
        };
        seen.insert(key, name.clone());
        let Some(idx) = confs.iter().position(|c| c.name == name) else {
            panes.push(meta);
            continue; // conf removed since cached
        };
        // pane content must never travel over the control pipe: a pane
        // displaying literal "%end <t> <num>" text (logs, this plugin's own
        // docs...) would terminate the response block early and desync every
        // later command. Route it through a buffer + file instead.
        tmux.run(&format!("capture-pane -b '{buf}' -t '{pane}'"))?;
        tmux.run(&format!("save-buffer -b '{buf}' '{}'", cap.display()))?;
        captured_any = true;
        let screen = std::fs::read_to_string(&cap).unwrap_or_default();
        let state = crate::detect::detect_state(&confs[idx], title, &screen);
        let mut subject = crate::detect::subject(&confs[idx], title, &screen, path);
        if state != "idle" {
            subj.remove(pane); // pane got a new prompt — cached subject is stale
        } else if subject.is_empty() && confs[idx].subject_cmd.is_some() {
            let cached = subj
                .get(pane)
                .filter(|(cwd, _)| {
                    cwd == path || cwd == path.rsplit('/').next().unwrap_or(path)
                })
                .map(|(_, subject)| subject.clone());
            match cached {
                Some(cached) => {
                    subject = cached;
                    subj.insert(pane.to_string(), (path.to_string(), subject.clone()));
                }
                None => {
                    let t0 = std::time::Instant::now();
                    let started = procs::agent_start(&confs[idx], &mut snap, pid);
                    subject = crate::detect::subject_cmd(&confs[idx], pane, path, started)
                        .unwrap_or_default();
                    crate::tmux::debug_note(&format!(
                        "subject_cmd {pane} {}ms",
                        t0.elapsed().as_millis()
                    ));
                    subj.insert(pane.to_string(), (path.to_string(), subject.clone()));
                }
            }
        }
        meta.agent_index = Some(agents.len());
        agents.push(PaneRow {
            pane: pane.to_string(),
            loc: format!(
                "{}:{}.{}",
                meta.session_name, meta.window_index, meta.pane_index
            ),
            agent: name,
            state: state.to_string(),
            cwd: path.rsplit('/').next().unwrap_or(path).to_string(),
            title: subject,
        });
        panes.push(meta);
    }
    if captured_any {
        let _ = tmux.run(&format!("delete-buffer -b '{buf}'"));
        let _ = std::fs::remove_file(&cap);
    }
    subj.retain(|pane, _| seen.keys().any(|k| &k.0 == pane)); // dead panes pruned
    *cache = seen;
    crate::tmux::debug_note(&format!(
        "snapshot panes={} agents={}",
        panes.len(),
        agents.len()
    ));
    Ok(ScanSnapshot { panes, agents })
}

pub fn to_tsv(rows: &[PaneRow]) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                r.pane, r.loc, r.agent, r.state, r.cwd, r.title
            )
        })
        .collect()
}

/// `#[fg=red]⣿#[default]N #[fg=yellow]⣾#[default]N #[fg=green]⣿#[default]N`,
/// zero counts omitted, no trailing space. Empty when no agents.
pub fn status_segment(rows: &[PaneRow]) -> String {
    let n = |s: &str| rows.iter().filter(|r| r.state == s).count();
    let (b, w, i) = (n("blocked"), n("working"), n("idle"));
    let mut out = String::new();
    if b > 0 {
        out.push_str(&format!("#[fg=red]⣿#[default]{b} "));
    }
    if w > 0 {
        out.push_str(&format!("#[fg=yellow]⣾#[default]{w} "));
    }
    if i > 0 {
        out.push_str(&format!("#[fg=green]⣿#[default]{i} "));
    }
    out.trim_end().to_string()
}

/// Parse cached TSV back into rows (only state is needed downstream, but
/// keep the full row for the sidebar's instant first frame).
pub fn from_tsv(tsv: &str) -> Vec<PaneRow> {
    tsv.lines()
        .filter_map(|line| {
            let mut f = line.splitn(6, '\t');
            Some(PaneRow {
                pane: f.next()?.to_string(),
                loc: f.next()?.to_string(),
                agent: f.next()?.to_string(),
                state: f.next()?.to_string(),
                cwd: f.next()?.to_string(),
                title: f.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: &str) -> PaneRow {
        PaneRow {
            pane: "%1".into(),
            loc: "s:1.1".into(),
            agent: "claude".into(),
            state: state.into(),
            cwd: "x".into(),
            title: String::new(),
        }
    }

    #[test]
    fn pane_inventory_parser_skips_malformed_and_sidebar_rows() {
        let rows = [
            "$1\twork\t@1\t0\teditor\t%1\t0\t101\tcodex\t/repo\trepo\t",
            "$1\twork\t@1\t0\teditor\t%2\t1\t0\t\t/repo\tordinary\t",
            "missing\tfields",
            "$1\twork\t@1\tx\teditor\t%3\t2\t102\tsleep\t/repo\tbad window\t",
            "$1\twork\t@1\t0\teditor\t%4\tx\t103\tsleep\t/repo\tbad pane\t",
            "$1\twork\t@1\t0\teditor\t%5\t3\tnot-a-pid\tsleep\t/repo\tbad pid\t",
            "1\twork\t@1\t0\teditor\t%13\t3\t109\tsleep\t/repo\tbad session prefix\t",
            "$x\twork\t@1\t0\teditor\t%14\t3\t110\tsleep\t/repo\tbad session digits\t",
            "$1\twork\t1\t0\teditor\t%15\t3\t111\tsleep\t/repo\tbad window prefix\t",
            "$1\twork\t@x\t0\teditor\t%16\t3\t112\tsleep\t/repo\tbad window digits\t",
            "$1\twork\t@1\t0\teditor\t17\t3\t113\tsleep\t/repo\tbad pane prefix\t",
            "$1\twork\t@1\t0\teditor\t%x\t3\t114\tsleep\t/repo\tbad pane digits\t",
            "$\twork\t@1\t0\teditor\t%18\t3\t115\tsleep\t/repo\tempty session id\t",
            "$1\twork\t@\t0\teditor\t%19\t3\t116\tsleep\t/repo\tempty window id\t",
            "$1\twork\t@1\t0\teditor\t%\t3\t117\tsleep\t/repo\tempty pane id\t",
            "$1\twork\t@1\t0\teditor\t%6\t4\t104\tsleep\t/repo\ttab\tfragment\t",
            "$1\twork\t@1\t0\teditor\t%7\t5\t105\tbroken",
            "fragment\t/repo\tnewline\t",
            "$1\twork\t@1\t0\teditor\t%8\t6\t106\tsleep\t/repo\tself\t",
            "$1\twork\t@1\t0\teditor\t%9\t7\t0\t\t/repo\tmarked\t1",
            "$1\twork\t@1\t0\teditor\t%10\t8\t0\t\t/repo\tagenmux\t",
            "$1\twork\t@1\t0\teditor\t%11\t9\t107\tsleep\t/repo\tagenmux\t",
            "$2\tother\t@2\t3\tserver\t%12\t4\t108\tsleep\t/tmp\tlater valid\t",
        ]
        .join("\n");

        let parsed = parse_panes(&rows, Some("%8"));

        assert_eq!(
            parsed
                .iter()
                .map(|pane| (pane.meta.pane.as_str(), pane.pid, pane.meta.command.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("%1", 101, "codex"),
                ("%2", 0, ""),
                ("%11", 107, "sleep"),
                ("%12", 108, "sleep"),
            ]
        );
        assert_eq!(parsed[0].meta.session_id, "$1");
        assert_eq!(parsed[0].meta.session_name, "work");
        assert_eq!(parsed[0].meta.window_id, "@1");
        assert_eq!(parsed[0].meta.window_index, 0);
        assert_eq!(parsed[0].meta.window_name, "editor");
        assert_eq!(parsed[0].meta.pane_index, 0);
        assert_eq!(parsed[0].meta.path, "/repo");
        assert_eq!(parsed[0].meta.pane_title, "repo");
        assert_eq!(parsed[0].meta.agent_index, None);
    }

    #[test]
    fn segment_counts_and_omits_zeros() {
        assert_eq!(status_segment(&[]), "");
        assert_eq!(
            status_segment(&[row("working"), row("idle"), row("idle")]),
            "#[fg=yellow]⣾#[default]1 #[fg=green]⣿#[default]2"
        );
        assert_eq!(status_segment(&[row("blocked")]), "#[fg=red]⣿#[default]1");
    }

    #[test]
    fn tsv_roundtrip() {
        let rows = vec![row("idle")];
        let parsed = from_tsv(&to_tsv(&rows));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].state, "idle");
    }
}
