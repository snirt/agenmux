// Where this server's daemon, sidebar panes and key senders meet. Each
// backend resolves its own directory; creating it safely is shared.
use super::{is_rex, rex, tmux};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;

/// The runtime directory of the server this process works with.
pub fn runtime_dir() -> PathBuf {
    if is_rex() {
        rex::runtime_dir()
    } else {
        tmux::runtime_dir()
    }
}

/// `runtime_dir`, created private to this user.
pub fn prepare_runtime_dir() -> std::io::Result<PathBuf> {
    prepare_dir(runtime_dir())
}

/// `path`, created private to this user. Refuses one that is not a
/// directory, belongs to someone else, or others can read.
pub fn prepare_dir(path: PathBuf) -> std::io::Result<PathBuf> {
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::other("unsafe runtime directory"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_runtime_dir_is_private_and_owned() {
        let base = std::env::temp_dir().join(format!("agenmux-runtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = prepare_dir(base.clone()).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        // reused as is
        assert!(prepare_dir(base.clone()).is_ok());
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_dir(base.clone()).is_err(), "others can read it");
        std::fs::remove_dir_all(&base).unwrap();
        let file = base.with_extension("file");
        std::fs::write(&file, "").unwrap();
        assert!(prepare_dir(file.clone()).is_err(), "not a directory");
        std::fs::remove_file(file).unwrap();
    }
}
