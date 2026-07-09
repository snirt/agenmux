// Agent detection rules, loaded from agents/*.conf shell-syntax files.
// The binary hardcodes zero agent knowledge — everything comes from here.
use regex::Regex;
use std::path::Path;

#[derive(Clone, Copy, PartialEq)]
pub enum Check {
    Bt,
    Bs,
    Wt,
    Ws,
    Is,
}

pub struct AgentConf {
    pub name: String,
    pub bins: Vec<String>,
    pub path_hints: Vec<String>,
    pub blocked_title: Option<Regex>,
    pub blocked_screen: Option<Regex>,
    pub working_title: Option<Regex>,
    pub working_screen: Option<Regex>,
    pub idle_screen: Option<Regex>,
    pub check_order: Vec<Check>,
    pub title_strip: Option<Regex>,
    pub subject_screen: Option<Regex>,
    pub subject_cmd: Option<String>,
}

/// Extract `KEY=value` assignments from shell-syntax conf text without
/// executing it. Handles '...' (verbatim, may span lines), "..." with
/// \" \\ \` \$ escapes, and # comments outside quotes.
fn parse_assignments(src: &str) -> Vec<(String, String)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // line start: try IDENT=
        let key_start = i;
        let mut j = i;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
        if j > key_start && j < b.len() && b[j] == b'=' {
            let key = src[key_start..j].to_string();
            let (val, next) = parse_value(src, j + 1);
            out.push((key, val));
            i = next;
        } else {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            i += 1;
        }
    }
    out
}

fn parse_value(src: &str, start: usize) -> (String, usize) {
    let b = src.as_bytes();
    let mut val: Vec<u8> = Vec::new();
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'\'' => {
                i += 1;
                let s = i;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                val.extend_from_slice(&b[s..i]);
                i += 1; // closing quote
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\'
                        && i + 1 < b.len()
                        && matches!(b[i + 1], b'"' | b'\\' | b'`' | b'$')
                    {
                        val.push(b[i + 1]);
                        i += 2;
                    } else {
                        val.push(b[i]);
                        i += 1;
                    }
                }
                i += 1;
            }
            b'\n' | b' ' | b'\t' | b'#' => break,
            c => {
                val.push(c);
                i += 1;
            }
        }
    }
    // skip trailing comment / whitespace to end of line
    while i < b.len() && b[i] != b'\n' {
        i += 1;
    }
    if i < b.len() {
        i += 1;
    }
    (String::from_utf8_lossy(&val).into_owned(), i)
}

/// grep -Ei semantics: case-insensitive, ^/$ anchor per line.
fn re_grep(name: &str, key: &str, val: &str) -> Option<Regex> {
    compile(name, key, &format!("(?im){val}"))
}

/// sed semantics: case-sensitive, applied to single lines by the caller.
fn re_sed(name: &str, key: &str, val: &str) -> Option<Regex> {
    compile(name, key, val)
}

fn compile(name: &str, key: &str, pat: &str) -> Option<Regex> {
    match Regex::new(pat) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("agents-mon: {name}.conf {key}: bad regex: {e}");
            None
        }
    }
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

pub fn load_conf(path: &Path) -> std::io::Result<AgentConf> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let src = std::fs::read_to_string(path)?;
    let mut c = AgentConf {
        name: name.clone(),
        bins: vec![],
        path_hints: vec![],
        blocked_title: None,
        blocked_screen: None,
        working_title: None,
        working_screen: None,
        idle_screen: None,
        check_order: vec![Check::Bt, Check::Wt, Check::Bs, Check::Ws],
        title_strip: None,
        subject_screen: None,
        subject_cmd: None,
    };
    for (key, val) in parse_assignments(&src) {
        // empty value = unset (bash `gre` never matches an empty pattern)
        if val.is_empty() {
            continue;
        }
        match key.as_str() {
            "AGENT_BINS" => c.bins = words(&val),
            "AGENT_PATH_HINTS" => c.path_hints = words(&val),
            "BLOCKED_TITLE" => c.blocked_title = re_grep(&name, &key, &val),
            "BLOCKED_SCREEN" => c.blocked_screen = re_grep(&name, &key, &val),
            "WORKING_TITLE" => c.working_title = re_grep(&name, &key, &val),
            "WORKING_SCREEN" => c.working_screen = re_grep(&name, &key, &val),
            "IDLE_SCREEN" => c.idle_screen = re_grep(&name, &key, &val),
            "CHECK_ORDER" => {
                c.check_order = val
                    .split_whitespace()
                    .filter_map(|t| match t {
                        "bt" => Some(Check::Bt),
                        "bs" => Some(Check::Bs),
                        "wt" => Some(Check::Wt),
                        "ws" => Some(Check::Ws),
                        "is" => Some(Check::Is),
                        _ => None,
                    })
                    .collect()
            }
            "TITLE_STRIP" => c.title_strip = re_sed(&name, &key, &val),
            "SUBJECT_SCREEN" => c.subject_screen = re_sed(&name, &key, &val),
            "SUBJECT_CMD" => c.subject_cmd = Some(val),
            _ => {}
        }
    }
    Ok(c)
}

/// Builtin agents/, then user confs overriding by filename (position kept).
pub fn load_all(plugin_dir: &Path) -> Vec<AgentConf> {
    let mut confs: Vec<AgentConf> = Vec::new();
    let user_dir = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(&std::env::var("HOME").unwrap_or_default()).join(".config")
        })
        .join("tmux-agents-mon/agents");
    for dir in [&plugin_dir.join("agents"), &user_dir] {
        let mut files: Vec<_> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "conf"))
                .collect(),
            Err(_) => continue,
        };
        files.sort();
        for f in files {
            if let Ok(c) = load_conf(&f) {
                match confs.iter().position(|x| x.name == c.name) {
                    Some(i) => confs[i] = c,
                    None => confs.push(c),
                }
            }
        }
    }
    confs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_quoted_multiline_verbatim() {
        let src = "SUBJECT_CMD='line one\n  line \"two\" $x\n'\n";
        let a = parse_assignments(src);
        assert_eq!(a[0].0, "SUBJECT_CMD");
        assert_eq!(a[0].1, "line one\n  line \"two\" $x\n");
    }

    #[test]
    fn double_quoted_escapes_and_comments() {
        let src = "AGENT_BINS=\"a \\\"b\\\" c\" # comment\n# full comment\nX='v'#tail\n";
        let a = parse_assignments(src);
        assert_eq!(a[0].1, "a \"b\" c");
        assert_eq!(a[1], ("X".into(), "v".into()));
    }

    #[test]
    fn hash_inside_quotes_kept() {
        let a = parse_assignments("K='a#b'\n");
        assert_eq!(a[0].1, "a#b");
    }
}
