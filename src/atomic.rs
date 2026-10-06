//! Writing a file so that a crash leaves the old content or the new one,
//! never half of it.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// Writes through a temporary file in the same directory, which is created
/// when it is missing.
///
/// A file that is a link is written where the link points, so the link stays
/// one, and a file that exists keeps the permissions it has.
pub fn write(path: &Path, text: &str) -> Result<()> {
    // The rename below would put a plain file in the place of a link.
    let followed = follow(path);
    let path = followed.as_deref().unwrap_or(path);
    let cannot_write = || format!("cannot write {}", path.display());
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temporary = dir.join(name);
    // What an interrupted write left is in the way, whatever it is: a link
    // there would be written through, a read-only file would refuse.
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(cannot_write),
    }
    let permissions = fs::metadata(path)
        .ok()
        .map(|existing| existing.permissions());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    // Private from the start: a file only its owner reads is never, for a
    // moment, one others can.
    #[cfg(unix)]
    if let Some(permissions) = &permissions {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        options.mode(permissions.mode());
    }
    let mut file = options.open(&temporary).with_context(cannot_write)?;
    file.write_all(text.as_bytes()).with_context(cannot_write)?;
    drop(file);
    if let Some(permissions) = permissions {
        fs::set_permissions(&temporary, permissions).with_context(cannot_write)?;
    }
    fs::rename(&temporary, path).with_context(cannot_write)
}

/// Where `path` leads when it is a link, the file it points at need not exist
/// yet. `None` when it is not a link.
fn follow(path: &Path) -> Option<PathBuf> {
    if let Ok(real) = fs::canonicalize(path) {
        return Some(real);
    }
    // A link to a file that is not there: one step, as far as it can be read.
    let target = fs::read_link(path).ok()?;
    Some(match path.parent() {
        Some(dir) if target.is_relative() => dir.join(target),
        _ => target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_created_with_its_directory_and_then_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("file.toml");
        write(&path, "one\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\n");
        write(&path, "two\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        // Nothing is left beside it.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
}

// These tests need permission bits and symbolic links as a Unix has them.
#[cfg(test)]
#[cfg(unix)]
mod unix_tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    #[test]
    fn a_private_file_stays_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        fs::write(&path, "old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        write(&path, "new\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
    }

    #[test]
    fn a_link_to_a_file_that_is_not_there_yet_is_followed_too() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfiles").join("jobs.toml");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let link = dir.path().join("jobs.toml");
        // Relative, as a dotfiles tool writes it.
        symlink("dotfiles/jobs.toml", &link).unwrap();

        write(&link, "new\n").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new\n");
    }

    /// What an interrupted write may have left beside the file.
    #[test]
    fn a_leftover_temporary_file_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        let victim = dir.path().join("victim");
        fs::write(&victim, "untouched\n").unwrap();
        symlink(&victim, dir.path().join("jobs.toml.tmp")).unwrap();

        write(&path, "new\n").unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched\n");
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");

        // One that cannot be written to does not block the next write.
        let stale = dir.path().join("jobs.toml.tmp");
        fs::write(&stale, "stale").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o400)).unwrap();
        write(&path, "newer\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer\n");
    }

    /// A jobs file kept in a dotfiles repository is a link into it.
    #[test]
    fn a_link_stays_a_link_and_what_it_points_at_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfiles").join("jobs.toml");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "old\n").unwrap();
        let link = dir.path().join("jobs.toml");
        symlink(&target, &link).unwrap();

        write(&link, "new\n").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new\n");
    }
}
