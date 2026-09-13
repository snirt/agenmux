use crate::app_config::{action_for, Action, KeyChord};
use crate::input::{available_sequences, term_size, Key, SequenceAction};
use crate::release;
use crate::tmux::{command, command_spawn};
use std::path::{Path, PathBuf};

use super::render::{app_title, clip_frame, cursor_mark, join};
use super::ui::{bar, Action as UiAction, Editor, Label, Select, SelectedRow, TextEdit, TopBar};
use super::{MutationTarget, Sidebar, E};

pub(super) enum Overlay {
    Help,
    Versions {
        sel: usize,
        chosen: Option<String>,
    },
    Settings(Settings),
    Create {
        target: MutationTarget,
        name: String,
    },
    RenameScope(MutationTarget),
    Rename {
        target: MutationTarget,
        name: String,
    },
    Confirm(MutationTarget),
}

pub(super) struct Settings {
    sel: usize,
    scroll: usize,
    source: String,
    path: PathBuf,
    existed: bool,
    editable: bool,
    edit: Option<SettingEdit>,
    search: Option<TextEdit>,
    search_editing: bool,
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
    initial: String,
    effective: String,
    source: String,
}

const SETTINGS_MOUSE_OPTION: usize = 1 << 31;
const SETTINGS_MOUSE_SEARCH: usize = u32::MAX as usize;

fn settings_mouse_key(settings: &mut Settings, key: Key, total: usize) -> Key {
    let Key::Select(target) = key else {
        return key;
    };
    if target == SETTINGS_MOUSE_SEARCH {
        return if settings.edit.is_none() && !settings.confirm {
            Key::Search
        } else {
            Key::Other
        };
    }
    if let Some(SettingEdit {
        editor: Editor::Select(select),
        ..
    }) = &mut settings.edit
    {
        return target
            .checked_sub(SETTINGS_MOUSE_OPTION)
            .is_some_and(|option| select.select(option))
            .then_some(Key::Jump)
            .unwrap_or(Key::Other);
    }
    if settings.edit.is_none() && !settings.confirm && target < total {
        if target == settings.sel {
            Key::Jump
        } else {
            settings.sel = target;
            Key::Other
        }
    } else {
        Key::Other
    }
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
pub(super) fn update_available(plugin_dir: &Path) -> Option<String> {
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
fn known_tags(plugin_dir: &Path) -> Vec<String> {
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
    let persisted_config = crate::app_config::parse(source)
        .and_then(|file| crate::app_config::resolve(&file, &Default::default()))
        .ok();
    let persisted = persisted_config
        .as_ref()
        .map(crate::app_config::rows)
        .unwrap_or_default();
    crate::app_config::rows(effective)
        .into_iter()
        .map(|row| {
            let file = persisted
                .iter()
                .find(|candidate| candidate.name == row.name)
                .filter(|candidate| candidate.source == "file");
            let initial = if row.name == "behavior.hide_windows" {
                if file.is_some() {
                    persisted_config
                        .as_ref()
                        .and_then(|config| config.hide_windows.clone())
                        .unwrap_or_default()
                } else {
                    effective.hide_windows.clone().unwrap_or_default()
                }
            } else {
                file.map(|candidate| candidate.value.clone())
                    .unwrap_or_else(|| row.value.clone())
            };
            SettingRow {
                name: row.name,
                persisted: file
                    .map(|candidate| candidate.value.clone())
                    .unwrap_or_else(|| "—".into()),
                initial,
                effective: row.value,
                source: row.source,
            }
        })
        .collect()
}

fn visible_settings_rows(
    settings: &Settings,
    effective: &crate::app_config::AppConfig,
) -> Vec<SettingRow> {
    let query = settings
        .search
        .as_ref()
        .map(TextEdit::value)
        .unwrap_or_default()
        .to_ascii_lowercase();
    settings_rows(&settings.source, effective)
        .into_iter()
        .filter(|row| query.is_empty() || row.name.to_ascii_lowercase().contains(&query))
        .collect()
}

fn setting_group(name: &str) -> &'static str {
    if name.starts_with("display.") {
        "Display"
    } else if name.starts_with("behavior.") {
        "Behavior"
    } else if name.starts_with("tmux_management.") {
        "Tmux management"
    } else if name == "theme.base" {
        "Theme"
    } else if name.starts_with("theme.colors.") {
        "Colors"
    } else {
        "Keymap"
    }
}

fn setting_label(name: &str) -> &str {
    name.strip_prefix("keys.")
        .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name))
}

