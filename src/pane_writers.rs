//! Live frame delivery for processless tmux panes.
//!
//! The interface deliberately knows only pane IDs and completed frames. Tmux's
//! blocking `display-message -I` process lifecycle stays inside this module.
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};

struct PaneWriter {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl PaneWriter {
    fn open(pane: &str) -> std::io::Result<PaneWriter> {
        let mut cmd = Command::new("tmux");
        // Be explicit about the server. The daemon itself is a control client,
        // and its display writers must not accidentally land on the default
        // socket when tests or users run tmux with -L/-S.
        if let Ok(tmux) = std::env::var("TMUX") {
            if let Some(socket) = tmux.split(',').next().filter(|s| !s.is_empty()) {
                cmd.arg("-S").arg(socket).env_remove("TMUX");
            }
        }
        let mut child = cmd
            .args(["display-message", "-I", "-t", pane])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take();
        Ok(PaneWriter { child, stdin })
    }

    fn alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    fn emit(&mut self, frame: &[u8]) -> std::io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pane writer closed")
        })?;
        stdin.write_all(frame)?;
        stdin.flush()
    }
}

impl Drop for PaneWriter {
    fn drop(&mut self) {
        // EOF is what tells `display-message -I` to leave the empty pane alive
        // and exit. Waiting prevents one zombie per window switch.
        self.stdin.take();
        let _ = self.child.wait();
    }
}

pub struct PaneWriters {
    writers: HashMap<String, PaneWriter>,
}

impl PaneWriters {
    pub fn new() -> PaneWriters {
        PaneWriters {
            writers: HashMap::new(),
        }
    }

    /// Match the live writer set to visible sidebar panes. Returns true when a
    /// new target needs the current frame replayed.
    pub fn reconcile(&mut self, panes: impl IntoIterator<Item = String>) -> bool {
        let wanted: HashSet<String> = panes.into_iter().collect();
        let mut changed = false;
        self.writers.retain(|pane, writer| {
            let keep = wanted.contains(pane) && writer.alive();
            changed |= !keep;
            keep
        });
        for pane in &wanted {
            if !self.writers.contains_key(pane) {
                if let Ok(writer) = PaneWriter::open(pane) {
                    self.writers.insert(pane.clone(), writer);
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn emit(&mut self, frame: &str) {
        self.writers
            .retain(|_, writer| writer.emit(frame.as_bytes()).is_ok());
    }

    pub fn clear(&mut self) {
        self.writers.clear();
    }
}
