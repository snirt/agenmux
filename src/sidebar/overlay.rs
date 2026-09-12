use crate::app_config::{Action, Palette};
use crate::input::{term_size, Key};
use crate::release;
use crate::tmux::command_spawn;
use std::path::PathBuf;

use super::render::{app_title, bar, cursor_mark, join};
use super::{Sidebar, E};

pub(super) enum Overlay {
    Help,
    Versions { sel: usize, chosen: Option<String> },
}

/// The release this engine belongs to. install-bin.sh installs the binary that
/// matches the checkout's Cargo.toml, so this is also the plugin's version.
pub(super) fn current_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Newest release, as recorded by install-bin.sh's (at most daily) check.
/// None unless it is strictly newer than what is running: a checkout ahead of
/// every release (master, or a just-bumped manifest) must not be told to
/// "update" to the older tag behind it.
pub(super) fn update_available(plugin_dir: &PathBuf) -> Option<String> {
    let release_dir = plugin_dir.join("target/release");
    let latest = std::fs::read_to_string(release_dir.join(".agenmux-latest"))
        .or_else(|_| std::fs::read_to_string(release_dir.join(".agents-mon-latest")))
        .ok()?;
    let latest = latest.trim();
    (is_tag(latest) && newer_than(latest, &current_tag())).then(|| latest.to_string())
}

/// Numeric, component-wise tag compare: is `a` a later release than `b`?
/// String order is not enough — "v0.1.10" sorts before "v0.1.9".
fn newer_than(a: &str, b: &str) -> bool {
    let parts = |t: &str| -> Vec<u64> {
        t.trim_start_matches('v')
            .split(['.', '-'])
            .map(|s| s.parse().unwrap_or(0))
            .collect()
    };
    let (x, y) = (parts(a), parts(b));
    for i in 0..x.len().max(y.len()) {
        let (l, r) = (
            x.get(i).copied().unwrap_or(0),
            y.get(i).copied().unwrap_or(0),
        );
        if l != r {
            return l > r;
        }
    }
    false
}

/// Releases install-bin.sh saw on the remote, newest first.
pub(super) fn known_tags(plugin_dir: &PathBuf) -> Vec<String> {
    let release_dir = plugin_dir.join("target/release");
    let raw = std::fs::read_to_string(release_dir.join(".agenmux-tags"))
        .or_else(|_| std::fs::read_to_string(release_dir.join(".agents-mon-tags")))
        .unwrap_or_default();
    let mut tags: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|t| is_tag(t))
        .map(String::from)
        .collect();
    tags.truncate(10);
    tags
}

pub(super) fn picker_sel(tags: &[String], cur: &str, chosen: Option<&str>, sel: usize) -> usize {
    let selected = chosen
        .and_then(|tag| tags.iter().position(|t| t == tag))
        .or_else(|| {
            chosen
                .is_none()
                .then(|| tags.iter().position(|t| t == cur))
                .flatten()
        })
        .unwrap_or(sel);
    selected.min(tags.len().saturating_sub(1))
}

