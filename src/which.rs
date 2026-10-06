//! Finding a program on the `PATH`, the way the system would when asked to
//! start it. Windows adds an extension (`claude` is `claude.exe` or
//! `claude.cmd` there), which `std::process::Command` only does for `.exe`.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The file `program` resolves to on `path`, or `None` when no directory of
/// `path` has it.
pub fn find(program: &str, path: &OsStr) -> Option<PathBuf> {
    let extensions = extensions();
    let extensions: Vec<&str> = extensions.iter().map(String::as_str).collect();
    find_with(program, env::split_paths(path), &extensions, &is_runnable)
}

/// `find`, with what differs between systems passed in: the extensions the
/// system appends to a program name and what makes a file one it would run.
fn find_with(
    program: &str,
    dirs: impl Iterator<Item = PathBuf>,
    extensions: &[&str],
    runnable: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    for dir in dirs {
        for extension in extensions {
            let candidate = dir.join(format!("{program}{extension}"));
            if runnable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Windows tries the name as given, then each extension of `PATHEXT`.
#[cfg(windows)]
fn extensions() -> Vec<String> {
    let listed = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    std::iter::once(String::new())
        .chain(
            listed
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(str::to_owned),
        )
        .collect()
}

#[cfg(not(windows))]
fn extensions() -> Vec<String> {
    vec![String::new()]
}

/// A regular file with an execute bit.
#[cfg(unix)]
fn is_runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_runnable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn any_file(path: &Path) -> bool {
        path.is_file()
    }

    #[test]
    fn the_first_directory_that_has_the_program_wins() {
        let root = tempfile::tempdir().unwrap();
        let (first, second) = (root.path().join("a"), root.path().join("b"));
        for dir in [&first, &second] {
            fs::create_dir(dir).unwrap();
        }
        fs::write(second.join("claude"), "").unwrap();
        let dirs = vec![first.clone(), second.clone()];

        let found = find_with("claude", dirs.clone().into_iter(), &[""], &any_file);
        assert_eq!(found, Some(second.join("claude")));

        fs::write(first.join("claude"), "").unwrap();
        let found = find_with("claude", dirs.into_iter(), &[""], &any_file);
        assert_eq!(found, Some(first.join("claude")));
    }

    #[test]
    fn extensions_are_tried_in_order() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_path_buf();
        fs::write(dir.join("claude.cmd"), "").unwrap();
        let extensions = [".exe", ".cmd"];

        let found = find_with(
            "claude",
            vec![dir.clone()].into_iter(),
            &extensions,
            &any_file,
        );
        assert_eq!(found, Some(dir.join("claude.cmd")));

        fs::write(dir.join("claude.exe"), "").unwrap();
        let found = find_with(
            "claude",
            vec![dir.clone()].into_iter(),
            &extensions,
            &any_file,
        );
        assert_eq!(found, Some(dir.join("claude.exe")));
    }

    #[test]
    fn a_file_the_system_would_not_run_is_passed_over() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_path_buf();
        fs::write(dir.join("claude"), "").unwrap();

        let found = find_with("claude", vec![dir].into_iter(), &[""], &|_| false);
        assert_eq!(found, None);
    }

    #[test]
    fn nothing_is_found_on_an_empty_path() {
        let none: Vec<PathBuf> = Vec::new();
        assert_eq!(
            find_with("claude", none.into_iter(), &[""], &any_file),
            None
        );
    }
}
