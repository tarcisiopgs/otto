//! Makes the OS scheduler match the jobs file. The units directory is the only
//! state: what is on disk there is what is scheduled.

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context as _, Result, bail};

use crate::config::{Config, Job};
use crate::scheduler::runner::Runner;
use crate::scheduler::{Context, Scheduler, Unit};

/// What a sync does, or did, to one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Add,
    Update,
    Remove,
    Unchanged,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Add => "added",
            Action::Update => "updated",
            Action::Remove => "removed",
            Action::Unchanged => "unchanged",
        }
    }
}

pub struct Outcome {
    pub job: String,
    pub result: Result<Action>,
}

/// Compares the units a job should have with what is on disk. `present[i]` is
/// the content found at `desired[i].path`, or `None` when the file is missing.
pub fn decide(desired: &[Unit], present: &[Option<String>]) -> Action {
    if present.iter().all(Option::is_none) {
        Action::Add
    } else if desired.len() == present.len()
        && desired
            .iter()
            .zip(present)
            .all(|(unit, found)| found.as_deref() == Some(unit.contents.as_str()))
    {
        Action::Unchanged
    } else {
        Action::Update
    }
}

/// Refuses a job that is certain to fail when the scheduler fires it.
pub fn preflight(job: &Job, path: &str) -> Result<()> {
    if !job.prompt.is_file() {
        bail!("prompt file not found: {}", job.prompt.display());
    }
    if !job.workdir.is_dir() {
        bail!("working directory not found: {}", job.workdir.display());
    }
    let program = job.agent.program();
    if !env::split_paths(path).any(|dir| is_executable(&dir.join(program))) {
        bail!("{program} not found in PATH");
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn read_present(units: &[Unit]) -> Result<Vec<Option<String>>> {
    units
        .iter()
        .map(|unit| match fs::read_to_string(&unit.path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("cannot read {}", unit.path.display()))
            }
        })
        .collect()
}

/// One job of the jobs file: check it, compare it and apply the difference.
fn sync_job(
    name: &str,
    job: &Job,
    scheduler: &dyn Scheduler,
    ctx: &Context,
    runner: &dyn Runner,
    dry_run: bool,
) -> Result<Action> {
    // A job that fails here keeps whatever unit it already has: a prompt on an
    // unmounted disk should not cost the user the schedule.
    preflight(job, &ctx.path)?;
    let units = scheduler.units(name, job, ctx)?;
    let action = decide(&units, &read_present(&units)?);
    if dry_run {
        return Ok(action);
    }
    match action {
        Action::Add => {
            prepare_logs(ctx)?;
            scheduler.load(name, &units, runner)?;
        }
        Action::Update => {
            prepare_logs(ctx)?;
            scheduler.unload(name, runner)?;
            scheduler.load(name, &units, runner)?;
        }
        Action::Remove | Action::Unchanged => {}
    }
    Ok(action)
}

/// The scheduler appends to the log file but does not create its directory.
fn prepare_logs(ctx: &Context) -> Result<()> {
    fs::create_dir_all(&ctx.log_dir)
        .with_context(|| format!("cannot create {}", ctx.log_dir.display()))
}

