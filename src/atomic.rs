//! Writing a file so that a crash leaves the old content or the new one,
//! never half of it.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};

/// Writes through a temporary file in the same directory, which is created
/// when it is missing.
///
/// A file that is a link is written where the link points, so the link stays
/// one, and a file that exists keeps the permissions it has.
pub fn write(path: &Path, text: &str) -> Result<()> {
    // The rename below would put a plain file in the place of a link.
    let followed = fs::canonicalize(path);
    let path = followed.as_deref().unwrap_or(path);
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temporary = dir.join(name);
    fs::write(&temporary, text).with_context(|| format!("cannot write {}", temporary.display()))?;
    if let Ok(existing) = fs::metadata(path) {
        fs::set_permissions(&temporary, existing.permissions())
            .with_context(|| format!("cannot write {}", temporary.display()))?;
    }
    fs::rename(&temporary, path).with_context(|| format!("cannot write {}", path.display()))
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
