//! Quick launcher schema, defaults, and conflict validation.

use super::*;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String")]
pub enum LauncherWorkingDirectory {
    Selected,
    Tmux,
}
string_values!(LauncherWorkingDirectory, "expected selected or tmux", "selected" => Selected, "tmux" => Tmux);

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QuickLauncherConfig {
    pub sequence: Option<String>,
    pub label: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub working_directory: Option<LauncherWorkingDirectory>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickLauncher {
    pub id: String,
    pub sequence: String,
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_directory: LauncherWorkingDirectory,
    pub enabled: bool,
}

pub(super) const QUICK_LAUNCHER_FIELDS: &[&str] = &[
    "sequence",
    "label",
    "command",
    "args",
    "working_directory",
    "enabled",
];
const MAX_QUICK_LAUNCHERS: usize = 32;

fn default_quick_launchers() -> BTreeMap<String, QuickLauncher> {
    BTreeMap::from([
        (
            "nvim".into(),
            QuickLauncher {
                id: "nvim".into(),
                sequence: "oe".into(),
                label: "nvim".into(),
                command: "nvim".into(),
                args: Vec::new(),
                working_directory: LauncherWorkingDirectory::Selected,
                enabled: true,
            },
        ),
        (
            "lazygit".into(),
            QuickLauncher {
                id: "lazygit".into(),
                sequence: "og".into(),
                label: "lazygit".into(),
                command: "lazygit".into(),
                args: Vec::new(),
                working_directory: LauncherWorkingDirectory::Selected,
                enabled: true,
            },
        ),
    ])
}

pub(super) fn valid_launcher_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}

