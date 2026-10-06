//! One backend per operating system scheduler. otto never runs a daemon of its
//! own: it writes the unit the OS scheduler understands and lets it wake `otto run`.

mod launchd;
pub mod runner;
mod systemd;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::config::Job;

/// A file a scheduler backend wants on disk.
#[derive(Debug, PartialEq, Eq)]
pub struct Unit {
    pub path: PathBuf,
    pub contents: String,
}

pub trait Scheduler {
    fn name(&self) -> &'static str;

    /// The files that make the OS run `<otto> run <job_name>` on the job's schedule.
    fn units(&self, job_name: &str, job: &Job, otto: &Path) -> Result<Vec<Unit>>;
}

/// The backend for the machine otto is running on.
pub fn native() -> Result<Box<dyn Scheduler>> {
    if cfg!(target_os = "macos") {
        Ok(Box::new(launchd::Launchd::for_user()?))
    } else if cfg!(target_os = "linux") {
        Ok(Box::new(systemd::Systemd::for_user()?))
    } else {
        // Windows Task Scheduler is the planned third backend.
        bail!("no scheduler backend for this platform yet")
    }
}
