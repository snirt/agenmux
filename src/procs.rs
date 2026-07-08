// Forkless process-table snapshot + agent identification.
// Spec: scan.sh identify_agent / agent_for_cmdline / normalize_bin.
use crate::conf::AgentConf;
use std::collections::HashMap;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub struct Snapshot {
    // pid -> (ppid, argv)
    procs: HashMap<u32, (u32, Vec<String>)>,
}

impl Snapshot {
    pub fn take() -> Snapshot {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        let procs = sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let argv: Vec<String> = p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect();
                (
                    pid.as_u32(),
                    (p.parent().map(|pp| pp.as_u32()).unwrap_or(0), argv),
                )
            })
            .collect();
        Snapshot { procs }
    }

    /// BFS over the pane's process tree, root included (agent may be the
    /// pane command itself).
    fn descendant_argvs(&self, root: u32) -> Vec<&Vec<String>> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, (ppid, _)) in &self.procs {
            children.entry(*ppid).or_default().push(*pid);
        }
        let mut out = Vec::new();
        let mut queue = vec![root];
        let mut i = 0;
        while i < queue.len() {
            let pid = queue[i];
            i += 1;
            if let Some((_, argv)) = self.procs.get(&pid) {
                if !argv.is_empty() {
                    out.push(argv);
                }
            }
            if let Some(kids) = children.get(&pid) {
                queue.extend(kids);
            }
        }
        out
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
        if let Some(i) = agent_for_argv(confs, argv) {
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
