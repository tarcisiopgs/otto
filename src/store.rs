//! What otto remembers about a job between runs: whether it is paused or set
//! to skip its next run, and the record of every run. Nothing else knows the
//! layout of the state directory.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// What the user asked of a job's schedule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct State {
    /// No scheduled run starts the agent until the job is resumed.
    pub paused: bool,
    /// The next scheduled run does not start the agent, then this clears.
    pub skip_next: bool,
}

impl State {
    pub fn label(self) -> &'static str {
        if self.paused {
            "paused"
        } else if self.skip_next {
            "skip next"
        } else {
            "active"
        }
    }
}

/// The state directory, one folder per job.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Store {
        Store { root }
    }

    fn job_dir(&self, job: &str) -> PathBuf {
        self.root.join(job)
    }

    /// A job nobody paused or skipped has no state file.
    pub fn state(&self, job: &str) -> Result<State> {
        let path = self.job_dir(job).join("state.toml");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(State::default()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
    }

    pub fn set_state(&self, job: &str, state: State) -> Result<()> {
        let text = toml::to_string(&state).context("cannot encode the job state")?;
        write_atomic(&self.job_dir(job).join("state.toml"), &text)
    }
}

/// Writes through a temporary file in the same directory, so a crash leaves
/// either the old content or the new one, never half of it.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temporary = dir.join(name);
    fs::write(&temporary, text).with_context(|| format!("cannot write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn store() -> (TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn a_job_without_a_state_file_is_active() {
        let (_dir, store) = store();
        assert_eq!(store.state("report").unwrap(), State::default());
        assert_eq!(State::default().label(), "active");
    }

    #[test]
    fn state_survives_a_round_trip() {
        let (dir, store) = store();
        let state = State {
            paused: true,
            skip_next: true,
        };
        store.set_state("report", state).unwrap();
        assert_eq!(store.state("report").unwrap(), state);
        assert!(dir.path().join("report/state.toml").is_file());
    }

    #[test]
    fn paused_wins_over_skip_next_in_the_label() {
        let both = State {
            paused: true,
            skip_next: true,
        };
        let skip = State {
            paused: false,
            skip_next: true,
        };
        assert_eq!(both.label(), "paused");
        assert_eq!(skip.label(), "skip next");
    }

    #[test]
    fn an_unreadable_state_file_is_an_error_that_names_it() {
        let (dir, store) = store();
        fs::create_dir(dir.path().join("report")).unwrap();
        fs::write(dir.path().join("report/state.toml"), "paused = 3").unwrap();
        let error = store.state("report").unwrap_err();
        assert!(format!("{error:#}").contains("state.toml"));
    }
}
