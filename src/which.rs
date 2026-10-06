//! Finding a program on the `PATH`, the way the system would when asked to
//! start it. Windows adds an extension (`claude` is `claude.exe` or
//! `claude.cmd` there), which `std::process::Command` only does for `.exe`.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The file `program` resolves to on `path`, or `None` when no directory of
/// `path` has it.
pub fn find(program: &str, path: &OsStr) -> Option<PathBuf> {
    let extensions = extensions(program);
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

#[cfg(windows)]
fn extensions(program: &str) -> Vec<String> {
    let pathext = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    windows_extensions(program, &pathext)
}

#[cfg(not(windows))]
fn extensions(_program: &str) -> Vec<String> {
    vec![String::new()]
}

/// What Windows appends to `program` when looking for it, as cmd.exe does: a
/// name that already has an extension is taken as it is, and a bare name gets
/// each extension of `PATHEXT` and is never tried bare. npm puts a shell
/// script named `codex` beside `codex.cmd`, and Windows cannot start it.
#[cfg(any(windows, test))]
fn windows_extensions(program: &str, pathext: &str) -> Vec<String> {
    if Path::new(program).extension().is_some() {
        return vec![String::new()];
    }
    pathext
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(str::to_owned)
        .collect()
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
    fn windows_never_takes_a_bare_name_for_a_program() {
        // npm leaves `codex`, a shell script, next to `codex.cmd`. Windows
        // cannot start the first, and cmd.exe would never pick it.
        assert_eq!(
            windows_extensions("codex", ".COM;.EXE;.BAT;.CMD"),
            [".COM", ".EXE", ".BAT", ".CMD"]
        );
    }

    #[test]
    fn windows_takes_a_name_that_already_has_its_extension_as_it_is() {
        assert_eq!(windows_extensions("codex.cmd", ".COM;.EXE;.BAT;.CMD"), [""]);
    }

    #[test]
    fn empty_entries_of_pathext_are_skipped() {
        assert_eq!(
            windows_extensions("codex", ";.EXE;;.CMD;"),
            [".EXE", ".CMD"]
        );
    }

    #[test]
    fn the_npm_layout_resolves_to_the_cmd_shim() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_path_buf();
        for name in ["codex", "codex.cmd", "codex.ps1"] {
            fs::write(dir.join(name), "").unwrap();
        }
        // Lowercase so the test also holds on a file system that tells
        // `.CMD` from `.cmd`, which Windows' does not.
        let extensions = windows_extensions("codex", ".com;.exe;.bat;.cmd");
        let extensions: Vec<&str> = extensions.iter().map(String::as_str).collect();

        let found = find_with(
            "codex",
            vec![dir.clone()].into_iter(),
            &extensions,
            &any_file,
        );

        assert_eq!(found, Some(dir.join("codex.cmd")));
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
