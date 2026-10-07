//! Key chord grammar and per-mode keymaps.

use super::*;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Down,
    Up,
    Jump,
    Search,
    Filter,
    Reset,
    Help,
    Versions,
    Settings,
    Close,
    Accept,
    Cancel,
    Backspace,
    Clear,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyChord {
    Printable(u8),
    Control(u8),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    PageUp,
    PageDown,
    Enter,
    Escape,
    Tab,
    Backspace,
}
impl KeyChord {
    /// Conservative portable controls with the current ISIG/IXON/IEXTEN mode:
    /// C-c, C-d, C-g, C-k, C-n, C-p, C-u, C-w, C-x, C-], C-^, C-_, plus the
    /// C-i/C-m/C-[/C-? aliases of Tab/Enter/Escape/BSpace.
    /// Exclude conventional signal, flow-control, and extended editing controls
    /// (C-o/q/r/s/t/v/y/z and C-backslash), and the four bytes the sidebar's own
    /// input protocol claims before any keymap lookup.
    /// C-c is retained for the default search cancel; resolved_keys reserves
    /// C-c/C-d as emergency paths (except search cancel).
    /// Custom terminal control-character assignments are not guaranteed.
    pub fn parse(s: &str) -> Result<Self, &'static str> {
        Ok(match s {
            "Up" => Self::Up,
            "Down" => Self::Down,
            "Left" => Self::Left,
            "Right" => Self::Right,
            "Home" => Self::Home,
            "End" => Self::End,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            "Delete" => Self::Delete,
            "Enter" => Self::Enter,
            "Escape" => Self::Escape,
            "Tab" => Self::Tab,
            "BSpace" => Self::Backspace,
            "Space" => Self::Printable(b' '),
            _ if s.len() == 1 && (b' '..=b'~').contains(&s.as_bytes()[0]) => {
                Self::Printable(s.as_bytes()[0])
            }
            _ if s.len() == 3 && s.starts_with("C-") => {
                let c = s.as_bytes()[2].to_ascii_uppercase();
                let code = match c {
                    b'@'..=b'_' => c & 31,
                    b'?' => 127,
                    _ => return Err("unsupported control chord"),
                };
                match code {
                    9 => Self::Tab,
                    13 => Self::Enter,
                    27 => Self::Escape,
                    127 => Self::Backspace,
                    // The terminal folds these into Enter/BSpace but tmux keeps
                    // them as distinct keys, so one spelling cannot mean the
                    // same key in the popup and in the split key tables.
                    8 | 10 => {
                        return Err("control chord is not distinguishable from Enter or BSpace")
                    }
                    // The sidebar's key transport spends these on its own
                    // packets (text prefix, wheel up/down, click target, key
                    // sequence, clear filter), so they never reach a keymap
                    // lookup in popup mode. Rejecting them keeps one config
                    // from meaning two things across modes.
                    0..=2 | 5..=6 | 12 => {
                        return Err("control chord is reserved by the input protocol")
                    }
                    3..=4 | 7 | 11 | 14 | 16 | 21 | 23..=24 | 29..=31 => Self::Control(code),
                    _ => return Err("control chord may be intercepted by the terminal"),
                }
            }
            _ => {
                return Err(
                    "expected printable ASCII, named key, or a single representable C- chord",
                )
            }
        })
    }
}
impl KeyChord {
    /// tmux key-string name. Every representable chord has one (verified
    /// against tmux 3.x bind-key); `;` needs the command-separator escape.
    pub fn tmux_name(self) -> String {
        match self {
            Self::Printable(b' ') => "Space".into(),
            Self::Printable(b';') => "\\;".into(),
            Self::Printable(b) => char::from(b).to_string(),
            Self::Control(c) => format!("C-{}", char::from(c + b'@').to_ascii_lowercase()),
            Self::Up => "Up".into(),
            Self::Down => "Down".into(),
            Self::Left => "Left".into(),
            Self::Right => "Right".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::Delete => "DC".into(),
            Self::PageUp => "PageUp".into(),
            Self::PageDown => "PageDown".into(),
            Self::Enter => "Enter".into(),
            Self::Escape => "Escape".into(),
            Self::Tab => "Tab".into(),
            Self::Backspace => "BSpace".into(),
        }
    }
    /// Help-overlay spelling ("Enter", "Esc", "C-u"); `short` is the footer
    /// spelling ("↵", "esc", "^u") that fits a narrow sidebar.
    pub fn label(self, short: bool) -> String {
        match self {
            Self::Printable(b' ') => "Space".into(),
            Self::Printable(b) => char::from(b).to_string(),
            Self::Control(c) if short => format!("^{}", char::from(c + b'@').to_ascii_lowercase()),
            Self::Control(c) => format!("C-{}", char::from(c + b'@').to_ascii_lowercase()),
            Self::Up => "↑".into(),
            Self::Down => "↓".into(),
            Self::Left => "←".into(),
            Self::Right => "→".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::Delete => "Del".into(),
            Self::PageUp => "PgUp".into(),
            Self::PageDown => "PgDn".into(),
            Self::Enter if short => "↵".into(),
            Self::Enter => "Enter".into(),
            Self::Escape if short => "esc".into(),
            Self::Escape => "Esc".into(),
            Self::Tab => "Tab".into(),
            Self::Backspace => "BSpace".into(),
        }
    }
}
/// Resolved per-mode bindings: every action of the mode, possibly with no chords.
pub type Keymap = BTreeMap<Action, Vec<KeyChord>>;
/// Reverse lookup; validation guarantees a chord maps to at most one action.
pub fn action_for(keys: &Keymap, chord: KeyChord) -> Option<Action> {
    keys.iter()
        .find(|(_, chords)| chords.contains(&chord))
        .map(|(action, _)| *action)
}
impl<'de> Deserialize<'de> for KeyChord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeyConfig {
    pub sequence_timeout_ms: Option<u64>,
    pub normal: Option<BTreeMap<Action, Vec<KeyChord>>>,
    pub search: Option<BTreeMap<Action, Vec<KeyChord>>>,
}
#[derive(Debug, Clone, Copy)]
pub enum KeyMode {
    Normal,
    Search,
}
impl KeyMode {
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Search => "search",
        }
    }
}
/// Replace only explicitly supplied action lists, then validate the complete mode.
pub fn resolved_keys(
    mode: KeyMode,
    overrides: Option<&BTreeMap<Action, Vec<KeyChord>>>,
) -> Result<BTreeMap<Action, Vec<KeyChord>>, ConfigError> {
    use Action::*;
    let defaults: &[(Action, &[&str])] = match mode {
        KeyMode::Normal => &[
            (Down, &["j", "Down"]),
            (Up, &["k", "Up"]),
            (Jump, &["Enter", "l"]),
            (Search, &["/"]),
            (Filter, &["f"]),
            (Reset, &["Escape"]),
            (Help, &["?"]),
            (Versions, &["U"]),
            (Settings, &["s"]),
            (Close, &["q", "Q"]),
        ],
        KeyMode::Search => &[
            (Up, &["Up", "C-p"]),
            (Down, &["Down", "C-n"]),
            (Accept, &["Enter"]),
            (Cancel, &["Escape", "C-c"]),
            (Backspace, &["BSpace"]),
            (Clear, &["C-u"]),
        ],
    };
    let mut keys: BTreeMap<_, _> = defaults
        .iter()
        .map(|(a, k)| {
            (
                *a,
                k.iter()
                    .map(|s| KeyChord::parse(s).unwrap())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    if let Some(overrides) = overrides {
        for (action, chords) in overrides {
            let field = format!("keys.{}.{action:?}", mode.name()).to_lowercase();
            if !keys.contains_key(action) {
                return Err(ConfigError::invalid(
                    &field,
                    "unsupported action for this mode",
                ));
            }
            if chords.len() > 16 {
                return Err(ConfigError::invalid(&field, "at most 16 chords per action"));
            }
            keys.insert(*action, chords.clone());
        }
    }
    let mut seen = HashSet::new();
    for (action, chords) in &keys {
        let field = format!("keys.{}.{action:?}", mode.name()).to_lowercase();
        for chord in chords {
            if matches!(mode, KeyMode::Search) && matches!(chord, KeyChord::Printable(_)) {
                return Err(ConfigError::invalid(
                    &field,
                    "printable search keys are reserved for query text",
                ));
            }
            if matches!(chord, KeyChord::Control(4))
                || matches!(chord, KeyChord::Control(3)) && *action != Cancel
            {
                return Err(ConfigError::invalid(
                    &field,
                    "Ctrl-C/Ctrl-D are reserved emergency exit keys (Ctrl-C may cancel search)",
                ));
            }
            if !seen.insert(*chord) {
                return Err(ConfigError::invalid(
                    &field,
                    "duplicate or aliased key chord after merging defaults",
                ));
            }
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_grammar_aliases_and_emergency_reservations() {
        for key in [
            "j", ";", "Space", "Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown",
            "Enter", "Escape", "Tab", "BSpace", "C-k", "C-p", "C-u", "C-g", "C-_",
        ] {
            assert!(KeyChord::parse(key).is_ok(), "{key}");
        }
        for key in [
            "", "é", "F1", "M-a", "C-M-a", "C-1", "C-S-a", "\u{1b}[A", "\n", "jj",
            // tmux binds these as their own keys, so the terminal's fold into
            // Enter/BSpace would install a binding for a different key.
            "C-j", "C-J", "C-h", "C-H",
        ] {
            assert!(KeyChord::parse(key).is_err(), "{key}");
        }
        for (a, b) in [
            ("C-i", "Tab"),
            ("C-m", "Enter"),
            ("C-[", "Escape"),
            ("C-?", "BSpace"),
            ("C-K", "C-k"),
            (" ", "Space"),
        ] {
            assert_eq!(KeyChord::parse(a), KeyChord::parse(b));
        }
        for source in [
            "[keys.normal]\nhelp = ['C-c']",
            "[keys.normal]\nclose = ['C-d']",
            "[keys.search]\ncancel = ['C-d']",
            "[keys.search]\nup = ['x']",
            "[keys.search]\nclear = ['Space']",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn intercepted_controls_are_rejected_in_every_mode() {
        for key in [
            "C-o", "C-q", "C-r", "C-s", "C-t", "C-v", "C-y", "C-z", "C-\\",
        ] {
            for key in [
                key.to_owned(),
                format!("C-{}", key[2..].to_ascii_uppercase()),
            ] {
                assert!(KeyChord::parse(&key).is_err(), "{key}");
                for (mode, action) in [("normal", "help"), ("search", "clear")] {
                    assert!(parse(&format!("[keys.{mode}]\n{action} = ['{key}']")).is_err());
                }
            }
        }
        assert!(parse("").is_ok()); // Includes the default C-c cancel and C-u clear.
        assert!(parse("[keys.search]\ncancel = ['C-c']").is_ok());
    }

    #[test]
    fn merged_keys_replace_unbind_and_detect_collisions() {
        for source in [
            "[keys.normal]\ndown = ['k']",
            "[keys.normal]\njump = ['C-m', 'Enter']",
            "[keys.normal]\nhelp = ['C-j']",
            "[keys.search]\nclear = ['C-j']",
            "[keys.search]\nclear = ['C-h']",
            "[keys.search]\nbackspace = ['C-h', 'BSpace']",
            "[keys.normal]\ndown = ['Tab', 'C-i']",
            "[keys.prefix]\ntoggle = ['e']",
            "[keys.search]\nup = ['C-n']",
            "[keys.prefix]\ntoggle = ['A', 'A']",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        let c = parse("[keys.normal]\ndown = ['k']\nup = []").unwrap();
        let resolved = resolved_keys(KeyMode::Normal, c.keys.unwrap().normal.as_ref()).unwrap();
        assert_eq!(resolved[&Action::Down], vec![KeyChord::Printable(b'k')]);
        assert!(resolved[&Action::Up].is_empty());
        assert_eq!(resolved[&Action::Close].len(), 2);
        let list = (b'0'..=b'?')
            .map(|b| format!("'{}'", b as char))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse(&format!("[keys.normal]\nhelp = [{list}]\nsearch = []")).is_ok());
        assert!(parse(&format!("[keys.normal]\nhelp = [{list}, 'Z']\nsearch = []")).is_err());
    }

    #[test]
    fn chords_have_tmux_names_and_labels() {
        for (source, name, label, short) in [
            ("j", "j", "j", "j"),
            ("Space", "Space", "Space", "Space"),
            (";", "\\;", ";", ";"),
            ("C-u", "C-u", "C-u", "^u"),
            ("C-]", "C-]", "C-]", "^]"),
            ("C-m", "Enter", "Enter", "↵"),
            ("C-i", "Tab", "Tab", "Tab"),
            ("C-?", "BSpace", "BSpace", "BSpace"),
            ("Escape", "Escape", "Esc", "esc"),
            ("PageUp", "PageUp", "PgUp", "PgUp"),
            ("Up", "Up", "↑", "↑"),
        ] {
            let chord = KeyChord::parse(source).unwrap();
            assert_eq!(chord.tmux_name(), name, "{source}");
            assert_eq!(chord.label(false), label, "{source}");
            assert_eq!(chord.label(true), short, "{source}");
        }
        // Bytes the sidebar transport claims for its own packets: accepting
        // them would bind a key that only works in split mode.
        for reserved in ["C-@", "C-a", "C-b", "C-e", "C-f", "C-l"] {
            assert_eq!(
                KeyChord::parse(reserved),
                Err("control chord is reserved by the input protocol"),
                "{reserved}"
            );
        }
        let keys = resolved_keys(KeyMode::Normal, None).unwrap();
        assert_eq!(
            action_for(&keys, KeyChord::Printable(b'j')),
            Some(Action::Down)
        );
        assert_eq!(action_for(&keys, KeyChord::Down), Some(Action::Down));
        assert_eq!(action_for(&keys, KeyChord::Printable(b'n')), None);
    }
}