fn valid_launcher_sequence(sequence: &str) -> bool {
    (1..=2).contains(&sequence.len()) && sequence.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn valid_launcher_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

pub(super) fn resolve_quick_launchers(
    config: Option<&BTreeMap<String, QuickLauncherConfig>>,
) -> Result<(Vec<QuickLauncher>, BTreeMap<String, String>), ConfigError> {
    let mut launchers = default_quick_launchers();
    let mut sources = BTreeMap::new();
    for launcher in launchers.values() {
        for field in QUICK_LAUNCHER_FIELDS {
            sources.insert(
                format!("quick_launchers.{}.{field}", launcher.id),
                "default".into(),
            );
        }
    }

    if let Some(config) = config {
        for (id, entry) in config {
            let field = format!("quick_launchers.{id}");
            if !valid_launcher_id(id) {
                return Err(ConfigError::invalid(
                    &field,
                    "ID must be lowercase ASCII letters, digits, hyphens, or underscores",
                ));
            }
            if config.len() > MAX_QUICK_LAUNCHERS {
                return Err(ConfigError::invalid(
                    "quick_launchers",
                    "at most 32 launcher entries are allowed",
                ));
            }
            let builtin = launchers.contains_key(id);
            for (name, value) in [
                ("sequence", entry.sequence.as_deref()),
                ("label", entry.label.as_deref()),
                ("command", entry.command.as_deref()),
            ] {
                if let Some(value) = value {
                    let valid = match name {
                        "sequence" => valid_launcher_sequence(value),
                        "label" => value.is_ascii() && valid_launcher_text(value, 48),
                        "command" => valid_launcher_text(value, 4096),
                        _ => unreachable!(),
                    };
                    if !valid {
                        let reason = match name {
                            "sequence" => "must be one or two ASCII letters or digits",
                            "label" => "must be 1..=48 printable ASCII characters",
                            "command" => {
                                "must be a non-empty executable name without control characters"
                            }
                            _ => unreachable!(),
                        };
                        return Err(ConfigError::invalid(&format!("{field}.{name}"), reason));
                    }
                } else if !builtin {
                    return Err(ConfigError::invalid(
                        &format!("{field}.{name}"),
                        "required for a custom launcher",
                    ));
                }
            }
            if let Some(args) = &entry.args {
                let bytes = args.iter().map(String::len).sum::<usize>();
                if args.len() > 64
                    || bytes > 8192
                    || args.iter().any(|arg| arg.chars().any(char::is_control))
                {
                    return Err(ConfigError::invalid(
                        &format!("{field}.args"),
                        "at most 64 arguments and 8192 bytes; control characters are not allowed",
                    ));
                }
            }

            let launcher = launchers
                .entry(id.clone())
                .or_insert_with(|| QuickLauncher {
                    id: id.clone(),
                    sequence: entry.sequence.clone().unwrap_or_default(),
                    label: entry.label.clone().unwrap_or_default(),
                    command: entry.command.clone().unwrap_or_default(),
                    args: Vec::new(),
                    working_directory: LauncherWorkingDirectory::Selected,
                    enabled: true,
                });
            for (name, supplied) in [
                ("sequence", entry.sequence.is_some()),
                ("label", entry.label.is_some()),
                ("command", entry.command.is_some()),
                ("args", entry.args.is_some()),
                ("working_directory", entry.working_directory.is_some()),
                ("enabled", entry.enabled.is_some()),
            ] {
                if supplied {
                    sources.insert(format!("quick_launchers.{id}.{name}"), "file".into());
                } else if !builtin {
                    sources.insert(format!("quick_launchers.{id}.{name}"), "default".into());
                }
            }
            if let Some(sequence) = &entry.sequence {
                launcher.sequence.clone_from(sequence);
            }
            if let Some(label) = &entry.label {
                launcher.label.clone_from(label);
            }
            if let Some(command) = &entry.command {
                launcher.command.clone_from(command);
            }
            if let Some(args) = &entry.args {
                launcher.args.clone_from(args);
            }
            if let Some(working_directory) = entry.working_directory {
                launcher.working_directory = working_directory;
            }
            if let Some(enabled) = entry.enabled {
                launcher.enabled = enabled;
            }
        }
    }
    if launchers.len() > MAX_QUICK_LAUNCHERS {
        return Err(ConfigError::invalid(
            "quick_launchers",
            "at most 32 launcher entries are allowed",
        ));
    }
    Ok((launchers.into_values().collect(), sources))
}

pub(super) fn validate_quick_launcher_conflicts(
    launchers: &[QuickLauncher],
    management_enabled: bool,
    normal: &Keymap,
) -> Result<(), ConfigError> {
    if !management_enabled {
        return Ok(());
    }
    let active: Vec<_> = launchers
        .iter()
        .filter(|launcher| launcher.enabled)
        .collect();
    for launcher in &active {
        let first = launcher.sequence.as_bytes()[0];
        if let Some(action) = action_for(normal, KeyChord::Printable(first)) {
            return Err(ConfigError::invalid(
                &format!("quick_launchers.{}.sequence", launcher.id),
                format!(
                    "sequence '{}' conflicts with keys.normal.{}",
                    launcher.sequence,
                    format!("{action:?}").to_lowercase()
                ),
            ));
        }
        if first == b'G' {
            return Err(ConfigError::invalid(
                &format!("quick_launchers.{}.sequence", launcher.id),
                format!(
                    "sequence '{}' conflicts with the built-in G action",
                    launcher.sequence
                ),
            ));
        }
        for binding in crate::input::available_sequences(true) {
            if launcher.sequence.starts_with(binding.sequence)
                || binding.sequence.starts_with(&launcher.sequence)
            {
                return Err(ConfigError::invalid(
                    &format!("quick_launchers.{}.sequence", launcher.id),
                    format!(
                        "sequence '{}' conflicts with built-in sequence '{}'",
                        launcher.sequence, binding.sequence
                    ),
                ));
            }
        }
    }
    for (index, launcher) in active.iter().enumerate() {
        for other in &active[index + 1..] {
            if launcher.sequence.starts_with(&other.sequence)
                || other.sequence.starts_with(&launcher.sequence)
            {
                return Err(ConfigError::invalid(
                    &format!("quick_launchers.{}.sequence", launcher.id),
                    format!(
                        "sequence '{}' conflicts with launcher '{}' sequence '{}'",
                        launcher.sequence, other.id, other.sequence
                    ),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_launchers_resolve_defaults_overrides_custom_entries_and_rows() {
        let defaults = resolve(&parse("").unwrap(), &BTreeMap::new()).unwrap();
        let nvim = defaults
            .quick_launchers
            .iter()
            .find(|launcher| launcher.id == "nvim")
            .unwrap();
        assert_eq!(nvim.sequence, "oe");
        assert_eq!(nvim.command, "nvim");
        assert_eq!(nvim.working_directory, LauncherWorkingDirectory::Selected);
        let lazygit = defaults
            .quick_launchers
            .iter()
            .find(|launcher| launcher.id == "lazygit")
            .unwrap();
        assert_eq!(lazygit.sequence, "og");

        let source = r#"
[tmux_management]
enabled = true

[quick_launchers.nvim]
command = "/tmp/tool box/editor"
args = ["--wait", "a'b"]

[quick_launchers.lazygit]
enabled = false

[quick_launchers.terminal]
sequence = "ot"
label = "terminal"
command = "fish"
args = ["--login"]
working_directory = "tmux"
"#;
        let config = resolve(&parse(source).unwrap(), &BTreeMap::new()).unwrap();
        let nvim = config
            .quick_launchers
            .iter()
            .find(|launcher| launcher.id == "nvim")
            .unwrap();
        assert_eq!(nvim.command, "/tmp/tool box/editor");
        assert_eq!(nvim.args, ["--wait", "a'b"]);
        assert_eq!(nvim.sequence, "oe");
        assert_eq!(nvim.working_directory, LauncherWorkingDirectory::Selected);
        assert_eq!(config.sources["quick_launchers.nvim.command"], "file");
        assert_eq!(config.sources["quick_launchers.nvim.sequence"], "default");
        let lazygit = config
            .quick_launchers
            .iter()
            .find(|launcher| launcher.id == "lazygit")
            .unwrap();
        assert!(!lazygit.enabled);
        let terminal = config
            .quick_launchers
            .iter()
            .find(|launcher| launcher.id == "terminal")
            .unwrap();
        assert_eq!(terminal.working_directory, LauncherWorkingDirectory::Tmux);
        assert_eq!(terminal.args, ["--login"]);

        let rows = rows(&config);
        for (name, value, source) in [
            (
                "quick_launchers.nvim.command",
                "/tmp/tool box/editor",
                "file",
            ),
            ("quick_launchers.nvim.sequence", "oe", "default"),
            ("quick_launchers.nvim.args", "[\"--wait\", \"a'b\"]", "file"),
            ("quick_launchers.lazygit.enabled", "false", "file"),
            ("quick_launchers.terminal.working_directory", "tmux", "file"),
        ] {
            let row = rows.iter().find(|row| row.name == name).unwrap();
            assert_eq!(row.value, value, "{name}");
            assert_eq!(row.source, source, "{name}");
        }
    }

    #[test]
    fn quick_launcher_schema_and_active_sequence_conflicts_are_rejected() {
        for source in [
            "[quick_launchers.Bad]\nsequence='x'\nlabel='x'\ncommand='x'",
            "[quick_launchers.custom]\nsequence='x'\nlabel='custom'",
            "[quick_launchers.custom]\nsequence='xx'\nlabel='custom'\ncommand='x'\nunknown=true",
            "[quick_launchers.custom]\nsequence='x y'\nlabel='custom'\ncommand='x'",
            "[quick_launchers.custom]\nsequence='x'\nlabel='custom'\ncommand='x'\nargs=[1]",
            "[quick_launchers.custom]\nsequence='x'\nlabel='custom'\ncommand='x'\nworking_directory='home'",
            "[quick_launchers.custom]\nsequence='x'\nlabel=\"bad\\nlabel\"\ncommand='x'",
        ] {
            assert!(parse(source).is_err(), "accepted {source}");
        }

        // Conflicts are checked only when the management gate makes the
        // sequence active. The same file remains valid while launchers are off.
        let colliding = "[tmux_management]\nenabled=false\n[quick_launchers.nvim]\nsequence='U'\n";
        assert!(parse(colliding).is_ok());
        for source in [
            "[quick_launchers.nvim]\nsequence='U'",
            "[tmux_management]\nenabled=true\n[quick_launchers.nvim]\nsequence='G'",
            "[tmux_management]\nenabled=true\n[quick_launchers.nvim]\nsequence='gg'",
            "[tmux_management]\nenabled=true\n[quick_launchers.nvim]\nsequence='e'\n[quick_launchers.lazygit]\nsequence='et'",
            "[tmux_management]\nenabled=true\n[keys.normal]\ndown=['o']",
        ] {
            assert!(parse(source).is_err(), "accepted active conflict: {source}");
        }
        let disabled = parse(
            "[tmux_management]\nenabled=true\n[quick_launchers.nvim]\nenabled=false\nsequence='U'",
        )
        .unwrap();
        assert!(resolve(&disabled, &BTreeMap::new()).is_ok());
    }
}
