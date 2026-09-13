use crate::app_config::{Action, Palette};
use crate::input::{term_size, Key};
use crate::release;
use crate::tmux::command_spawn;
use std::path::PathBuf;

use super::render::{app_title, bar, clip_frame, cursor_mark, join};
use super::ui::{Action as UiAction, Editor, Label, Select, TextEdit};
use super::{Sidebar, E};

pub(super) enum Overlay {
    Help,
    Versions { sel: usize, chosen: Option<String> },
    Settings(Settings),
}

pub(super) struct Settings {
    sel: usize,
    scroll: usize,
    source: String,
    existed: bool,
    edit: Option<SettingEdit>,
    confirm: bool,
    message: Option<(bool, String)>,
}

struct SettingEdit {
    name: String,
    editor: Editor,
}

struct SettingRow {
    name: String,
    persisted: String,
    effective: String,
    source: String,
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
fn known_tags(plugin_dir: &PathBuf) -> Vec<String> {
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

fn picker_sel(tags: &[String], cur: &str, chosen: Option<&str>, sel: usize) -> usize {
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

fn settings_rows(source: &str, effective: &crate::app_config::AppConfig) -> Vec<SettingRow> {
    let persisted = crate::app_config::parse(source)
        .and_then(|file| crate::app_config::resolve(&file, &Default::default()))
        .map(|config| crate::app_config::rows(&config))
        .unwrap_or_default();
    crate::app_config::rows(effective)
        .into_iter()
        .map(|row| {
            let file = persisted
                .iter()
                .find(|candidate| candidate.name == row.name);
            SettingRow {
                name: row.name,
                persisted: file
                    .filter(|candidate| candidate.source == "file")
                    .map(|candidate| candidate.value.clone())
                    .unwrap_or_else(|| "—".into()),
                effective: row.value,
                source: row.source,
            }
        })
        .collect()
}

fn setting_group(name: &str) -> &'static str {
    if name.starts_with("display.") {
        "Display"
    } else if name.starts_with("behavior.") {
        "Behavior"
    } else if name == "theme.base" {
        "Theme"
    } else if name.starts_with("theme.colors.") {
        "Colors"
    } else {
        "Keymap"
    }
}

fn setting_value(name: &str, buffer: &str) -> Result<String, String> {
    let value = buffer.trim();
    if name.starts_with("keys.") {
        let mut array = toml_edit::Array::new();
        if !value.is_empty() {
            for chord in value.split(',').map(str::trim) {
                crate::app_config::KeyChord::parse(chord).map_err(str::to_string)?;
                array.push(chord);
            }
        }
        return Ok(toml_edit::Value::Array(array).to_string());
    }
    if matches!(name, "display.show_all_panes" | "behavior.notifications") {
        return match value {
            "true" | "false" => Ok(value.into()),
            _ => Err("expected true or false".into()),
        };
    }
    if matches!(name, "display.sidebar_width" | "display.popup_width")
        || (name == "display.popup_height" && value != "auto")
    {
        value
            .parse::<u16>()
            .ok()
            .filter(|number| (1..=10000).contains(number))
            .ok_or_else(|| "expected 1..=10000".to_string())?;
        return Ok(value.into());
    }
    if name.starts_with("theme.colors.") && value.parse::<u8>().is_ok() {
        return Ok(value.into());
    }
    Ok(toml_edit::Value::from(value).to_string())
}

fn choices(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "display.mode" => Some(&["split", "popup"]),
        "theme.base" => Some(&["dark", "light", "terminal"]),
        "display.show_all_panes" | "behavior.notifications" => Some(&["true", "false"]),
        _ => None,
    }
}

fn render_settings(
    settings: &mut Settings,
    effective: &crate::app_config::AppConfig,
    cols: usize,
    rows: usize,
) -> String {
    let all = settings_rows(&settings.source, effective);
    let total = all.len() + 1;
    settings.sel = settings.sel.min(total.saturating_sub(1));
    let selected = all.get(settings.sel);
    let mut out = format!("{E}[2J{E}[H{E}[1m{} — settings{E}[0m\n", app_title());
    if settings.confirm {
        out.push_str("\nRevert to defaults?\n\nPersisted customizations will be removed.\nCLI and tmux overrides remain effective.\n\nEnter confirm · Esc cancel");
        return clip_frame(&out, cols, rows.saturating_sub(1));
    }
    if let Some(SettingEdit {
        name,
        editor: Editor::TextEdit(edit),
    }) = &settings.edit
    {
        out.push_str(&format!(
            "\n{name}\n\n> {}_\n\nEnter apply · Esc cancel",
            edit.value()
        ));
        if let Some((error, message)) = &settings.message {
            out.push_str(&format!(
                "\n\n{}{}{}",
                if *error { "error: " } else { "" },
                message,
                E
            ));
        }
        return clip_frame(&out, cols, rows.saturating_sub(1));
    }
    let select = settings.edit.as_ref().and_then(|edit| match &edit.editor {
        Editor::Select(select) => Some(select),
        Editor::TextEdit(_) => None,
    });
    let narrow = cols < 80;
    if !narrow {
        out.push_str("  setting                          persisted      effective      source\n");
    }
    let fixed_rows = if narrow { 6 } else { 3 };
    let height = rows
        .saturating_sub(fixed_rows + select.map_or(0, Select::height))
        .max(1);
    if settings.sel < settings.scroll {
        settings.scroll = settings.sel;
    } else if settings.sel >= settings.scroll + height {
        settings.scroll = settings.sel + 1 - height;
    }
    let end = (settings.scroll + height).min(total);
    let mut last_group = settings
        .scroll
        .checked_sub(1)
        .and_then(|index| all.get(index))
        .map(|row| setting_group(&row.name))
        .unwrap_or("");
    for index in settings.scroll..end {
        if index == all.len() {
            out.push_str(&format!(
                "{}\n",
                UiAction::new("Revert to defaults").render(index == settings.sel)
            ));
            continue;
        }
        let row = &all[index];
        let group = setting_group(&row.name);
        let first = group != last_group;
        last_group = group;
        let mark = if index == settings.sel { "❯" } else { " " };
        if narrow {
            let name = row.name.rsplit('.').next().unwrap_or(&row.name);
            let label = format!(
                "{}{name}",
                if first {
                    format!("{group} · ")
                } else {
                    String::new()
                }
            );
            out.push_str(&format!(
                "{}\n",
                Label::new(&label).render(mark, &row.effective)
            ));
        } else {
            out.push_str(&format!(
                "{mark} {:<32} {:<14} {:<14} {}\n",
                row.name, row.persisted, row.effective, row.source
            ));
        }
        if index == settings.sel {
            if let Some(select) = select {
                for option in select.render() {
                    out.push_str(&option);
                    out.push('\n');
                }
            }
        }
    }
    if narrow {
        if let Some(row) = selected {
            out.push_str(&format!(
                "\n{}\npersisted: {} · effective: {} ({})\n",
                row.name, row.persisted, row.effective, row.source
            ));
        }
    }
    if let Some((error, message)) = &settings.message {
        out.push_str(&format!(
            "\n{}{}",
            if *error { "error: " } else { "" },
            message
        ));
    } else if select.is_some() {
        out.push_str("\n↑↓ choose · Enter save · Esc cancel");
    } else {
        out.push_str("\n↑↓ move · Enter edit · Esc back");
    }
    clip_frame(&out, cols, rows.saturating_sub(1))
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
                    (Action::Settings, "application settings".into()),
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
            Some(Overlay::Settings(settings)) => {
                let (cols, rows) = self
                    .daemon
                    .as_ref()
                    .map(|daemon| daemon.size)
                    .unwrap_or_else(term_size);
                render_settings(settings, &self.settings.settings, cols, rows)
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
        if matches!(self.overlay, Some(Overlay::Settings(_))) {
            self.settings_key(key);
            return;
        }
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
        self.use_key_table("agenmux");
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

    pub(super) fn settings_editing(&self) -> bool {
        matches!(
            &self.overlay,
            Some(Overlay::Settings(Settings { edit: Some(_), .. }))
        )
    }

    fn use_key_table(&mut self, table: &str) {
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
                let _ = command_spawn(&["switch-client", "-c", client, "-T", table]);
            }
        }
    }

