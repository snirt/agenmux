//! Command icon rules: user `[[command_icons]]` entries plus built-in defaults.

use super::*;
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CommandIconConfig {
    #[serde(rename = "match")]
    pub pattern: String,
    pub icon: String,
}

/// One resolved rule; `regex` matches the whole command name.
#[derive(Debug, Clone)]
pub struct CommandIconRule {
    pub pattern: String,
    pub icon: String,
    pub builtin: bool,
    regex: Regex,
}
// Regex has no equality; the pattern it was compiled from stands in for it.
impl PartialEq for CommandIconRule {
    fn eq(&self, other: &Self) -> bool {
        (&self.pattern, &self.icon, self.builtin) == (&other.pattern, &other.icon, other.builtin)
    }
}

/// User rules in file order, then built-ins; the first match wins.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandIcons {
    pub rules: Vec<CommandIconRule>,
}

const MAX_RULES: usize = 64;
const MAX_PATTERN_BYTES: usize = 256;
const MAX_ICON_BYTES: usize = 16;
const REGEX_SIZE_LIMIT: usize = 1 << 20;
const BUILTINS: &[(&str, &str)] = &[
    ("nvim", "\u{f36f}"),    // nf-linux-neovim
    ("lazygit", "\u{e702}"), // nf-dev-git
];

/// Compile `^(?:pattern)$` once per distinct pattern: resolution runs on every
/// live refresh, not only on reload.
fn compile(pattern: &str) -> Result<Regex, regex::Error> {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(regex) = cache.lock().ok().and_then(|c| c.get(pattern).cloned()) {
        return Ok(regex);
    }
    let regex = RegexBuilder::new(&format!("^(?:{pattern})$"))
        .size_limit(REGEX_SIZE_LIMIT)
        .build()?;
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= 4 * MAX_RULES {
            cache.clear();
        }
        cache.insert(pattern.to_owned(), regex.clone());
    }
    Ok(regex)
}

pub(super) fn resolve_command_icons(
    config: Option<&Vec<CommandIconConfig>>,
) -> Result<CommandIcons, ConfigError> {
    let entries = config.map_or(&[][..], Vec::as_slice);
    if entries.len() > MAX_RULES {
        return Err(ConfigError::invalid(
            "command_icons",
            "at most 64 rules are allowed",
        ));
    }
    let mut rules = Vec::with_capacity(entries.len() + BUILTINS.len());
    for (i, entry) in entries.iter().enumerate() {
        let field = format!("command_icons[{i}]");
        if entry.pattern.is_empty()
            || entry.pattern.len() > MAX_PATTERN_BYTES
            || entry.pattern.chars().any(char::is_control)
        {
            return Err(ConfigError::invalid(
                &format!("{field}.match"),
                "must be 1..=256 bytes without control characters",
            ));
        }
        if entry.icon.len() > MAX_ICON_BYTES || entry.icon.chars().any(char::is_control) {
            return Err(ConfigError::invalid(
                &format!("{field}.icon"),
                "must be at most 16 bytes without control characters",
            ));
        }
        let regex = compile(&entry.pattern).map_err(|_| {
            ConfigError::invalid(
                &format!("{field}.match"),
                "invalid or too large regular expression",
            )
        })?;
        rules.push(CommandIconRule {
            pattern: entry.pattern.clone(),
            icon: entry.icon.clone(),
            builtin: false,
            regex,
        });
    }
    for (pattern, icon) in BUILTINS {
        rules.push(CommandIconRule {
            pattern: (*pattern).into(),
            icon: (*icon).into(),
            builtin: true,
            regex: compile(pattern).expect("built-in command icon pattern"),
        });
    }
    Ok(CommandIcons { rules })
}

impl Default for CommandIcons {
    fn default() -> Self {
        resolve_command_icons(None).expect("built-in command icons")
    }
}