fn setting_value(name: &str, buffer: &str) -> Result<String, String> {
    let value = buffer.trim();
    if name.starts_with("keys.") {
        let mut array = toml_edit::Array::new();
        if value == "," {
            array.push(value);
        } else if !value.is_empty() {
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
        let number = value
            .parse::<u16>()
            .ok()
            .filter(|number| (1..=10000).contains(number))
            .ok_or_else(|| "expected 1..=10000".to_string())?;
        return Ok(number.to_string());
    }
    if name.starts_with("theme.colors.") {
        if let Ok(color) = value.parse::<u8>() {
            return Ok(color.to_string());
        }
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

fn initial_setting_value(row: &SettingRow) -> String {
    if matches!(row.initial.as_str(), "(unset)" | "(unbound)") {
        return String::new();
    }
    if row.name.starts_with("theme.colors.") && row.initial == "terminal" {
        return "default".into();
    }
    row.initial.clone()
}

fn select_move(
    key: &Key,
    normal: &std::collections::BTreeMap<Action, Vec<KeyChord>>,
) -> Option<isize> {
    match key {
        Key::Up | Key::WheelUp => Some(-1),
        Key::Down | Key::WheelDown => Some(1),
        Key::Text(text) => match action_for(normal, KeyChord::parse(text).ok()?) {
            Some(Action::Up) => Some(-1),
            Some(Action::Down) => Some(1),
            _ => None,
        },
        _ => None,
    }
}

fn settings_state(
    document: Result<(PathBuf, bool, String), crate::app_config::ConfigError>,
) -> Settings {
    let (path, source, existed, editable, message) = match document {
        Ok((path, existed, source)) => (path, source, existed, true, None),
        Err(error) => (
            crate::app_config::config_path().unwrap_or_default(),
            "version = 1\n".into(),
            false,
            false,
            Some((true, error.to_string())),
        ),
    };
    Settings {
        sel: 0,
        scroll: 0,
        source,
        path,
        existed,
        editable,
        edit: None,
        search: None,
        search_editing: false,
        confirm: false,
        message,
    }
}

fn render_settings(
    settings: &mut Settings,
    effective: &crate::app_config::AppConfig,
    palette: &crate::app_config::Palette,
    focused: bool,
    cols: usize,
    rows: usize,
) -> (String, Vec<Option<usize>>) {
    let all = visible_settings_rows(settings, effective);
    let search_query = settings
        .search
        .as_ref()
        .map(TextEdit::value)
        .unwrap_or_default();
    let show_revert = search_query.is_empty();
    let total = all.len() + usize::from(show_revert);
    settings.sel = if total == 0 {
        0
    } else {
        settings.sel.min(total - 1)
    };
    let selected = all.get(settings.sel);
    let mut out = format!("{E}[2J{E}[H{E}[1m{} — settings{E}[0m\n", app_title());
    let mut targets = Vec::new();
    if settings.confirm {
        out.push_str("\nRevert to defaults?\n\nPersisted customizations will be removed.\nCLI and tmux overrides remain effective.\n\nEnter confirm · Esc cancel");
        return (clip_frame(&out, cols, rows.saturating_sub(1)), targets);
    }
    if !settings.editable {
        if let Some((_, message)) = &settings.message {
            out.push_str(&format!("\nerror: {message}\n\nEsc back"));
        }
        return (clip_frame(&out, cols, rows.saturating_sub(1)), targets);
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
        return (clip_frame(&out, cols, rows.saturating_sub(1)), targets);
    }
    let select = settings.edit.as_ref().and_then(|edit| match &edit.editor {
        Editor::Select(select) => Some(select),
        Editor::TextEdit(_) => None,
    });
    if let Some(search) = &settings.search {
        out.push_str(&format!(
            "\n/ {}{}\n",
            search.value(),
            if settings.search_editing { "_" } else { "" }
        ));
    } else {
        out.push_str("\n/ search\n");
    }
    targets.extend([None, Some(SETTINGS_MOUSE_SEARCH)]);
    let narrow = cols < 80;
    if !narrow {
        out.push_str("  setting                    persisted      effective      source\n");
        targets.push(None);
    }
    // Reserve chrome, category headers, and expanded options before computing
    // selectable rows so short panes keep the active control visible.
    let groups = all
        .iter()
        .map(|row| setting_group(&row.name))
        .fold(Vec::<&str>::new(), |mut groups, group| {
            if groups.last().copied() != Some(group) {
                groups.push(group);
            }
            groups
        })
        .len();
    let fixed_rows = if narrow { 8 } else { 6 };
    let height = rows
        .saturating_sub(fixed_rows + groups + select.map_or(0, Select::height))
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
            let line = UiAction::new("Revert to defaults").render(index == settings.sel);
            out.push_str(
                &SelectedRow::new(palette, index == settings.sel, focused).render(&line, cols),
            );
            out.push('\n');
            targets.push(Some(index));
            continue;
        }
        let row = &all[index];
        let group = setting_group(&row.name);
        let first = group != last_group;
        last_group = group;
        let mark = if index == settings.sel { "❯" } else { " " };
        if first {
            out.push_str(&format!("  {E}[7m {group} {E}[0m\n"));
            targets.push(None);
        }
        let name = setting_label(&row.name);
        let line = if narrow {
            Label::new(name).render(mark, &row.effective)
        } else {
            format!(
                "{mark} {:<26} {:<14} {:<14} {}",
                name, row.persisted, row.effective, row.source
            )
        };
        out.push_str(
            &SelectedRow::new(palette, index == settings.sel, focused).render(&line, cols),
        );
        out.push('\n');
        targets.push(Some(index));
        if index == settings.sel {
            if let Some(select) = select {
                for (option_index, option) in select.render().into_iter().enumerate() {
                    out.push_str(&option);
                    out.push('\n');
                    targets.push(Some(SETTINGS_MOUSE_OPTION + option_index));
                }
            }
        }
    }
    if all.is_empty() && !search_query.is_empty() {
        out.push_str("  No settings match\n");
        targets.push(None);
    }
    if narrow {
        if let Some(row) = selected {
            out.push_str(&format!(
                "\n{}\npersisted: {} · effective: {} ({})\n",
                row.name, row.persisted, row.effective, row.source
            ));
            targets.extend([None, None, None]);
        }
    }
    if let Some((error, message)) = &settings.message {
        out.push_str(&format!(
            "\n{}{}",
            if *error { "error: " } else { "" },
            message
        ));
    } else if select.is_some() {
        out.push_str(if narrow {
            "\njk choose · Enter save · Esc cancel"
        } else {
            "\n↑↓/jk choose · Enter save · Esc cancel"
        });
    } else if settings.search_editing {
        out.push_str("\ntype to filter · Enter apply · Esc clear");
    } else if settings.search.is_some() {
        out.push_str(if narrow {
            "\njk move · Enter edit · / refine · Esc clear"
        } else {
            "\n↑↓/jk move · Enter edit · / refine · Esc clear"
        });
    } else {
        out.push_str(if narrow {
            "\njk move · Enter edit · / find · Esc back"
        } else {
            "\n↑↓/jk move · Enter edit · / search · Esc back"
        });
    }
    targets.extend([None, None]);
    let frame = clip_frame(&out, cols, rows.saturating_sub(1));
    targets.truncate(frame.lines().count().saturating_sub(1));
    (frame, targets)
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
        let top_bar = TopBar::new(&self.palette, self.plugin_selected, self.header_inherited);
        let header = top_bar.foreground("1");
        let muted = self.palette.muted_fg.fg("2");
        let idle = self.palette.idle_fg.fg("");
        let working = self.palette.working_fg.fg("");
        let blocked = self.palette.blocked_fg.fg("");
        let done = self.palette.done_fg.fg("");
        let error = self.palette.error_fg.fg("2");
        let mut click_rows = String::new();
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
                for binding in available_sequences(self.settings.settings.tmux_management_enabled) {
                    let prefix = binding.sequence.as_bytes()[0];
                    if action_for(&self.normal_keys, KeyChord::Printable(prefix)).is_none() {
                        keys.push((binding.sequence.into(), binding.label.into()));
                    }
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
                let (text, targets) = render_settings(
                    settings,
                    &self.settings.settings,
                    &self.palette,
                    self.plugin_selected,
                    cols,
                    rows,
                );
                click_rows = targets
                    .into_iter()
                    .map(|target| match target {
                        Some(index) => format!("=\t{index}\t0\n"),
                        None => "-\n".into(),
                    })
                    .collect();
                text
            }
            Some(Overlay::Create { target, name }) => {
                let action = match target.action {
                    SequenceAction::CreateWindow => "create window",
                    SequenceAction::CreateSession => "create session",
                    _ => return,
                };
                let name: String = name.chars().filter(|c| !c.is_control()).collect();
                let hint = join(&[
                    self.hint(&self.search_keys, Action::Accept, "create"),
                    self.hint(&self.search_keys, Action::Cancel, "cancel"),
                ]);
                format!(
                    "{E}[2J{E}[H{header}{title} — {action}{E}[0m\n\n\
                     name (optional): {name}\n\n{muted}{hint}{E}[0m"
                )
            }
            Some(Overlay::RenameScope(_)) => format!(
                "{E}[2J{E}[H{header}{title} — rename{E}[0m\n\n\
                 rename:\n\n\
                 p  pane\n\
                 w  window\n\
                 s  session\n\n{muted}p/w/s choose · Esc cancel{E}[0m"
            ),
            Some(Overlay::Rename { target, name, .. }) => {
                let kind = match target.action {
                    SequenceAction::RenamePane => "pane",
                    SequenceAction::RenameWindow => "window",
                    SequenceAction::RenameSession => "session",
                    _ => return,
                };
                let name: String = name.chars().filter(|c| !c.is_control()).collect();
                let hint = join(&[
                    self.hint(&self.search_keys, Action::Accept, "rename"),
                    self.hint(&self.search_keys, Action::Cancel, "cancel"),
                ]);
                format!(
                    "{E}[2J{E}[H{header}{title} — rename {kind}{E}[0m\n\n\
                     name: {name}▏\n\n{muted}{hint}{E}[0m"
                )
            }
            Some(Overlay::Confirm(target)) => {
                let (kind, identity) = match target.action {
                    SequenceAction::DeletePane => ("pane", &target.pane_id),
                    SequenceAction::DeleteWindow => ("window", &target.window_id),
                    SequenceAction::DeleteSession => ("session", &target.session_id),
                    _ => return,
                };
                format!(
                    "{E}[2J{E}[H{header}{title} — delete {kind}{E}[0m\n\n\
                     delete {kind} {identity}? [y/N]\n\n{muted}y delete · Enter/n/Esc cancel{E}[0m"
                )
            }
            None => return,
        };
        let header_bg = top_bar.background();
        let text = if header_bg.is_empty() {
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
                bar(header, &header_bg, cols, width)
            )
        };
        self.emit(text, &click_rows, force);
    }

    fn open_rename_input(&mut self, mut target: MutationTarget, action: SequenceAction) {
        target.action = action;
        let (id, format) = match action {
            SequenceAction::RenamePane => (target.pane_id.as_str(), "#{pane_title}"),
            SequenceAction::RenameWindow => (target.window_id.as_str(), "#{window_name}"),
            SequenceAction::RenameSession => (target.session_id.as_str(), "#{session_name}"),
            _ => return,
        };
        match command(&["display-message", "-p", "-t", id, format]) {
            Ok(name) => {
                let name = name
                    .trim_end_matches(['\r', '\n'])
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect();
                self.overlay = Some(Overlay::Rename { target, name });
            }
            Err(error) => {
                self.restore_mutation_input(&target.client);
                self.mutation_error(&target.client, &error.to_string());
            }
        }
    }

    pub(super) fn overlay_key(&mut self, key: Key) -> super::DispatchResult {
        if matches!(self.overlay, Some(Overlay::Settings(_))) {
            self.settings_key(key);
            return super::DispatchResult::Continue;
        }
        let Some(overlay) = self.overlay.take() else {
            return super::DispatchResult::Continue;
        };
        match overlay {
            Overlay::Settings(_) => unreachable!("settings keys are routed above"),
            Overlay::Help => self.close_overlay(),
            Overlay::Versions {
                mut sel,
                mut chosen,
            } => {
                let tags = known_tags(&self.plugin_dir);
                let cur = current_tag();
                sel = picker_sel(&tags, &cur, chosen.as_deref(), sel);
                match key {
                    Key::Down if !tags.is_empty() => sel = (sel + 1).min(tags.len() - 1),
                    Key::Up => sel = sel.saturating_sub(1),
                    Key::Jump => {
                        if let Some(tag) = tags.get(sel).filter(|tag| **tag != cur) {
                            self.switch_version(tag);
                        }
                        self.close_overlay();
                        return super::DispatchResult::Continue;
                    }
                    Key::Quit | Key::Close => {
                        self.close_overlay();
                        return super::DispatchResult::Continue;
                    }
                    _ => {}
                }
                chosen = tags.get(sel).cloned();
                self.overlay = Some(Overlay::Versions { sel, chosen });
            }
            Overlay::Create { target, mut name } => match key {
                Key::Text(text) => {
                    name.extend(text.chars().filter(|c| !c.is_control()));
                    name.truncate(128);
                    self.overlay = Some(Overlay::Create { target, name });
                }
                Key::Backspace => {
                    name.pop();
                    self.overlay = Some(Overlay::Create { target, name });
                }
                Key::Jump => return self.execute_mutation(&target, &name),
                Key::AllStates | Key::ClearSearch | Key::Quit | Key::Close => {
                    self.restore_mutation_input(&target.client);
                }
                _ => self.overlay = Some(Overlay::Create { target, name }),
            },
            Overlay::RenameScope(target) => match key {
                Key::Text(scope) if scope.eq_ignore_ascii_case("p") => {
                    self.open_rename_input(target, SequenceAction::RenamePane);
                }
                Key::Text(scope) if scope.eq_ignore_ascii_case("w") => {
                    self.open_rename_input(target, SequenceAction::RenameWindow);
                }
                Key::Text(scope) if scope.eq_ignore_ascii_case("s") => {
                    self.open_rename_input(target, SequenceAction::RenameSession);
                }
                Key::AllStates | Key::ClearSearch | Key::Quit | Key::Close => {
                    self.restore_mutation_input(&target.client);
                }
                _ => self.overlay = Some(Overlay::RenameScope(target)),
            },
            Overlay::Rename { target, mut name } => match key {
                Key::Text(text) => {
                    for character in text.chars().filter(|c| !c.is_control()) {
                        if name.chars().count() >= 128 {
                            break;
                        }
                        name.push(character);
                    }
                    self.overlay = Some(Overlay::Rename { target, name });
                }
                Key::Backspace => {
                    name.pop();
                    self.overlay = Some(Overlay::Rename { target, name });
                }
                Key::Jump if name.trim().is_empty() => {
                    self.restore_mutation_input(&target.client);
                }
                Key::Jump => return self.execute_mutation(&target, &name),
                Key::AllStates | Key::ClearSearch | Key::Quit | Key::Close => {
                    self.restore_mutation_input(&target.client);
                }
                _ => self.overlay = Some(Overlay::Rename { target, name }),
            },
            Overlay::Confirm(target) => {
                let confirmed =
                    matches!(key, Key::Text(ref text) if text.eq_ignore_ascii_case("y"));
                if confirmed {
                    return self.execute_mutation(&target, "");
                }
                self.restore_mutation_input(&target.client);
            }
        }
        self.last_frame.clear();
        super::DispatchResult::Continue
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
                | Some(Overlay::Settings(Settings {
                    search_editing: true,
                    ..
                }))
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
        path: &std::path::Path,
        old_source: &str,
        old_existed: bool,
        source: &str,
    ) -> Result<(), String> {
        let (current_path, current_existed, current_source) =
            crate::app_config::document().map_err(|error| error.to_string())?;
        if current_path != path || current_existed != old_existed || current_source != old_source {
            return Err("configuration changed on disk; reopen settings".into());
        }
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
        crate::app_config::save_document(path, source).map_err(|error| error.to_string())?;
        let rollback = || -> Result<(), String> {
            if old_existed {
                crate::app_config::save_document(path, old_source)
                    .map_err(|error| error.to_string())?;
            } else if let Err(error) = std::fs::remove_file(path) {
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
        let read_only = matches!(
            &self.overlay,
            Some(Overlay::Settings(Settings {
                editable: false,
                ..
            }))
        );
        if read_only {
            if matches!(key, Key::AllStates | Key::Quit | Key::Close) {
                self.close_overlay();
            }
            self.last_frame.clear();
            return;
        }
        let (rows, show_revert) = match &self.overlay {
            Some(Overlay::Settings(settings)) => (
                visible_settings_rows(settings, &self.settings.settings),
                settings
                    .search
                    .as_ref()
                    .map(TextEdit::value)
                    .unwrap_or_default()
                    .is_empty(),
            ),
            _ => return,
        };
        let key = match &mut self.overlay {
            Some(Overlay::Settings(settings)) => {
                settings_mouse_key(settings, key, rows.len() + usize::from(show_revert))
            }
            _ => return,
        };
        let search_start = matches!(key, Key::Search)
            || matches!(
                &key,
                Key::Text(text)
                    if KeyChord::parse(text)
                        .ok()
                        .and_then(|chord| action_for(&self.settings.settings.normal, chord))
                        == Some(Action::Search)
            );
        let select_delta = select_move(&key, &self.settings.settings.normal);
        let mut candidate = None;
        let mut close = false;
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
                    Key::Text(text) => match &mut edit.editor {
                        Editor::TextEdit(editor) => editor.push(&text),
                        Editor::Select(select) => {
                            if let Some(delta) = select_delta {
                                select.move_by(delta);
                            }
                        }
                    },
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
                        if let (Editor::Select(select), Some(delta)) =
                            (&mut edit.editor, select_delta)
                        {
                            select.move_by(delta);
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
            } else {
                match key {
                    Key::Text(text) if settings.search_editing => {
                        settings.search.as_mut().unwrap().push(&text);
                        settings.sel = 0;
                        settings.scroll = 0;
                    }
                    Key::Backspace if settings.search_editing => {
                        settings.search.as_mut().unwrap().backspace();
                        settings.sel = 0;
                        settings.scroll = 0;
                    }
                    Key::ClearSearch if settings.search_editing => {
                        settings.search.as_mut().unwrap().clear();
                        settings.sel = 0;
                        settings.scroll = 0;
                    }
                    Key::Jump if settings.search_editing => {
                        settings.search_editing = false;
                        settings.sel = 0;
                        settings.scroll = 0;
                    }
                    _ if search_start => {
                        if settings.search.is_none() {
                            settings.search = Some(TextEdit::new(String::new()));
                        }
                        settings.search_editing = true;
                        settings.sel = 0;
                        settings.scroll = 0;
                    }
                    _ if select_delta == Some(1) && !rows.is_empty() => {
                        let last = rows.len() + usize::from(show_revert) - 1;
                        settings.sel = (settings.sel + 1).min(last);
                    }
                    _ if select_delta == Some(-1) => {
                        settings.sel = settings.sel.saturating_sub(1);
                    }
                    Key::Jump if show_revert && settings.sel == rows.len() => {
                        settings.confirm = true
                    }
                    Key::Jump => {
                        if let Some(row) = rows.get(settings.sel) {
                            let value = initial_setting_value(row);
                            let editor = choices(&row.name).map_or_else(
                                || Editor::TextEdit(TextEdit::new(value.clone())),
                                |options| Editor::Select(Select::new(&value, options)),
                            );
                            settings.edit = Some(SettingEdit {
                                name: row.name.clone(),
                                editor,
                            });
                        }
                    }
                    Key::AllStates | Key::Quit | Key::Close if settings.search.is_some() => {
                        settings.search = None;
                        settings.search_editing = false;
                        settings.sel = 0;
                        settings.scroll = 0;
                    }
                    Key::AllStates | Key::Quit | Key::Close => close = true,
                    _ => {}
                }
            }
        }
        if let Some(source) = candidate {
            let (path, old_source, old_existed) = match &self.overlay {
                Some(Overlay::Settings(settings)) => (
                    settings.path.clone(),
                    settings.source.clone(),
                    settings.existed,
                ),
                _ => return,
            };
            let result = self.apply_settings_source(&path, &old_source, old_existed, &source);
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
        let interacting = matches!(
            &self.overlay,
            Some(Overlay::Settings(Settings { edit: Some(_), .. }))
                | Some(Overlay::Settings(Settings {
                    search_editing: true,
                    ..
                }))
        );
        if close {
            self.close_overlay();
        } else if interacting {
            self.use_key_table("agenmux-settings-edit");
        } else {
            self.reclaim_key_table();
        }
        self.last_frame.clear();
    }

    pub(super) fn settings(&mut self) {
        self.overlay = Some(Overlay::Settings(settings_state(
            crate::app_config::document(),
        )));
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
                path: PathBuf::new(),
                existed: false,
                editable: true,
                edit: None,
                search: None,
                search_editing: false,
                confirm: false,
                message: None,
            };
            let (frame, targets) = render_settings(
                &mut settings,
                &effective,
                &crate::app_config::Palette::default(),
                true,
                cols,
                rows,
            );
            assert!(frame.lines().count() <= rows);
            assert_eq!(targets.len(), frame.lines().count().saturating_sub(1));
            assert!(targets.iter().any(Option::is_some));
            assert_eq!(
                targets.iter().flatten().next(),
                Some(&SETTINGS_MOUSE_SEARCH)
            );
            assert!(frame.lines().all(|line| line.chars().count() <= cols + 20));
            assert!(frame.contains("agenmux"));
            if rows >= 16 {
                assert!(frame.contains("Esc back"), "{frame}");
                settings.message = Some((false, "saved".into()));
                let (saved, _) = render_settings(
                    &mut settings,
                    &effective,
                    &crate::app_config::Palette::default(),
                    true,
                    cols,
                    rows,
                );
                assert!(saved.contains("saved"), "{saved}");
            }
            settings.sel = settings_rows(&settings.source, &effective).len();
            let (end, _) = render_settings(
                &mut settings,
                &effective,
                &crate::app_config::Palette::default(),
                true,
                cols,
                rows,
            );
            assert!(end.contains("Revert to defaults"));
        }
    }

    #[test]
    fn settings_search_matches_names_and_keeps_category_headers() {
        let effective =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let mut settings = settings_state(Ok((PathBuf::new(), false, "version = 1\n".into())));
        settings.search = Some(TextEdit::new("sidebar".into()));
        let visible = visible_settings_rows(&settings, &effective);
        assert_eq!(
            visible
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            vec!["display.sidebar_width"]
        );
        let (frame, _) = render_settings(
            &mut settings,
            &effective,
            &crate::app_config::Palette::default(),
            true,
            80,
            20,
        );
        assert!(frame.contains("Display"), "{frame}");
        assert!(frame.contains("\u{1b}[7m Display "), "{frame}");
        assert!(frame.contains("sidebar_width"), "{frame}");

        settings.search = Some(TextEdit::new("split".into()));
        assert!(visible_settings_rows(&settings, &effective).is_empty());
        let (empty, _) = render_settings(
            &mut settings,
            &effective,
            &crate::app_config::Palette::default(),
            true,
            80,
            20,
        );
        assert!(empty.contains("No settings match"), "{empty}");
    }

    #[test]
    fn settings_values_validate_and_revert_confirmation_names_overrides() {
        assert_eq!(setting_value("display.sidebar_width", "44").unwrap(), "44");
        assert_eq!(
            setting_value("display.sidebar_width", "0044").unwrap(),
            "44"
        );
        assert!(setting_value("display.sidebar_width", "0").is_err());
        assert_eq!(
            setting_value("keys.normal.down", "j, Down").unwrap(),
            "[\"j\", \"Down\"]"
        );
        assert_eq!(setting_value("keys.normal.down", ",").unwrap(), "[\",\"]");
        let effective =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let mut settings = Settings {
            sel: 0,
            scroll: 0,
            source: "version = 1\n".into(),
            path: PathBuf::new(),
            existed: false,
            editable: true,
            edit: None,
            search: None,
            search_editing: false,
            confirm: true,
            message: None,
        };
        let (frame, _) = render_settings(
            &mut settings,
            &effective,
            &crate::app_config::Palette::default(),
            true,
            50,
            14,
        );
        assert!(frame.contains("Revert to defaults"));
        assert!(frame.contains("CLI and tmux overrides remain effective"));
        assert_eq!(
            setting_value("theme.colors.header_bg", "0007").unwrap(),
            "7"
        );
        let unset = SettingRow {
            name: "behavior.hide_windows".into(),
            persisted: "—".into(),
            initial: String::new(),
            effective: "(unset)".into(),
            source: "default".into(),
        };
        assert_eq!(initial_setting_value(&unset), "");
        let source = "[behavior]\nhide_windows = 'a\\b'\n";
        let raw_config = crate::app_config::parse(source)
            .and_then(|file| crate::app_config::resolve(&file, &Default::default()))
            .unwrap();
        let raw_row = settings_rows(source, &raw_config)
            .into_iter()
            .find(|row| row.name == "behavior.hide_windows")
            .unwrap();
        assert_eq!(initial_setting_value(&raw_row), "a\\b");
        let value = setting_value(&raw_row.name, &initial_setting_value(&raw_row)).unwrap();
        let edited = crate::app_config::edit_document(source, &raw_row.name, Some(&value)).unwrap();
        let round_trip = crate::app_config::parse(&edited)
            .and_then(|file| crate::app_config::resolve(&file, &Default::default()))
            .unwrap();
        assert_eq!(round_trip.hide_windows.as_deref(), Some("a\\b"));
        assert_eq!(setting_label("keys.normal.down"), "normal.down");
        assert_eq!(setting_label("keys.search.down"), "search.down");
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
    fn invalid_settings_document_is_read_only() {
        let effective =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let error = crate::app_config::parse("[invalid").unwrap_err();
        let mut settings = settings_state(Err(error));
        assert!(!settings.editable);

        let (frame, _) = render_settings(
            &mut settings,
            &effective,
            &crate::app_config::Palette::default(),
            true,
            50,
            14,
        );

        assert!(frame.contains("error:"));
        assert!(frame.contains("Esc back"));
        assert!(!frame.contains("Enter edit"));
        assert!(!frame.contains("Revert to defaults"));
    }

    #[test]
    fn closed_settings_expand_inline() {
        let effective =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        assert_eq!(
            select_move(&Key::Text("j".into()), &effective.normal),
            Some(1)
        );
        assert_eq!(
            select_move(&Key::Text("k".into()), &effective.normal),
            Some(-1)
        );
        assert_eq!(select_move(&Key::Text("q".into()), &effective.normal), None);
        let sel = settings_rows("version = 1\n", &effective)
            .iter()
            .position(|row| row.name == "display.mode")
            .unwrap();
        let mut settings = Settings {
            sel,
            scroll: 0,
            source: "version = 1\n".into(),
            path: PathBuf::new(),
            existed: false,
            editable: true,
            edit: Some(SettingEdit {
                name: "display.mode".into(),
                editor: Editor::Select(Select::new("split", choices("display.mode").unwrap())),
            }),
            search: None,
            search_editing: false,
            confirm: false,
            message: None,
        };

        let (frame, targets) = render_settings(
            &mut settings,
            &effective,
            &crate::app_config::Palette::default(),
            true,
            50,
            19,
        );

        assert!(frame.contains("Display"), "{frame}");
        assert!(frame.contains("mode: split"), "{frame}");
        assert!(frame.contains("❯ split"), "{frame}");
        assert!(frame.contains("  popup"), "{frame}");
        assert!(frame.contains("sidebar_width"), "{frame}");
        assert!(targets.contains(&Some(SETTINGS_MOUSE_OPTION)));
        assert!(targets.contains(&Some(SETTINGS_MOUSE_OPTION + 1)));
        assert!(matches!(
            settings_mouse_key(
                &mut settings,
                Key::Select(SETTINGS_MOUSE_OPTION + 1),
                settings_rows("version = 1\n", &effective).len() + 1,
            ),
            Key::Jump
        ));
        let Some(SettingEdit {
            editor: Editor::Select(select),
            ..
        }) = &settings.edit
        else {
            panic!("select editor closed")
        };
        assert_eq!(select.value(), "popup");
    }

    #[test]
    fn settings_mouse_selects_opens_searches_and_scrolls() {
        let effective =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let mut settings = settings_state(Ok((PathBuf::new(), false, "version = 1\n".into())));
        let total = settings_rows(&settings.source, &effective).len() + 1;

        assert!(matches!(
            settings_mouse_key(&mut settings, Key::Select(2), total),
            Key::Other
        ));
        assert_eq!(settings.sel, 2);
        assert!(matches!(
            settings_mouse_key(&mut settings, Key::Select(2), total),
            Key::Jump
        ));
        assert!(matches!(
            settings_mouse_key(&mut settings, Key::Select(SETTINGS_MOUSE_SEARCH), total),
            Key::Search
        ));
        assert_eq!(select_move(&Key::WheelUp, &effective.normal), Some(-1));
        assert_eq!(select_move(&Key::WheelDown, &effective.normal), Some(1));
    }
}
