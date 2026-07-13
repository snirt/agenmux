// Forkless process-table snapshot + agent identification.
// Spec: scan.sh identify_agent / agent_for_cmdline / normalize_bin.
use crate::conf::AgentConf;
use std::collections::HashMap;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub struct Snapshot {
    sys: System,
    // ppid -> child pids, from the cheap bulk pass
    children: HashMap<u32, Vec<u32>>,
}

impl Snapshot {
    /// Cheap bulk pass: pids + ppids only. Reading argv is one sysctl per
    /// process (~hundreds of ms system-wide) — deferred to descendant_argvs,
    /// which fetches it for a single pane's subtree.
    pub fn take() -> Snapshot {
        let t0 = std::time::Instant::now();
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        crate::tmux::debug_note(&format!("snapshot bulk {}ms", t0.elapsed().as_millis()));
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, p) in sys.processes() {
            if let Some(pp) = p.parent() {
                children.entry(pp.as_u32()).or_default().push(pid.as_u32());
            }
        }
        Snapshot { sys, children }
    }

    /// BFS over the pane's process tree, root included (agent may be the
    /// pane command itself); argv fetched for just this subtree.
    fn descendant_argvs(&mut self, root: u32) -> Vec<Vec<String>> {
        let mut queue = vec![root];
        let mut i = 0;
        while i < queue.len() {
            if let Some(kids) = self.children.get(&queue[i]) {
                queue.extend(kids);
            }
            i += 1;
        }
        let pids: Vec<Pid> = queue.iter().map(|p| Pid::from_u32(*p)).collect();
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            false,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        pids.iter()
            .filter_map(|pid| self.sys.process(*pid))
            .map(|p| {
                p.cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect::<Vec<String>>()
            })
            .filter(|argv| !argv.is_empty())
            .collect()
    }
}

/// path/wrapper -> bare name (strip dir, .js/.cmd/.exe)
pub fn normalize_bin(tok: &str) -> &str {
    let b = tok.rsplit('/').next().unwrap_or(tok);
    b.strip_suffix(".js")
        .or_else(|| b.strip_suffix(".cmd"))
        .or_else(|| b.strip_suffix(".exe"))
        .unwrap_or(b)
}

fn agent_for_bin(confs: &[AgentConf], bin: &str) -> Option<usize> {
    confs
        .iter()
        .position(|c| c.bins.iter().any(|b| b == bin))
}

/// Wrapped process: first non-flag arg after argv[0] decides.
fn agent_for_argv(confs: &[AgentConf], argv: &[String]) -> Option<usize> {
    for tok in argv.iter().skip(1) {
        match tok.as_str() {
            // inline payload, never an agent
            "-e" | "--eval" | "-c" | "-p" | "--print" => return None,
            t if t.starts_with('-') => continue,
            t => {
                if let Some(i) = agent_for_bin(confs, normalize_bin(t)) {
                    return Some(i);
                }
                return confs
                    .iter()
                    .position(|c| c.path_hints.iter().any(|h| t.contains(h.as_str())));
                // only the first script arg counts
            }
        }
    }
    None
}

/// Identify the agent running in a pane. `snap` is filled lazily — only a
/// cache miss pays for the process-table read.
pub fn identify(
    confs: &[AgentConf],
    snap: &mut Option<Snapshot>,
    pane_pid: u32,
    cmd: &str,
) -> Option<usize> {
    if let Some(i) = agent_for_bin(confs, normalize_bin(cmd)) {
        return Some(i);
    }
    let snap = snap.get_or_insert_with(Snapshot::take);
    for argv in snap.descendant_argvs(pane_pid) {
        if let Some(i) = agent_for_bin(confs, normalize_bin(&argv[0])) {
            return Some(i);
        }
        if let Some(i) = agent_for_argv(confs, &argv) {
            return Some(i);
        }
    }
    None
}

/// (pane_id, pane_pid, cmd) -> agent name or None; invalidates itself when
/// the pane's foreground command changes.
pub type IdentCache = HashMap<(String, u32, String), Option<String>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize() {
        assert_eq!(normalize_bin("/usr/local/bin/claude"), "claude");
        assert_eq!(normalize_bin("cli.js"), "cli");
        assert_eq!(normalize_bin("codex.exe"), "codex");
    }

    fn confs() -> Vec<AgentConf> {
        let dir = std::env::temp_dir().join(format!("am-procs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pi.conf"),
            "AGENT_BINS=\"pi\"\nAGENT_PATH_HINTS=\"pi-coding-agent\"\n",
        )
        .unwrap();
        let c = vec![crate::conf::load_conf(&dir.join("pi.conf")).unwrap()];
        let _ = std::fs::remove_dir_all(&dir);
        c
    }

    #[test]
    fn argv_first_nonflag_and_payload_bail() {
        let cs = confs();
        let hit = vec!["node".into(), "--max-old-space".into(), "/x/pi-coding-agent/cli.js".into()];
        assert_eq!(agent_for_argv(&cs, &hit), Some(0));
        let payload = vec!["node".into(), "-e".into(), "pi".into()];
        assert_eq!(agent_for_argv(&cs, &payload), None);
        let direct = vec!["sh".into(), "/usr/bin/pi".into()];
        assert_eq!(agent_for_argv(&cs, &direct), Some(0));
    }
}
