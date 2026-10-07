//! One backend per operating system scheduler. otto never runs a daemon of its
//! own: it writes the unit the OS scheduler understands and lets it wake `otto run`.

// Only the tests that build a machine out of executable files use it.
#[cfg(test)]
#[cfg(unix)]
pub mod fake;
mod launchd;
pub mod runner;
mod systemd;
mod windows;

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::config::Job;
use runner::Runner;

/// Every unit whose file name starts with this belongs to otto, on every
/// scheduler. Nothing else in a units directory is ever read or removed.
const PREFIX: &str = "io.github.tarcisiopgs.otto.";

/// A file a scheduler backend wants on disk.
#[derive(Debug, PartialEq, Eq)]
pub struct Unit {
    pub path: PathBuf,
    pub contents: String,
}

/// What a unit needs to know about the machine it will run on.
#[derive(Debug, Clone)]
pub struct Context {
    /// Absolute path of the otto binary.
    pub otto: PathBuf,
    /// Absolute path of the jobs file.
    pub config: PathBuf,
    /// The `PATH` the job runs with. The OS scheduler starts with a bare one,
    /// so the agent CLI and its tools would not be found without it.
    pub path: String,
    pub log_dir: PathBuf,
    /// The version of the otto that writes the unit.
    pub version: &'static str,
}

impl Context {
    /// Where the output of every run of a job is appended.
    pub fn log_file(&self, job_name: &str) -> PathBuf {
        self.log_dir.join(format!("{job_name}.log"))
    }
}

pub trait Scheduler {
    /// The files that make the OS run `<otto> run <job_name>` on the job's schedule.
    fn units(&self, job_name: &str, job: &Job, ctx: &Context) -> Result<Vec<Unit>>;

    /// Names of the jobs that have otto units in the units directory, sorted.
    /// Units of other programs are never listed.
    fn installed(&self) -> Result<Vec<String>>;

    /// Writes the units and loads the job into the scheduler. When it fails the
    /// units may be left on disk; `unload` cleans up.
    fn load(&self, job_name: &str, units: &[Unit], runner: &dyn Runner) -> Result<()>;

    /// Unloads the job from the scheduler and removes its units.
    fn unload(&self, job_name: &str, runner: &dyn Runner) -> Result<()>;
}

/// The file names in a units directory; a directory that does not exist is empty.
fn file_names(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", dir.display()));
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", dir.display()))?;
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Removes a unit file; one that is already gone is fine.
fn remove_unit(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
    }
}

/// Writes unit files, creating their directory when it is missing.
fn write_units(dir: &Path, units: &[Unit]) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for unit in units {
        fs::write(&unit.path, &unit.contents)
            .with_context(|| format!("cannot write {}", unit.path.display()))?;
    }
    Ok(())
}

/// Text placed inside an XML element: a plist value or a task definition.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The backend for the machine otto is running on.
pub fn native() -> Result<Box<dyn Scheduler>> {
    if cfg!(target_os = "macos") {
        Ok(Box::new(launchd::Launchd::for_user()?))
    } else if cfg!(target_os = "linux") {
        Ok(Box::new(systemd::Systemd::for_user()?))
    } else if cfg!(target_os = "windows") {
        Ok(Box::new(windows::Windows::for_user()?))
    } else {
        bail!("no scheduler backend for this platform yet")
    }
}
