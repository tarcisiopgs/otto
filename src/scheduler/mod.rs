//! One backend per operating system scheduler. otto never runs a daemon of its
//! own: it writes the unit the OS scheduler understands and lets it wake `otto run`.

mod launchd;
pub mod runner;
mod systemd;

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::config::Job;

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
}

impl Context {
    /// Where the output of every run of a job is appended.
    pub fn log_file(&self, job_name: &str) -> PathBuf {
        self.log_dir.join(format!("{job_name}.log"))
    }
}

pub trait Scheduler {
    fn name(&self) -> &'static str;

    /// The files that make the OS run `<otto> run <job_name>` on the job's schedule.
    fn units(&self, job_name: &str, job: &Job, ctx: &Context) -> Result<Vec<Unit>>;
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