/// Makes the scheduler match `config`: jobs are added, updated or left alone,
/// and an otto job the scheduler has that `config` lacks is removed. Each job
/// stands alone, so one failure does not stop the others.
pub fn sync(
    config: &Config,
    scheduler: &dyn Scheduler,
    ctx: &Context,
    runner: &dyn Runner,
    dry_run: bool,
) -> Vec<Outcome> {
    let installed = match scheduler.installed() {
        Ok(installed) => installed,
        Err(error) => {
            return vec![Outcome {
                job: "*".to_owned(),
                result: Err(error),
            }];
        }
    };
    let mut outcomes: Vec<Outcome> = config
        .jobs
        .iter()
        .map(|(name, job)| Outcome {
            job: name.clone(),
            result: sync_job(name, job, scheduler, ctx, runner, dry_run),
        })
        .collect();
    for name in installed {
        if config.jobs.contains_key(&name) {
            continue;
        }
        let result = if dry_run {
            Ok(Action::Remove)
        } else {
            scheduler.unload(&name, runner).map(|()| Action::Remove)
        };
        outcomes.push(Outcome { job: name, result });
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use anyhow::bail;
    use tempfile::TempDir;

    use super::*;
    use crate::scheduler::runner::Recorder;

    /// A scheduler that keeps its units in a temporary directory and records
    /// what it was asked to load and unload.
    struct Fake {
        dir: PathBuf,
        calls: RefCell<Vec<String>>,
        broken: Option<String>,
    }

    impl Scheduler for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }

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
            if self.broken.as_deref() == Some(job_name) {
                bail!("the scheduler refused {job_name}");
            }
            for unit in units {
                fs::write(&unit.path, &unit.contents)?;
            }
            self.calls.borrow_mut().push(format!("load {job_name}"));
            Ok(())
        }

        fn unload(&self, job_name: &str, _runner: &dyn Runner) -> Result<()> {
            fs::remove_file(self.dir.join(format!("{job_name}.unit")))?;
            self.calls.borrow_mut().push(format!("unload {job_name}"));
            Ok(())
        }
    }

    /// A machine with one prompt, one working directory and `claude` on the PATH.
    struct World {
        root: TempDir,
        ctx: Context,
        fake: Fake,
    }

    impl World {
        fn new() -> World {
            let root = tempfile::tempdir().unwrap();
            let at = |name: &str| root.path().join(name);
            fs::write(at("prompt.md"), "do it").unwrap();
            for dir in ["work", "bin", "units"] {
                fs::create_dir(at(dir)).unwrap();
            }
            fs::write(at("bin/claude"), "").unwrap();
            fs::set_permissions(at("bin/claude"), fs::Permissions::from_mode(0o755)).unwrap();
            let ctx = Context {
                otto: PathBuf::from("/usr/bin/otto"),
                config: at("jobs.toml"),
                path: at("bin").to_string_lossy().into_owned(),
                log_dir: at("logs"),
            };
            let fake = Fake {
                dir: at("units"),
                calls: RefCell::new(Vec::new()),
                broken: None,
            };
            World { root, ctx, fake }
        }

        fn config(&self, text: &str) -> Config {
            Config::parse(text, self.root.path(), None).unwrap()
        }

        /// Runs a sync and returns each job with its label, or `error: <message>`.
        fn sync(&self, text: &str, dry_run: bool) -> Vec<(String, String)> {
            let config = self.config(text);
            sync(&config, &self.fake, &self.ctx, &Recorder::new(), dry_run)
                .into_iter()
                .map(|outcome| {
                    let shown = match outcome.result {
                        Ok(action) => action.label().to_owned(),
                        Err(error) => format!("error: {error:#}"),
                    };
                    (outcome.job, shown)
                })
                .collect()
        }

        fn calls(&self) -> Vec<String> {
            self.fake.calls.borrow().clone()
        }

        fn unit(&self, job: &str) -> PathBuf {
            self.fake.dir.join(format!("{job}.unit"))
        }
    }

    fn job(name: &str, agent: &str, prompt: &str, at: &str) -> String {
        format!(
            "[jobs.{name}]\nagent = \"{agent}\"\nprompt = \"{prompt}\"\nworkdir = \"work\"\nschedule = {{ at = \"{at}\" }}\n"
        )
    }

    fn pair(job: &str, shown: &str) -> (String, String) {
        (job.to_owned(), shown.to_owned())
    }

    #[test]
    fn decide_covers_the_three_states() {
        let unit = |text: &str| Unit {
            path: PathBuf::from("/x"),
            contents: text.to_owned(),
        };
        assert_eq!(decide(&[unit("a")], &[None]), Action::Add);
        assert_eq!(
            decide(&[unit("a")], &[Some("a".to_owned())]),
            Action::Unchanged
        );
        assert_eq!(
            decide(&[unit("a")], &[Some("b".to_owned())]),
            Action::Update
        );
        assert_eq!(
            decide(&[unit("a"), unit("b")], &[Some("a".to_owned()), None]),
            Action::Update
        );
    }

    #[test]
    fn a_new_job_is_added_then_left_alone() {
        let world = World::new();
        let text = job("report", "claude", "prompt.md", "09:00");

        assert_eq!(world.sync(&text, false), [pair("report", "added")]);
        assert_eq!(world.calls(), ["load report"]);
        assert!(world.ctx.log_dir.is_dir());

        assert_eq!(world.sync(&text, false), [pair("report", "unchanged")]);
        assert_eq!(world.calls(), ["load report"]);
    }

    #[test]
    fn a_changed_schedule_reloads_the_job() {
        let world = World::new();
        world.sync(&job("report", "claude", "prompt.md", "09:00"), false);

        let outcome = world.sync(&job("report", "claude", "prompt.md", "10:00"), false);

        assert_eq!(outcome, [pair("report", "updated")]);
        assert_eq!(
            world.calls(),
            ["load report", "unload report", "load report"]
        );
        assert!(
            fs::read_to_string(world.unit("report"))
                .unwrap()
                .starts_with("10:00")
        );
    }

    #[test]
    fn a_job_gone_from_the_config_is_removed() {
        let world = World::new();
        world.sync(&job("report", "claude", "prompt.md", "09:00"), false);

        assert_eq!(world.sync("", false), [pair("report", "removed")]);
        assert_eq!(world.calls(), ["load report", "unload report"]);
        assert!(!world.unit("report").exists());
    }

    #[test]
    fn dry_run_changes_nothing() {
        let world = World::new();

        let outcome = world.sync(&job("report", "claude", "prompt.md", "09:00"), true);

        assert_eq!(outcome, [pair("report", "added")]);
        assert!(world.calls().is_empty());
        assert!(!world.unit("report").exists());
        assert!(!world.ctx.log_dir.exists());
    }

    #[test]
    fn a_job_that_fails_preflight_does_not_stop_the_others() {
        let world = World::new();
        let text =
            job("a", "claude", "missing.md", "09:00") + &job("b", "claude", "prompt.md", "09:00");

        let outcome = world.sync(&text, false);

        assert_eq!(outcome[0].0, "a");
        assert!(outcome[0].1.contains("prompt file not found"));
        assert_eq!(outcome[1], pair("b", "added"));
        assert_eq!(world.calls(), ["load b"]);
    }

    #[test]
    fn preflight_names_what_is_missing() {
        let world = World::new();
        let path = &world.ctx.path;
        let config = world.config(
            &(job("ok", "claude", "prompt.md", "09:00")
                + &job("other", "codex", "prompt.md", "09:00")
                + "[jobs.nowhere]\nagent = \"claude\"\nprompt = \"prompt.md\"\nworkdir = \"gone\"\nschedule = { at = \"09:00\" }\n"),
        );
        let message = |name: &str| {
            preflight(config.job(name).unwrap(), path)
                .unwrap_err()
                .to_string()
        };

        assert!(preflight(config.job("ok").unwrap(), path).is_ok());
        assert!(message("nowhere").contains("working directory not found"));
        assert!(message("other").contains("codex not found in PATH"));

        let claude = world.root.path().join("bin/claude");
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(message("ok").contains("claude not found in PATH"));
    }

    #[test]
    fn an_installed_job_that_now_fails_preflight_is_left_untouched() {
        let world = World::new();
        let text = job("report", "claude", "prompt.md", "09:00");
        world.sync(&text, false);
        let before = fs::read_to_string(world.unit("report")).unwrap();
        fs::remove_file(world.root.path().join("prompt.md")).unwrap();

        let outcome = world.sync(&text, false);

        assert!(outcome[0].1.contains("prompt file not found"));
        assert_eq!(outcome.len(), 1);
        assert_eq!(world.calls(), ["load report"]);
        assert_eq!(fs::read_to_string(world.unit("report")).unwrap(), before);
    }

    #[test]
    fn a_failed_load_is_reported_and_the_rest_proceeds() {
        let mut world = World::new();
        world.fake.broken = Some("a".to_owned());
        let text =
            job("a", "claude", "prompt.md", "09:00") + &job("b", "claude", "prompt.md", "09:00");

        let outcome = world.sync(&text, false);

        assert!(outcome[0].1.contains("the scheduler refused a"));
        assert_eq!(outcome[1], pair("b", "added"));
    }
}
