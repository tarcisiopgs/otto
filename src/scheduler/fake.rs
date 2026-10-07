//! A scheduler for tests: its units are files in a directory the test owns,
//! and loading one only records that it was asked.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Result, bail};

use super::runner::Runner;
use super::{Context, Scheduler, Unit};
use crate::config::Job;

/// A scheduler that keeps its units in a temporary directory and records
/// what it was asked to load and unload.
pub struct Fake {
    pub dir: PathBuf,
    /// Shared, so a test can still read it after handing the scheduler over.
    pub calls: Rc<RefCell<Vec<String>>>,
    /// The job this scheduler refuses to load.
    pub broken: Option<String>,
}

impl Fake {
    pub fn new(dir: PathBuf) -> Fake {
        Fake {
            dir,
            calls: Rc::default(),
            broken: None,
        }
    }
}

impl Scheduler for Fake {
    fn units(&self, job_name: &str, job: &Job, ctx: &Context) -> Result<Vec<Unit>> {
        Ok(vec![Unit {
            path: self.dir.join(format!("{job_name}.unit")),
            contents: format!("{} {}", job.schedule.at, ctx.path),
        }])
    }

    fn installed(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(job) = name.strip_suffix(".unit") {
                names.push(job.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    fn load(&self, job_name: &str, units: &[Unit], _runner: &dyn Runner) -> Result<()> {
        // Like the real backends: the file is written, then the scheduler is asked.
        for unit in units {
            fs::write(&unit.path, &unit.contents)?;
        }
        if self.broken.as_deref() == Some(job_name) {
            bail!("the scheduler refused {job_name}");
        }
        self.calls.borrow_mut().push(format!("load {job_name}"));
        Ok(())
    }

    fn unload(&self, job_name: &str, _runner: &dyn Runner) -> Result<()> {
        let _ = fs::remove_file(self.dir.join(format!("{job_name}.unit")));
        self.calls.borrow_mut().push(format!("unload {job_name}"));
        Ok(())
    }
}