/// A tag is passed to update.sh as an argument — keep it boring.
fn is_tag(t: &str) -> bool {
    t.len() > 1
        && t.starts_with('v')
        && t[1..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

impl Sidebar {
    /// Version picker: update or roll back to any release the last check saw.
    /// Selecting one switches the source, the engine, and restarts the view.
    pub(super) fn versions(&mut self) {
        // opening the picker is an explicit "what is out there?" — ask now
        // instead of serving a list that the daily check may have left a day
        // old. It lands in the file and normal scan renders pick it up live.
        let plugin_dir = self.plugin_dir.clone();
        std::thread::spawn(move || {
            release::refresh(&plugin_dir);
        });
        self.overlay = Some(Overlay::Versions {
            sel: 0,
            chosen: None,
        });
        self.last_frame.clear();
    }

    #[cfg_attr(feature = "ratatui", allow(dead_code))]
    pub(super) fn render_overlay(&mut self, force: bool) {
        let title = app_title();
        let header = self.palette.header_fg.fg("1");
        let muted = self.palette.muted_fg.fg("2");
        let idle = self.palette.idle_fg.fg("");
        let working = self.palette.working_fg.fg("");
        let blocked = self.palette.blocked_fg.fg("");
        let done = self.palette.done_fg.fg("");
        let error = self.palette.error_fg.fg("2");
        let text = match &mut self.overlay {
            Some(Overlay::Help) => {
                let accept = self.search_keys[&Action::Accept]
                    .first()
                    .map_or(String::new(), |c| {
                        format!(
                            "; {} enables {}",
                            c.label(false),
                            self.nav_label(false, false)
                        )
                    });
                let mut keys = vec![(self.nav_label(false, true), "move selection".to_string())];
                let jump = if self.settings.settings.show_all_panes {
                    "jump to pane"
                } else {
                    "jump to agent"
                };
                for (action, what) in [
                    (Action::Jump, jump.to_string()),
                    (Action::Search, format!("live search{accept}")),
                    (Action::Filter, "select next state filter".into()),
                    (Action::Reset, "clear filters / show all".into()),
                    (Action::Versions, "update / switch version".into()),
                    (Action::Close, "close sidebar".into()),
                    (Action::Help, "this help".into()),
                ] {
                    keys.push((self.labels(&self.normal_keys, action, false), what));
                }
                let keys: String = keys
                    .iter()
                    .filter(|(label, _)| !label.is_empty())
                    .map(|(label, what)| format!("{label:<8} {what}\n"))
                    .collect();
                format!(
                    "{E}[2J{E}[H{header}{title} — help{E}[0m\n\n\
{E}[1mstatus{E}[0m\n\
 {idle}⣿{E}[0m  idle\n\
 {working}⠹{E}[0m  working (spinner)\n\
 {blocked}⣿{E}[0m  blocked, waiting for input (blinks)\n\
 {done}⣿{E}[0m  done, not viewed yet (blinks)\n\n\
{E}[1mkeys{E}[0m\n{keys}\n\
{muted}press any key to return{E}[0m"
                )
            }
            Some(Overlay::Versions { sel, chosen }) => {
                let cur = current_tag();
                let tags = known_tags(&self.plugin_dir);
                *sel = picker_sel(&tags, &cur, chosen.as_deref(), *sel);
                let mut text = format!("{E}[2J{E}[H{header}{title} — versions{E}[0m\n\n");
                if tags.is_empty() {
                    text.push_str(&format!(
                        " {error}no releases found — checking…{E}[0m\n\n\
                         {muted}{}{E}[0m",
                        self.hint(&self.normal_keys, Action::Close, "back")
                    ));
                } else {
                    for (i, t) in tags.iter().enumerate() {
                        let mark = cursor_mark(&self.palette, i == *sel, true, "idle");
                        let tail = if *t == cur {
                            format!(" {muted}(current){E}[0m")
                        } else {
                            String::new()
                        };
                        text.push_str(&format!("{mark}{t}{tail}\n"));
                    }
                    let hint = join(&[
                        self.hint(&self.normal_keys, Action::Jump, "switch"),
                        self.nav_label(true, true),
                        self.hint(&self.normal_keys, Action::Close, "back"),
                    ]);
                    text.push_str(&format!("\n{muted}{hint}{E}[0m"));
                }
                text
            }
            None => return,
        };
        let text = if self.palette.header_bg == Palette::default().header_bg {
            text
        } else {
            let (header, body) = text.split_once('\n').unwrap_or((&text, ""));
            // Clear with the canvas background, not the header fill.
            let header = header
                .strip_prefix(&format!("{E}[2J{E}[H"))
                .unwrap_or(header);
            let cols = self
                .daemon
                .as_ref()
                .map(|d| d.size.0)
                .unwrap_or_else(|| term_size().0);
            let width = title.chars().count()
                + if matches!(self.overlay, Some(Overlay::Help)) {
                    7
                } else {
                    11
                };
            format!(
                "{E}[2J{E}[H{}\n{body}",
                bar(header, &self.palette.header_bg.bg(), cols, width)
            )
        };
        self.emit(text, "", force);
    }

    pub(super) fn overlay_key(&mut self, key: Key) {
        if matches!(self.overlay, Some(Overlay::Help)) {
            self.close_overlay();
            return;
        }
        let tags = known_tags(&self.plugin_dir);
        let cur = current_tag();
        let mut switch = None;
        let mut close = false;
        if let Some(Overlay::Versions { sel, chosen }) = &mut self.overlay {
            *sel = picker_sel(&tags, &cur, chosen.as_deref(), *sel);
            match key {
                Key::Down if !tags.is_empty() => *sel = (*sel + 1).min(tags.len() - 1),
                Key::Up => *sel = sel.saturating_sub(1),
                Key::Jump => {
                    switch = tags.get(*sel).filter(|t| **t != cur).cloned();
                    close = true;
                }
                Key::Quit | Key::Close => close = true,
                _ => {}
            }
            *chosen = tags.get(*sel).cloned();
        }
        if let Some(tag) = switch {
            self.switch_version(&tag);
            self.close_overlay();
        } else if close {
            self.close_overlay();
        }
    }

    fn close_overlay(&mut self) {
        self.overlay = None;
        if self.daemon.is_none() {
            print!("{E}[2J");
        }
        self.last_frame.clear();
        self.reclaim_key_table();
    }

    fn reclaim_key_table(&mut self) {
        if self.daemon.is_none() {
            return;
        }
        let clients = self
            .tmux
            .run("list-clients -F '#{client_name}\t#{pane_title}'")
            .unwrap_or_default();
        for line in clients.lines() {
            let Some((client, title)) = line.split_once('\t') else {
                continue;
            };
            if title == "agenmux" {
                let _ = command_spawn(&["switch-client", "-c", client, "-T", "agenmux"]);
            }
        }
    }

    /// nohup + no wait: update kills the panes this engine renders into, and a
    /// pane kill would otherwise SIGHUP the switch halfway through.
    fn switch_version(&mut self, tag: &str) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let _ = std::process::Command::new("nohup")
            .arg(exe)
            .arg("update")
            .arg(tag)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    pub(super) fn help(&mut self) {
        self.overlay = Some(Overlay::Help);
        self.last_frame.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_compare_numerically_not_as_strings() {
        assert!(newer_than("v0.1.7", "v0.1.6"));
        assert!(!newer_than("v0.1.6", "v0.1.7")); // the bug the user hit
        assert!(!newer_than("v0.1.7", "v0.1.7"));
        assert!(newer_than("v0.2.0", "v0.1.99"));
        assert!(newer_than("v1.0.0", "v0.99.99"));
        // string order puts v0.1.10 before v0.1.9 — numbers must not
        assert!(newer_than("v0.1.10", "v0.1.9"));
        assert!(!newer_than("v0.1.9", "v0.1.10"));
        // a shorter tag is the same as trailing zeros
        assert!(!newer_than("v0.1", "v0.1.0"));
        assert!(newer_than("v0.1.1", "v0.1"));
    }

    #[test]
    fn picker_selection_survives_refreshes() {
        let tags = vec!["v3".into(), "v2".into(), "v1".into()];
        assert_eq!(picker_sel(&tags, "v2", None, 0), 1);

        let reordered = vec!["v4".into(), "v3".into(), "v1".into(), "v2".into()];
        assert_eq!(picker_sel(&reordered, "v2", Some("v1"), 2), 2);
        assert_eq!(picker_sel(&reordered, "v2", Some("missing"), 1), 1);

        let shrunk = vec!["v3".into()];
        assert_eq!(picker_sel(&shrunk, "v2", Some("missing"), 9), 0);
        assert_eq!(picker_sel(&[], "v2", None, 9), 0);
    }

    #[test]
    fn update_notice_only_for_a_newer_release() {
        let dir = std::env::temp_dir().join(format!("agenmux-test-{}", std::process::id()));
        let file = dir.join("target/release/.agenmux-latest");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();

        assert_eq!(update_available(&dir), None); // no check has run yet
        std::fs::write(&file, format!("{}\n", current_tag())).unwrap();
        assert_eq!(update_available(&dir), None); // already on the newest
        std::fs::write(&file, "v9.9.9\n").unwrap();
        assert_eq!(update_available(&dir).as_deref(), Some("v9.9.9"));
        // the notice rides the header: a newline would push every list line
        // down one and break the click -> pane mapping
        assert!(!update_available(&dir).unwrap().contains('\n'));
        // the regression: any difference counted as an update, so a checkout
        // ahead of every release advertised "↑" for the older tag behind it
        std::fs::write(&file, "v0.0.1\n").unwrap();
        assert_eq!(update_available(&dir), None);
        // the tag is handed to update.sh as an argument
        std::fs::write(&file, "v1.0.0; rm -rf /\n").unwrap();
        assert_eq!(update_available(&dir), None);
        std::fs::write(&file, "garbage\n").unwrap();
        assert_eq!(update_available(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
