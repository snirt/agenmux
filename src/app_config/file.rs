//! Config file discovery, loading, and layout-preserving edits.

use super::*;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;

/// Pure discovery: relative/empty roots never cause project-local searching.
pub fn discover(xdg: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    xdg.filter(|p| p.is_absolute())
        .map(|p| p.join("agenmux/config.toml"))
        .or_else(|| {
            home.filter(|p| p.is_absolute())
                .map(|p| p.join(".config/agenmux/config.toml"))
        })
}
pub fn config_path() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
    discover(
        xdg.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )
}
pub fn load() -> Result<FileConfig, ConfigError> {
    match config_path() {
        Some(path) => load_path(&path),
        None => Ok(FileConfig::default()),
    }
}

pub fn document() -> Result<(PathBuf, bool, String), ConfigError> {
    let path = config_path().ok_or_else(|| {
        ConfigError::invalid(
            "file",
            "no absolute XDG_CONFIG_HOME or HOME configuration root",
        )
    })?;
    match read_source_path(&path)? {
        Some(source) => {
            parse(&source)?;
            Ok((path, true, source))
        }
        None => Ok((path, false, "version = 1\n".into())),
    }
}

fn document_error(reason: impl Into<String>) -> ConfigError {
    ConfigError::invalid("document", reason)
}

fn parse_item(value: &str) -> Result<toml_edit::Item, ConfigError> {
    let document = format!("value = {value}\n")
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| document_error("invalid TOML value"))?;
    document
        .get("value")
        .cloned()
        .ok_or_else(|| document_error("missing TOML value"))
}

fn set_document_path(
    table: &mut dyn toml_edit::TableLike,
    path: &[&str],
    value: toml_edit::Item,
) -> Result<(), ConfigError> {
    if path.len() == 1 {
        if let Some(current) = table.get_mut(path[0]) {
            let decor = current.as_value().map(|value| value.decor().clone());
            *current = value;
            if let (Some(decor), Some(value)) = (decor, current.as_value_mut()) {
                *value.decor_mut() = decor;
            }
        } else {
            table.insert(path[0], value);
        }
        return Ok(());
    }
    if !table.contains_key(path[0]) {
        table.insert(path[0], toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let child = table
        .get_mut(path[0])
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or_else(|| document_error("setting parent must be a table"))?;
    set_document_path(child, &path[1..], value)
}

fn remove_document_path(table: &mut dyn toml_edit::TableLike, path: &[&str]) {
    if path.len() == 1 {
        table.remove(path[0]);
        return;
    }
    let empty = table
        .get_mut(path[0])
        .and_then(toml_edit::Item::as_table_like_mut)
        .map(|child| {
            remove_document_path(child, &path[1..]);
            child.is_empty()
        })
        .unwrap_or(false);
    if empty {
        table.remove(path[0]);
    }
}

/// Update one known application setting while retaining the user's TOML layout.
pub fn edit_document(source: &str, name: &str, value: Option<&str>) -> Result<String, ConfigError> {
    let file = parse(source)?;
    let known_default = rows(&resolve(&FileConfig::default(), &BTreeMap::new())?)
        .into_iter()
        .any(|row| row.name == name);
    let launcher_field = name
        .strip_prefix("quick_launchers.")
        .and_then(|name| name.split_once('.'))
        .filter(|(id, field)| {
            valid_launcher_id(id)
                && QUICK_LAUNCHER_FIELDS.contains(field)
                && (matches!(*id, "nvim" | "lazygit")
                    || file
                        .quick_launchers
                        .as_ref()
                        .is_some_and(|launchers| launchers.contains_key(*id)))
        })
        .is_some();
    let known = known_default || launcher_field;
    if !known {
        return Err(ConfigError::invalid(name, "unknown application setting"));
    }
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| document_error("invalid TOML document"))?;
    let path: Vec<_> = name.split('.').collect();
    match value {
        Some(value) => set_document_path(document.as_table_mut(), &path, parse_item(value)?)?,
        None => remove_document_path(document.as_table_mut(), &path),
    }
    let output = document.to_string();
    parse(&output)?;
    Ok(output)
}

/// Remove file-layer customizations without deleting comments or schema version.
pub fn revert_document(source: &str) -> Result<String, ConfigError> {
    parse(source)?;
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| document_error("invalid TOML document"))?;
    for section in ["display", "behavior", "theme", "keys", "quick_launchers"] {
        document.as_table_mut().remove(section);
    }
    let output = document.to_string();
    parse(&output)?;
    Ok(output)
}