    fn apply_settings_source(
        &mut self,
        old_source: &str,
        old_existed: bool,
        source: &str,
    ) -> Result<(), String> {
        let path = crate::app_config::config_path().ok_or("no configuration path")?;
        let file = crate::app_config::parse(source).map_err(|error| error.to_string())?;
        let options = crate::app_config::read_options(|command| self.tmux.run(command))
            .map_err(|error| error.to_string())?;
        let next = self
            .settings
            .resolve(&file, &options)
            .map_err(|error| error.to_string())?;
        let old_file = crate::app_config::parse(old_source).map_err(|error| error.to_string())?;
        let old = self
            .settings
            .resolve(&old_file, &options)
            .map_err(|error| error.to_string())?;
        crate::app_config::save_document(&path, source).map_err(|error| error.to_string())?;
        let rollback = || -> Result<(), String> {
            if old_existed {
                crate::app_config::save_document(&path, old_source)
                    .map_err(|error| error.to_string())?;
            } else if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error.to_string());
                }
            }
            if crate::setup::run_config(&self.plugin_dir, &old) != 0 {
                return Err("could not restore previous key tables".into());
            }
            Ok(())
        };
        if crate::setup::run_config(&self.plugin_dir, &next) != 0 {
            return Err(match rollback() {
                Ok(()) => "could not apply settings; previous settings restored".into(),
                Err(error) => format!("could not apply settings; rollback failed: {error}"),
            });
        }
        let token = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos().to_string())
            .unwrap_or_else(|_| "settings".into());
        if self
            .tmux
            .run(&format!(
                "set-option -g {} {token}",
                crate::app_config::RELOAD_OPTION
            ))
            .is_err()
        {
            return Err(match rollback() {
                Ok(()) => "could not publish settings; previous settings restored".into(),
                Err(error) => format!("could not publish settings; rollback failed: {error}"),
            });
        }
        let width_changed = next.sidebar_width != self.settings.settings.sidebar_width;
        self.settings.replace(file, next);
        if width_changed && self.daemon.is_some() {
            let panes = self
                .tmux
                .run("list-panes -a -f '#{==:#{pane_title},agenmux}' -F '#{pane_id}\t#{window_width}'")
                .unwrap_or_default();
            for line in panes.lines() {
                let Some((pane, window_width)) = line.split_once('\t') else {
                    continue;
                };
                let width = window_width
                    .parse::<usize>()
                    .unwrap_or(usize::MAX)
                    .saturating_sub(2)
                    .max(1)
                    .min(self.settings.settings.sidebar_width as usize)
                    .to_string();
                let _ = crate::tmux::command_status(&["resize-pane", "-t", pane, "-x", &width]);
            }
        }
        self.adopt_reload(crate::app_config::Refreshed {
            width_changed,
            reloaded: true,
        });
        Ok(())
    }

    fn settings_key(&mut self, key: Key) {
        let rows = match &self.overlay {
            Some(Overlay::Settings(settings)) => {
                settings_rows(&settings.source, &self.settings.settings)
            }
            _ => return,
        };
        let mut candidate = None;
        let mut close = false;
        let mut editing = false;
        if let Some(Overlay::Settings(settings)) = &mut self.overlay {
            settings.message = None;
            if settings.confirm {
                match key {
                    Key::Jump => match crate::app_config::revert_document(&settings.source) {
                        Ok(source) => candidate = Some(source),
                        Err(error) => settings.message = Some((true, error.to_string())),
                    },
                    Key::AllStates | Key::Quit | Key::Close => settings.confirm = false,
                    _ => {}
                }
            } else if let Some(edit) = &mut settings.edit {
                match key {
                    Key::Text(text) => {
                        if let Editor::TextEdit(editor) = &mut edit.editor {
                            editor.push(&text);
                        }
                    }
                    Key::Backspace => {
                        if let Editor::TextEdit(editor) = &mut edit.editor {
                            editor.backspace();
                        }
                    }
                    Key::ClearSearch => {
                        if let Editor::TextEdit(editor) = &mut edit.editor {
                            editor.clear();
                        }
                    }
                    Key::Up | Key::Down => {
                        if let Editor::Select(select) = &mut edit.editor {
                            select.move_by(if matches!(key, Key::Up) { -1 } else { 1 });
                        }
                    }
                    Key::Jump => {
                        match setting_value(&edit.name, edit.editor.value()).and_then(|value| {
                            crate::app_config::edit_document(
                                &settings.source,
                                &edit.name,
                                Some(&value),
                            )
                            .map_err(|error| error.to_string())
                        }) {
                            Ok(source) => candidate = Some(source),
                            Err(error) => settings.message = Some((true, error)),
                        }
                    }
                    Key::AllStates | Key::Quit | Key::Close => settings.edit = None,
                    _ => {}
                }
                editing = settings.edit.is_some() && candidate.is_none();
            } else {
                match key {
                    Key::Down if !rows.is_empty() => {
                        settings.sel = (settings.sel + 1).min(rows.len())
                    }
                    Key::Up => settings.sel = settings.sel.saturating_sub(1),
                    Key::Jump if settings.sel == rows.len() => settings.confirm = true,
                    Key::Jump => {
                        if let Some(row) = rows.get(settings.sel) {
                            let mut value = if row.persisted == "—" {
                                row.effective.clone()
                            } else {
                                row.persisted.clone()
                            };
                            if row.name.starts_with("theme.colors.") && value == "terminal" {
                                value = "default".into();
                            }
                            let editor = choices(&row.name).map_or_else(
                                || Editor::TextEdit(TextEdit::new(value.clone())),
                                |options| Editor::Select(Select::new(&value, options)),
                            );
                            settings.edit = Some(SettingEdit {
                                name: row.name.clone(),
                                editor,
                            });
                            editing = true;
                        }
                    }
                    Key::AllStates | Key::Quit | Key::Close => close = true,
                    _ => {}
                }
            }
        }
        if let Some(source) = candidate {
            let (old_source, old_existed) = match &self.overlay {
                Some(Overlay::Settings(settings)) => (settings.source.clone(), settings.existed),
                _ => return,
            };
            let result = self.apply_settings_source(&old_source, old_existed, &source);
            if let Some(Overlay::Settings(settings)) = &mut self.overlay {
                match result {
                    Ok(()) => {
                        settings.source = source;
                        settings.existed = true;
                        settings.edit = None;
                        settings.confirm = false;
                        settings.message = Some((false, "saved".into()));
                    }
                    Err(error) => settings.message = Some((true, error)),
                }
            }
        }
        if close {
            self.close_overlay();
        } else if editing {
            self.use_key_table("agenmux-settings-edit");
        } else {
            self.reclaim_key_table();
        }
        self.last_frame.clear();
    }

    pub(super) fn settings(&mut self) {
        let (source, existed, message) = match crate::app_config::document() {
            Ok((_, existed, source)) => (source, existed, None),
            Err(error) => (
                "version = 1\n".into(),
                false,
                Some((true, error.to_string())),
            ),
        };
        self.overlay = Some(Overlay::Settings(Settings {
            sel: 0,
            scroll: 0,
            source,
            existed,
            edit: None,
            confirm: false,
            message,
        }));
        self.last_frame.clear();
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

    #[test]
    fn settings_frames_fit_wide_narrow_and_tiny_views() {
        let effective =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        for (cols, rows) in [(100, 30), (40, 16), (24, 7)] {
            let mut settings = Settings {
                sel: 0,
                scroll: 0,
                source: "version = 1\n".into(),
                existed: false,
                edit: None,
                confirm: false,
                message: None,
            };
            let frame = render_settings(&mut settings, &effective, cols, rows);
            assert!(frame.lines().count() <= rows);
            assert!(frame.lines().all(|line| line.chars().count() <= cols + 20));
            assert!(frame.contains("agenmux"));
            settings.sel = settings_rows(&settings.source, &effective).len();
            let end = render_settings(&mut settings, &effective, cols, rows);
            assert!(end.contains("Revert to defaults"));
        }
    }

    #[test]
    fn settings_values_validate_and_revert_confirmation_names_overrides() {
        assert_eq!(setting_value("display.sidebar_width", "44").unwrap(), "44");
        assert!(setting_value("display.sidebar_width", "0").is_err());
        assert_eq!(
            setting_value("keys.normal.down", "j, Down").unwrap(),
            "[\"j\", \"Down\"]"
        );
        let effective =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let mut settings = Settings {
            sel: 0,
            scroll: 0,
            source: "version = 1\n".into(),
            existed: false,
            edit: None,
            confirm: true,
            message: None,
        };
        let frame = render_settings(&mut settings, &effective, 50, 14);
        assert!(frame.contains("Revert to defaults"));
        assert!(frame.contains("CLI and tmux overrides remain effective"));
        let file = crate::app_config::parse("[display]\nsidebar_width = 44\n").unwrap();
        let options =
            std::collections::BTreeMap::from([("@agenmux-width".to_string(), "55".to_string())]);
        let overridden = crate::app_config::resolve(&file, &options).unwrap();
        let row = settings_rows("[display]\nsidebar_width = 44\n", &overridden)
            .into_iter()
            .find(|row| row.name == "display.sidebar_width")
            .unwrap();
        assert_eq!(
            (row.persisted.as_str(), row.effective.as_str()),
            ("44", "55")
        );
        assert_eq!(row.source, "tmux @agenmux-width");
    }

    #[test]
    fn closed_settings_expand_inline() {
        let effective =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let sel = settings_rows("version = 1\n", &effective)
            .iter()
            .position(|row| row.name == "display.mode")
            .unwrap();
        let mut settings = Settings {
            sel,
            scroll: 0,
            source: "version = 1\n".into(),
            existed: false,
            edit: Some(SettingEdit {
                name: "display.mode".into(),
                editor: Editor::Select(Select::new("split", choices("display.mode").unwrap())),
            }),
            confirm: false,
            message: None,
        };

        let frame = render_settings(&mut settings, &effective, 50, 18);

        assert!(frame.contains("Display · mode: split"), "{frame}");
        assert!(frame.contains("❯ split"), "{frame}");
        assert!(frame.contains("  popup"), "{frame}");
        assert!(frame.contains("sidebar_width"), "{frame}");
    }
}