impl CommandIcons {
    /// Icon for a non-agent pane command. `None` when nothing matches, the
    /// matching rule's icon is empty, or a built-in meets plain glyphs.
    pub fn lookup(&self, command: &str, glyphs: Glyphs) -> Option<&str> {
        let rule = self
            .rules
            .iter()
            .find(|rule| rule.regex.is_match(command))?;
        if rule.icon.is_empty() || (rule.builtin && glyphs == Glyphs::Plain) {
            return None;
        }
        Some(&rule.icon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(source: &str) -> Result<CommandIcons, ConfigError> {
        let file = parse(source)?;
        resolve_command_icons(file.command_icons.as_ref())
    }

    #[test]
    fn parses_rules_and_rejects_bad_fields() {
        let file = parse("[[command_icons]]\nmatch = \"n?vim\"\nicon = \"V\"\n").unwrap();
        assert_eq!(
            file.command_icons.unwrap(),
            [CommandIconConfig {
                pattern: "n?vim".into(),
                icon: "V".into()
            }]
        );
        assert!(
            parse("[[command_icons]]\nicon = \"V\"\n").is_err(),
            "match is required"
        );
        assert!(
            parse("[[command_icons]]\nmatch = \"x\"\n").is_err(),
            "icon is required"
        );
        assert!(
            parse("[[command_icons]]\nmatch = \"x\"\nicon = \"V\"\ncolor = \"#57a143\"\n").is_err(),
            "unknown fields are rejected"
        );
        let table = parse("[command_icons]\nmatch = \"x\"\nicon = \"V\"\n").unwrap_err();
        assert!(table.to_string().contains("command_icons"), "{table}");
        let positional = parse("command_icons = [[\"x\", \"V\"]]\n").unwrap_err();
        assert!(
            positional.to_string().contains("command_icons[0]"),
            "{positional}"
        );
    }

    #[test]
    fn patterns_match_the_whole_name() {
        let icons = rules(
            "[[command_icons]]\nmatch = \"vim\"\nicon = \"A\"\n\
             [[command_icons]]\nmatch = \"b?top|htop\"\nicon = \"T\"\n",
        )
        .unwrap();
        assert_eq!(icons.lookup("vim", Glyphs::Rich), Some("A"));
        assert_eq!(
            icons.lookup("nvim", Glyphs::Rich),
            Some("\u{f36f}"),
            "vim is not nvim"
        );
        for top in ["top", "btop", "htop"] {
            assert_eq!(icons.lookup(top, Glyphs::Rich), Some("T"), "{top}");
        }
        assert_eq!(icons.lookup("topgrade", Glyphs::Rich), None);
    }

    #[test]
    fn file_order_then_builtins() {
        let icons = rules(
            "[[command_icons]]\nmatch = \"n?vim\"\nicon = \"A\"\n\
             [[command_icons]]\nmatch = \"nvim\"\nicon = \"B\"\n\
             [[command_icons]]\nmatch = \"lazygit\"\nicon = \"\"\n",
        )
        .unwrap();
        assert_eq!(
            icons.lookup("nvim", Glyphs::Rich),
            Some("A"),
            "first match wins"
        );
        assert_eq!(
            icons.lookup("lazygit", Glyphs::Rich),
            None,
            "empty icon disables"
        );
        let defaults = CommandIcons::default();
        assert_eq!(defaults.lookup("nvim", Glyphs::Rich), Some("\u{f36f}"));
        assert_eq!(defaults.lookup("lazygit", Glyphs::Rich), Some("\u{e702}"));
        assert_eq!(defaults.lookup("zsh", Glyphs::Rich), None);
    }

    #[test]
    fn plain_glyphs_hide_only_builtins() {
        let icons = rules("[[command_icons]]\nmatch = \"btop\"\nicon = \"\u{f4bc}\"\n").unwrap();
        assert_eq!(icons.lookup("nvim", Glyphs::Plain), None);
        assert_eq!(icons.lookup("btop", Glyphs::Plain), Some("\u{f4bc}"));
        let user = rules("[[command_icons]]\nmatch = \"nvim\"\nicon = \"VI\"\n").unwrap();
        assert_eq!(user.lookup("nvim", Glyphs::Plain), Some("VI"));
    }

    #[test]
    fn validation_names_the_entry() {
        let bad = |source: &str, field: &str| {
            let error = parse(source).unwrap_err().to_string();
            assert!(error.contains(field), "{error}");
        };
        bad(
            "[[command_icons]]\nmatch = \"(\"\nicon = \"x\"\n",
            "command_icons[0].match",
        );
        bad(
            "[[command_icons]]\nmatch = \"\"\nicon = \"x\"\n",
            "command_icons[0].match",
        );
        bad(
            &format!(
                "[[command_icons]]\nmatch = \"{}\"\nicon = \"x\"\n",
                "a".repeat(257)
            ),
            "command_icons[0].match",
        );
        bad(
            "[[command_icons]]\nmatch = \"a\"\nicon = \"x\"\n[[command_icons]]\nmatch = \"b\"\nicon = \"12345678901234567\"\n",
            "command_icons[1].icon",
        );
        bad(
            "[[command_icons]]\nmatch = \"a\"\nicon = \"\\u0007\"\n",
            "command_icons[0].icon",
        );
        bad(
            "[[command_icons]]\nmatch = \"a{1000}{1000}\"\nicon = \"x\"\n",
            "command_icons[0].match",
        );
        let many = "[[command_icons]]\nmatch = \"a\"\nicon = \"x\"\n".repeat(65);
        bad(&many, "command_icons");
    }

    #[test]
    fn edited_icons_change_the_resolved_config() {
        let a = rules("[[command_icons]]\nmatch = \"a\"\nicon = \"x\"\n").unwrap();
        let b = rules("[[command_icons]]\nmatch = \"a\"\nicon = \"y\"\n").unwrap();
        assert_ne!(a, b);
        assert_eq!(
            a,
            rules("[[command_icons]]\nmatch = \"a\"\nicon = \"x\"\n").unwrap()
        );
    }
}