/// Validate and atomically replace the configuration, following regular-file symlinks.
pub fn save_document(path: &Path, source: &str) -> Result<(), ConfigError> {
    if source.len() > MAX_BYTES {
        return Err(ConfigError::invalid("file", "configuration exceeds 64 KiB"));
    }
    parse(source)?;
    let link = fs::symlink_metadata(path);
    let target = match link {
        Ok(metadata) if metadata.file_type().is_symlink() => match fs::canonicalize(path) {
            Ok(target) => target,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let target = fs::read_link(path).map_err(|e| ConfigError::io(path, e))?;
                if target.is_absolute() {
                    target
                } else {
                    path.parent()
                        .ok_or_else(|| {
                            ConfigError::invalid("file", "configuration path has no parent")
                        })?
                        .join(target)
                }
            }
            Err(error) => return Err(ConfigError::io(path, error)),
        },
        Ok(metadata) if metadata.is_file() => path.to_path_buf(),
        Ok(_) => {
            return Err(ConfigError::invalid(
                "file",
                "configuration target is not a regular file",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(ConfigError::io(path, error)),
    };
    let permissions = match fs::metadata(&target) {
        Ok(metadata) if metadata.is_file() => Some(metadata.permissions()),
        Ok(_) => {
            return Err(ConfigError::invalid(
                "file",
                "configuration target is not a regular file",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(ConfigError::io(&target, error)),
    };
    let parent = target
        .parent()
        .ok_or_else(|| ConfigError::invalid("file", "configuration path has no parent"))?;
    fs::create_dir_all(parent).map_err(|e| ConfigError::io(parent, e))?;
    let mut temporary = None;
    for attempt in 0..100 {
        let candidate = parent.join(format!(
            ".agenmux-config-{}-{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ConfigError::io(&candidate, error)),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        ConfigError::invalid("file", "cannot allocate temporary configuration file")
    })?;
    let result = (|| {
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        drop(file);
        fs::rename(&temporary_path, &target)?;
        fs::File::open(parent)?.sync_all()
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary_path);
        return Err(ConfigError::io(&target, error));
    }
    Ok(())
}
/// Read-only, nonblocking open; inspect the opened descriptor, not a pre-open stat.
/// Symlinks to regular files are intentionally supported.
fn read_source_path(path: &Path) -> Result<Option<String>, ConfigError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return match fs::symlink_metadata(path) {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                    for parent in path.ancestors().skip(1) {
                        match fs::symlink_metadata(parent) {
                            Ok(metadata) => {
                                if metadata.file_type().is_symlink() {
                                    fs::metadata(parent)
                                        .map_err(|error| ConfigError::io(path, error))?;
                                }
                                break;
                            }
                            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                            Err(error) => return Err(ConfigError::io(path, error)),
                        }
                    }
                    Ok(None)
                }
                Err(other) => Err(ConfigError::io(path, other)),
                Ok(_) => Err(ConfigError::io(path, error)),
            };
        }
        Err(error) => return Err(ConfigError::io(path, error)),
    };
    if !file
        .metadata()
        .map_err(|error| ConfigError::io(path, error))?
        .is_file()
    {
        return Err(ConfigError::invalid("file", "must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| ConfigError::io(path, error))?;
    if bytes.len() > MAX_BYTES {
        return Err(ConfigError::invalid("file", "exceeds 65536 bytes"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ConfigError::invalid("file", "must be strict UTF-8"))
}

pub fn load_path(path: &Path) -> Result<FileConfig, ConfigError> {
    read_source_path(path)
        .and_then(|source| {
            source.map_or_else(|| Ok(FileConfig::default()), |source| parse(&source))
        })
        .map_err(|mut error| {
            error.location = path.to_string_lossy().into_owned();
            error
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static ID: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "agenmux-schema-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovery_uses_only_absolute_roots() {
        let p = |s| Some(Path::new(s));
        assert_eq!(
            discover(p("/xdg"), p("/home")),
            Some("/xdg/agenmux/config.toml".into())
        );
        for xdg in [None, p(""), p("relative")] {
            assert_eq!(
                discover(xdg, p("/home")),
                Some("/home/.config/agenmux/config.toml".into())
            );
            for home in [None, p(""), p("relative")] {
                assert_eq!(discover(xdg, home), None);
            }
        }
        assert_eq!(
            discover(p("/xdg"), None),
            Some("/xdg/agenmux/config.toml".into())
        );
    }

    #[test]
    fn loader_checks_opened_files_bytes_and_utf8() {
        let dir = Temp::new();
        let path = dir.0.join("config.toml");
        assert_eq!(load_path(&path).unwrap(), FileConfig::default());
        fs::write(&path, b"\xff").unwrap();
        assert_eq!(load_path(&path).unwrap_err().exit_code(), 2);
        fs::write(&path, " ".repeat(MAX_BYTES)).unwrap();
        assert!(load_path(&path).is_ok());
        fs::write(&path, " ".repeat(MAX_BYTES + 1)).unwrap();
        assert!(load_path(&path).is_err());
        assert!(parse(&" ".repeat(MAX_BYTES + 1)).is_err());
        fs::write(&path, "version = 1").unwrap();
        let link = dir.0.join("link");
        symlink(&path, &link).unwrap();
        assert_eq!(load_path(&link).unwrap().version, Some(1));
        fs::remove_file(&path).unwrap();
        assert_eq!(load_path(&link).unwrap_err().exit_code(), 1);
        assert_eq!(
            load_path(&link.join("nested/config.toml"))
                .unwrap_err()
                .exit_code(),
            1
        );
        assert_eq!(load_path(&dir.0).unwrap_err().exit_code(), 2);
        assert_eq!(
            load_path(Path::new("/dev/null")).unwrap_err().exit_code(),
            2
        );
        let fifo = dir.0.join("fifo");
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert_eq!(load_path(&fifo).unwrap_err().exit_code(), 2);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn diagnostics_escape_controls_and_do_not_render_source() {
        let dir = Temp::new();
        let path = dir.0.join("bad\n\u{1b}.toml");
        fs::write(
            &path,
            "[display]\nsidebar_width = 0\n# private source sentinel",
        )
        .unwrap();
        let diagnostic = load_path(&path).unwrap_err().to_string();
        assert!(diagnostic.contains("display.sidebar_width"));
        assert!(!diagnostic.contains('\n') && !diagnostic.contains('\u{1b}'));
        assert!(!diagnostic.contains("private source sentinel"));
        let diagnostic = parse("[theme]\ncommand = 'private source sentinel'")
            .unwrap_err()
            .to_string();
        assert!(!diagnostic.contains("private source sentinel"));
        for source in [
            "version = 'private source sentinel'",
            "[display]\nsidebar_width = 'private source sentinel'",
            "[behavior]\nnotifications = 'private source sentinel'",
            "[keys.normal]\nhelp = 'private source sentinel'",
            "[theme]\nbase = 'private source sentinel'",
            "[behavior]\nnotifications = '''\n[private source sentinel]\nprivate source sentinel = value\n'''",
            "version = @private source sentinel",
        ] {
            let diagnostic = parse(source).unwrap_err().to_string();
            assert!(!diagnostic.contains("private source sentinel"));
            assert!(diagnostic.contains("line ") && diagnostic.contains("column "));
            assert!(diagnostic.contains("invalid TOML syntax") || diagnostic.contains("invalid configuration schema"));
        }
    }

    #[test]
    fn editable_document_preserves_comments_and_validates_changes() {
        let source = "# keep me\nversion = 1\n\n[display]\nmode = \"split\" # layout\n";
        let edited = edit_document(source, "display.mode", Some("\"popup\"")).unwrap();
        assert!(edited.contains("# keep me"));
        assert!(edited.contains("mode = \"popup\" # layout"));
        assert_eq!(
            resolve(&parse(&edited).unwrap(), &Default::default())
                .unwrap()
                .mode,
            DisplayMode::Popup
        );
        assert!(edit_document(&edited, "display.sidebar_width", Some("0")).is_err());
        let framed = edit_document(&edited, "display.show_frame", Some("false")).unwrap();
        assert!(framed.contains("show_frame = false"));
        assert!(
            !resolve(&parse(&framed).unwrap(), &Default::default())
                .unwrap()
                .show_frame
        );
    }

    #[test]
    fn launcher_settings_can_be_edited_and_reverted() {
        let source = "# retain\nversion = 1\n[quick_launchers.nvim]\ncommand = \"nvim\"\n";
        let edited = edit_document(
            source,
            "quick_launchers.nvim.args",
            Some("[\"--clean\", \"two words\"]"),
        )
        .unwrap();
        assert!(edited.contains("# retain"));
        let config = resolve(&parse(&edited).unwrap(), &BTreeMap::new()).unwrap();
        let nvim = config
            .quick_launchers
            .iter()
            .find(|launcher| launcher.id == "nvim")
            .unwrap();
        assert_eq!(nvim.args, ["--clean", "two words"]);
        assert!(edit_document(&edited, "quick_launchers.nvim.args", Some("[1]")).is_err());

        let reverted = revert_document(&edited).unwrap();
        assert!(reverted.contains("# retain"));
        assert!(!reverted.contains("quick_launchers"));
        assert_eq!(
            resolve(&parse(&reverted).unwrap(), &BTreeMap::new())
                .unwrap()
                .quick_launchers,
            resolve(&parse("").unwrap(), &BTreeMap::new())
                .unwrap()
                .quick_launchers
        );
    }

    #[test]
    fn revert_removes_only_persisted_customizations() {
        let source =
            "# keep me\nversion = 1\n[display]\nmode = \"popup\"\n[theme]\nbase = \"light\"\n";
        let reverted = revert_document(source).unwrap();
        assert!(reverted.contains("# keep me"));
        assert!(reverted.contains("version = 1"));
        assert!(!reverted.contains("mode =") && !reverted.contains("base ="));
        assert_eq!(
            parse(&reverted).unwrap(),
            FileConfig {
                version: Some(1),
                ..Default::default()
            }
        );
    }

    #[test]
    fn command_icons_are_file_only_and_survive_revert() {
        let source =
            "[display]\nmode = \"popup\"\n[[command_icons]]\nmatch = \"btop\"\nicon = \"B\"\n";
        for name in [
            "command_icons",
            "command_icons[0]",
            "command_icons[0].icon",
            "command_icons.0.icon",
        ] {
            assert!(
                edit_document(source, name, Some("\"X\"")).is_err(),
                "{name}"
            );
        }
        let reverted = revert_document(source).unwrap();
        assert!(!reverted.contains("mode ="), "{reverted}");
        assert_eq!(
            parse(&reverted).unwrap().command_icons,
            parse(source).unwrap().command_icons,
            "the settings view never shows rules, so revert keeps them"
        );
    }

    #[test]
    fn atomic_save_preserves_a_symlink_and_rejects_invalid_output() {
        let dir = Temp::new();
        let target = dir.0.join("real.toml");
        let link = dir.0.join("config.toml");
        fs::write(&target, "version = 1\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        save_document(&link, "version = 1\n[behavior]\nnotifications = false\n").unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let before = fs::read_to_string(&target).unwrap();
        assert!(save_document(&link, "[display]\nsidebar_width = 0\n").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), before);

        let dangling_target = dir.0.join("created.toml");
        let dangling_link = dir.0.join("dangling.toml");
        #[cfg(unix)]
        std::os::unix::fs::symlink("created.toml", &dangling_link).unwrap();
        save_document(&dangling_link, "version = 1\n").unwrap();
        assert!(fs::symlink_metadata(&dangling_link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(dangling_target).unwrap(),
            "version = 1\n"
        );

        let directory_link = dir.0.join("directory.toml");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir.0, &directory_link).unwrap();
        assert!(save_document(&directory_link, "version = 1\n")
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));
    }
}
