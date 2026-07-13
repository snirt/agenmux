// Pane enumeration + state detection over the control-mode pipe.
// Output contract: byte-identical to `scan.sh list` / `scan.sh status`.
use crate::conf::AgentConf;
use crate::procs::{self, IdentCache, Snapshot};
use crate::tmux::{Tmux, TmuxError};
use std::collections::HashMap;

/// pane -> (cwd, SUBJECT_CMD output). The sidebar loop must stay fork-free:
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

const LIST_FMT: &str = "list-panes -a -F '#{pane_id}\t#{pane_pid}\t#{pane_current_command}\t#{pane_current_path}\t#{session_name}:#{window_index}.#{pane_index}\t#{pane_title}'";

pub fn scan(
    tmux: &mut Tmux,
    confs: &[AgentConf],
    cache: &mut IdentCache,
    subj: &mut SubjectCache,
    self_pane: Option<&str>,
) -> Result<Vec<PaneRow>, TmuxError> {
    tmux.sync()?;
    let panes = tmux.run(LIST_FMT)?;
    let mut snap: Option<Snapshot> = None;
    let mut rows = Vec::new();
    let mut seen = IdentCache::new();
    let buf = format!("agents-mon-{}", std::process::id());
    let cap = std::env::temp_dir().join(&buf);
    let mut captured_any = false;
    for line in panes.lines() {
        let mut f = line.splitn(6, '\t');
        let (Some(pane), Some(pid), Some(cmd), Some(path), Some(loc), Some(title)) = (
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
        ) else {
            continue;
        };
        if self_pane == Some(pane) {
            continue; // sidebar skips itself
        }
        let pid: u32 = pid.parse().unwrap_or(0);
        let key = (pane.to_string(), pid, cmd.to_string());
        let name = cache
            .get(&key)
            .cloned()
            .unwrap_or_else(|| {
                procs::identify(confs, &mut snap, pid, cmd).map(|i| confs[i].name.clone())
            });
        seen.insert(key, name.clone());
        let Some(name) = name else { continue };
        let Some(idx) = confs.iter().position(|c| c.name == name) else {
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
            match subj.get(pane).filter(|(cwd, _)| cwd == path) {
                Some((_, s)) => subject = s.clone(),
                None => {
                    let t0 = std::time::Instant::now();
                    subject = crate::detect::subject_cmd(&confs[idx], path).unwrap_or_default();
                    crate::tmux::debug_note(&format!(
                        "subject_cmd {pane} {}ms",
                        t0.elapsed().as_millis()
                    ));
                    subj.insert(pane.to_string(), (path.to_string(), subject.clone()));
                }
            }
        }
        rows.push(PaneRow {
            pane: pane.to_string(),
            loc: loc.to_string(),
            agent: name,
            state: state.to_string(),
            cwd: path.rsplit('/').next().unwrap_or(path).to_string(),
            title: subject,
        });
    }
    if captured_any {
        let _ = tmux.run(&format!("delete-buffer -b '{buf}'"));
        let _ = std::fs::remove_file(&cap);
    }
    subj.retain(|pane, _| seen.keys().any(|k| &k.0 == pane)); // dead panes pruned
    *cache = seen;
    Ok(rows)
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
