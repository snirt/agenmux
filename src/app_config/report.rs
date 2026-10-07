//! `agenmux config` help, validation, and effective-settings output.

use super::*;

/// Lay names out in aligned columns so a long list stays readable in a pane
/// narrower than the list itself.
fn columns(names: &[&str], per_row: usize, indent: &str) -> String {
    let width = names.iter().map(|name| name.len()).max().unwrap_or(0);
    names
        .chunks(per_row)
        .map(|row| {
            let cells: Vec<String> = row.iter().map(|name| format!("{name:width$}")).collect();
            format!("{indent}{}", cells.join("  ").trim_end())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every configurable field, printed from one place so `--help` cannot drift
/// from the file the loader accepts.
pub fn help() -> i32 {
    // One `action  default chords` row per binding, so the defaults are visible
    // without opening the example file.
    let bindings = |mode| {
        resolved_keys(mode, None)
            .unwrap_or_default()
            .into_iter()
            .map(|(action, chords)| {
                let action = format!("{action:?}").to_lowercase();
                let chords: Vec<String> = chords.iter().map(|chord| chord.tmux_name()).collect();
                format!("  {action:<20} ({})", chords.join(", "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    println!(
        r##"Usage: agenmux config [check [--effective [--all]] | reload]

  agenmux config                     Show this reference
  agenmux config check               Validate the config file
  agenmux config check --effective   List changed settings and their source
                                     (--all lists every setting)
  agenmux config reload              Apply an edited file to running sidebars

File:  $XDG_CONFIG_HOME/agenmux/config.toml
       or $HOME/.config/agenmux/config.toml

Every key is optional; a missing key keeps its default (shown in parentheses).
A @agenmux-* tmux option, when set, overrides the file.

[display]
  mode                 split | popup                      (split)
  show_all_panes       true | false                       (true)
  show_frame           true | false                       (true)
  sidebar_width        1..=10000 cells                    (30)
  popup_width          1..=10000 cells                    (40)
  popup_height         "auto" or 1..=10000 cells          (auto)
  agent_label          icon-text | icon | text            (icon-text)

[behavior]
  notifications        true | false                       (true)
  hide_windows         glob filtering the prefix+w picker (unset)

[tmux_management]
  enabled              true | false                       (true)
  confirm_delete       true | false                       (true)
  resume_agents        true | false                       (false)
  undo_history         0..=100 closed branches kept       (20)

[quick_launchers.<id>]
  sequence             1-2 ASCII letters or digits        (required)
  label                help text                          (required)
  command              executable name or path            (required)
  args                 array of strings                   ([])
  working_directory    selected | tmux                    (selected)
  enabled              true | false                       (true)

  - Built in: nvim (oe) and lazygit (og). Reusing their ID changes only the
    fields you set; a new ID needs sequence, label and command.
  - enabled = false removes a launcher.
  - Launchers need [tmux_management] enabled; clashing sequences are rejected.

[theme]
  base                 dark | light | terminal            (dark)

[theme.colors]
  Each role takes "default", 0..=255, or "#RRGGBB"; unset roles follow base.
{}

[keys]
  sequence_timeout_ms  positive integer milliseconds      (1000)

[keys.normal]
{}

[keys.search]
{}

  - Each value is a list that replaces the default; [] unbinds the action.
  - Chords: a printable ASCII character, Space, Up, Down, Left, Right, Home,
    End, PageUp, PageDown, Enter, Escape, Tab, BSpace, or C-<key>.
  - Reserved: C-c and C-d always exit; C-@, C-a, C-b and C-l are used
    internally; C-h and C-j are the same as BSpace and Enter.
  - Search mode cannot bind printable characters; they type the query.
  - gg and G jump to the first and last agent; binding g or G replaces them."##,
        columns(&Palette::default().roles().map(|(name, _)| name), 3, "  "),
        bindings(KeyMode::Normal),
        bindings(KeyMode::Search),
    );
    0
}

/// One resolved setting as `check --effective` reports it.
pub struct Row {
    pub name: String,
    pub value: String,
    pub source: String,
}

/// Every setting with its resolved value and the layer that decided it, in the
/// order `--help` documents rather than alphabetically, so the report reads
/// like the file it describes.
pub fn rows(config: &AppConfig) -> Vec<Row> {
    let chords = |list: &Vec<KeyChord>| {
        if list.is_empty() {
            "(unbound)".to_string()
        } else {
            list.iter()
                .map(|chord| chord.tmux_name())
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let mut out: Vec<(String, String)> = vec![
        (
            "display.mode".into(),
            match config.mode {
                DisplayMode::Split => "split".into(),
                DisplayMode::Popup => "popup".to_string(),
            },
        ),
        (
            "display.show_all_panes".into(),
            config.show_all_panes.to_string(),
        ),
        ("display.show_frame".into(), config.show_frame.to_string()),
        (
            "display.sidebar_width".into(),
            config.sidebar_width.to_string(),
        ),
        ("display.popup_width".into(), config.popup_width.to_string()),
        (
            "display.popup_height".into(),
            match config.popup_height {
                PopupHeight::Auto(_) => "auto".into(),
                PopupHeight::Cells(n) => n.to_string(),
            },
        ),
        (
            "display.agent_label".into(),
            match config.agent_label {
                AgentLabel::IconText => "icon-text",
                AgentLabel::Icon => "icon",
                AgentLabel::Text => "text",
            }
            .into(),
        ),
        (
            "behavior.notifications".into(),
            config.notifications.to_string(),
        ),
        (
            "behavior.hide_windows".into(),
            // Validated, but still user text: escape before it reaches a terminal.
            config
                .hide_windows
                .as_deref()
                .map_or("(unset)".to_string(), escaped),
        ),
        (
            "tmux_management.enabled".into(),
            config.tmux_management_enabled.to_string(),
        ),
        (
            "tmux_management.confirm_delete".into(),
            config.tmux_management_confirm_delete.to_string(),
        ),
        (
            "tmux_management.resume_agents".into(),
            config.tmux_management_resume_agents.to_string(),
        ),
        (
            "tmux_management.undo_history".into(),
            config.tmux_management_undo_history.to_string(),
        ),
        (
            "theme.base".into(),
            match config.theme.base.unwrap_or(ThemeBase::Dark) {
                ThemeBase::Dark => "dark".into(),
                ThemeBase::Light => "light".into(),
                ThemeBase::Terminal => "terminal".to_string(),
            },
        ),
    ];
    for (role, ink) in Palette::resolve(&config.theme).roles() {
        out.push((format!("theme.colors.{role}"), ink.describe()));
    }
    out.push((
        "keys.sequence_timeout_ms".into(),
        config.sequence_timeout_ms.to_string(),
    ));
    for (action, list) in &config.normal {
        out.push((
            format!("keys.normal.{action:?}").to_lowercase(),
            chords(list),
        ));
    }
    for (action, list) in &config.search {
        out.push((
            format!("keys.search.{action:?}").to_lowercase(),
            chords(list),
        ));
    }
    for launcher in &config.quick_launchers {
        let prefix = format!("quick_launchers.{}", launcher.id);
        let mut args = toml_edit::Array::new();
        for arg in &launcher.args {
            args.push(arg.as_str());
        }
        out.extend([
            (format!("{prefix}.sequence"), launcher.sequence.clone()),
            (format!("{prefix}.label"), launcher.label.clone()),
            (format!("{prefix}.command"), launcher.command.clone()),
            (format!("{prefix}.args"), args.to_string()),
            (
                format!("{prefix}.working_directory"),
                match launcher.working_directory {
                    LauncherWorkingDirectory::Selected => "selected",
                    LauncherWorkingDirectory::Tmux => "tmux",
                }
                .into(),
            ),
            (format!("{prefix}.enabled"), launcher.enabled.to_string()),
        ]);
    }
    out.into_iter()
        .map(|(name, value)| {
            let source = config
                .sources
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| "default".into());
            Row {
                name,
                value,
                source,
            }
        })
        .collect()
}

/// Settings the user actually decided: everything a bare default did not.
pub(super) fn customized(rows: &[Row]) -> usize {
    rows.iter()
        .filter(|row| row.source != "default" && row.source != "theme base")
        .count()
}

fn print_table(rows: &[Row]) {
    let name = rows.iter().map(|r| r.name.len()).max().unwrap_or(0).max(7);
    let value = rows.iter().map(|r| r.value.len()).max().unwrap_or(0).max(5);
    println!("{:name$}  {:value$}  source", "setting", "value");
    for row in rows {
        println!("{:name$}  {:value$}  {}", row.name, row.value, row.source);
    }
}

pub fn effective_check(all: bool) -> i32 {
    match current(None) {
        Ok(config) => {
            if let Some(path) = config_path() {
                println!("{}\n", escaped(&path.to_string_lossy()));
            }
            let all_rows = rows(&config);
            let shown: Vec<&Row> = if all {
                all_rows.iter().collect()
            } else {
                all_rows
                    .iter()
                    .filter(|row| row.source != "default" && row.source != "theme base")
                    .collect()
            };
            if shown.is_empty() {
                println!("every setting is at its default; --all lists them.");
                return 0;
            }
            let owned: Vec<Row> = shown
                .into_iter()
                .map(|row| Row {
                    name: row.name.clone(),
                    value: row.value.clone(),
                    source: row.source.clone(),
                })
                .collect();
            print_table(&owned);
            let rest = all_rows.len() - owned.len();
            if !all && rest > 0 {
                println!("\n{rest} settings are at their defaults; --all lists every one.");
            }
            0
        }
        Err(e) => {
            eprintln!("agenmux: {e}");
            e.exit_code()
        }
    }
}

/// Standalone validation: no tmux, detector, hook, or update dependencies.
pub fn check() -> i32 {
    let Some(path) = config_path() else {
        println!("no configuration file; using defaults");
        println!("no absolute XDG or HOME root to look in");
        return 0;
    };
    let shown = escaped(&path.to_string_lossy());
    let file = match load_path(&path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    if !path.exists() {
        println!("no configuration file; using defaults");
        println!("looked for {shown}");
        return 0;
    }
    // No tmux here: count what the file alone decides, not what options win.
    let set = resolve(&file, &Default::default())
        .map(|config| customized(&rows(&config)))
        .unwrap_or(0);
    println!("{shown}: valid");
    match set {
        0 => println!("0 settings differ from the defaults"),
        n => println!(
            "{n} settings differ from the defaults; agenmux config check --effective shows them"
        ),
    }
    0
}
