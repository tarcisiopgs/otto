//! What otto remembers about a job between runs: whether it is paused or set
//! to skip its next run, and the record of every run. Nothing else knows the
//! layout of the state directory.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::atomic;
use crate::scheduler::runner::Runner;

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
    liveness: Liveness,
}

/// How to learn whether a process still exists.
enum Liveness {
    /// `kill -0 <pid>`, on any Unix.
    Kill,
    /// `<dir>/<pid>` exists, where the kernel lists processes there.
    Procfs(PathBuf),
    /// `tasklist`, on Windows.
    Tasklist,
}

impl Store {
    pub fn new(root: PathBuf) -> Store {
        Store {
            root,
            liveness: Liveness::Kill,
        }
    }

    /// Asks this directory whether a process exists (`<procfs>/<pid>`) instead
    /// of running `kill`, which a minimal system may not have.
    pub fn with_procfs(self, procfs: PathBuf) -> Store {
        Store {
            liveness: Liveness::Procfs(procfs),
            ..self
        }
    }

    /// Asks `tasklist` whether a process exists: Windows has no `kill`.
    pub fn with_tasklist(self) -> Store {
        Store {
            liveness: Liveness::Tasklist,
            ..self
        }
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

    /// The job's directory, there and its owner's alone: the output of a run
    /// holds whatever the agent read, and no other user of the machine has
    /// any business with it.
    fn private_job_dir(&self, job: &str) -> Result<PathBuf> {
        atomic::private_dir(&self.root)?;
        let dir = self.job_dir(job);
        atomic::private_dir(&dir)?;
        Ok(dir)
    }

    pub fn set_state(&self, job: &str, state: State) -> Result<()> {
        let text = toml::to_string(&state).context("cannot encode the job state")?;
        atomic::write(&self.private_job_dir(job)?.join("state.toml"), &text)
    }

    /// Opens the record of a run that is about to start the agent.
    pub fn begin(&self, job: &str, trigger: Trigger, pid: u32, now: Timestamp) -> Result<Run> {
        let run = Run {
            id: self.new_id(job, now)?,
            started: now,
            finished: None,
            trigger,
            outcome: Outcome::Running,
            exit_code: None,
            pid: Some(pid),
        };
        self.write(job, &run)?;
        Ok(run)
    }

    /// Records a run that did not start the agent, already closed.
    pub fn record(
        &self,
        job: &str,
        trigger: Trigger,
        outcome: Outcome,
        now: Timestamp,
    ) -> Result<Run> {
        let run = Run {
            id: self.new_id(job, now)?,
            started: now,
            finished: Some(now),
            trigger,
            outcome,
            exit_code: None,
            pid: None,
        };
        self.write(job, &run)?;
        // A paused job writes one of these at every fire: keep them bounded.
        self.prune(job)?;
        Ok(run)
    }

    /// Closes a run with the agent's exit code (`None` when a signal ended it)
    /// and drops the records beyond the newest [`KEEP`].
    pub fn finish(
        &self,
        job: &str,
        run: &Run,
        exit_code: Option<i32>,
        now: Timestamp,
    ) -> Result<Run> {
        let outcome = if exit_code == Some(0) {
            Outcome::Ok
        } else {
            Outcome::Failed
        };
        self.close(job, run, outcome, exit_code, now)
    }

    /// Closes as failed a run whose agent exited with 0 without its output
    /// ending as the job expects: the record keeps the 0, `failed (0)`.
    pub fn finish_unmet(&self, job: &str, run: &Run, now: Timestamp) -> Result<Run> {
        self.close(job, run, Outcome::Failed, Some(0), now)
    }

    fn close(
        &self,
        job: &str,
        run: &Run,
        outcome: Outcome,
        exit_code: Option<i32>,
        now: Timestamp,
    ) -> Result<Run> {
        let done = Run {
            finished: Some(now),
            outcome,
            exit_code,
            ..run.clone()
        };
        self.write(job, &done)?;
        self.prune(job)?;
        Ok(done)
    }

    /// The job's runs, newest first. An open run whose process is gone is
    /// closed as interrupted on the way.
    pub fn runs(&self, job: &str, runner: &dyn Runner, now: Timestamp) -> Result<Vec<Run>> {
        let dir = self.job_dir(job);
        let mut runs = Vec::new();
        for id in self.ids(job)? {
            let path = dir.join(format!("{id}.toml"));
            // A record cut short by a crash is not worth losing the listing over.
            let Some(mut run) = read_run(&path) else {
                continue;
            };
            if run.outcome == Outcome::Running && !self.is_alive(run.pid, runner) {
                // The run may have closed its own record while the process was
                // being checked: read it again before calling it interrupted.
                // A process that is gone writes nothing more, so this settles it.
                let current = match fs::read_to_string(&path) {
                    Ok(text) => toml::from_str::<Run>(&text).ok(),
                    // Pruned in the meantime: writing it back would resurrect it.
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(_) => None,
                };
                match current {
                    Some(current) if current.outcome != Outcome::Running => run = current,
                    _ => {
                        run.outcome = Outcome::Interrupted;
                        run.finished = Some(now);
                        self.write(job, &run)?;
                    }
                }
            }
            runs.push(run);
        }
        Ok(runs)
    }

    pub fn is_running(&self, job: &str, runner: &dyn Runner, now: Timestamp) -> Result<bool> {
        Ok(self
            .runs(job, runner, now)?
            .iter()
            .any(|run| run.outcome == Outcome::Running))
    }

    /// Where the output of a run is kept.
    pub fn log_path(&self, job: &str, run_id: &str) -> Result<PathBuf> {
        if !is_run_id(run_id) {
            bail!("{run_id:?} is not a run id");
        }
        Ok(self.job_dir(job).join(format!("{run_id}.log")))
    }

    /// Whether the process that owns a run is still there. When nothing can
    /// answer, the run counts as alive: calling a live run interrupted loses
    /// its outcome, calling a dead one alive only delays the correction.
    fn is_alive(&self, pid: Option<u32>, runner: &dyn Runner) -> bool {
        let Some(pid) = pid else {
            return false;
        };
        let pid = pid.to_string();
        match &self.liveness {
            Liveness::Procfs(procfs) => procfs.join(&pid).exists(),
            Liveness::Kill => runner
                .run("kill", &["-0", &pid])
                .map_or(true, |output| output.success),
            // One CSV line per match, with the pid quoted; a sentence when
            // there is none. The exit status is zero either way.
            Liveness::Tasklist => {
                let filter = format!("PID eq {pid}");
                runner
                    .run("tasklist", &["/FI", &filter, "/FO", "CSV", "/NH"])
                    .map_or(true, |output| {
                        !output.success || output.stdout.contains(&format!("\"{pid}\""))
                    })
            }
        }
    }

    /// The ids with a record on disk, newest first.
    fn ids(&self, job: &str) -> Result<Vec<String>> {
        let dir = self.job_dir(job);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", dir.display()));
            }
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot read {}", dir.display()))?;
            let name = entry.file_name();
            let id = name.to_str().and_then(|name| name.strip_suffix(".toml"));
            if let Some(id) = id.filter(|id| is_run_id(id)) {
                ids.push(id.to_owned());
            }
        }
        ids.sort_by(|a, b| id_order(b).cmp(&id_order(a)));
        Ok(ids)
    }

    /// The start time as an id; a later run in the same second gets a suffix
    /// one past the highest in use. Reusing a freed suffix would make a new
    /// run sort as an old one, and pruning would take it first.
    fn new_id(&self, job: &str, now: Timestamp) -> Result<String> {
        let second = now.strftime("%Y%m%dT%H%M%SZ").to_string();
        let last = self
            .ids(job)?
            .iter()
            .map(|id| id_order(id))
            .filter(|(taken, _)| *taken == second)
            .map(|(_, place)| place)
            .max();
        Ok(match last {
            Some(place) => format!("{second}-{}", place + 1),
            None => second,
        })
    }

    fn write(&self, job: &str, run: &Run) -> Result<()> {
        let text = toml::to_string(run).context("cannot encode the run record")?;
        let dir = self.private_job_dir(job)?;
        atomic::write(&dir.join(format!("{}.toml", run.id)), &text)
    }

    fn prune(&self, job: &str) -> Result<()> {
        let dir = self.job_dir(job);
        for id in self.ids(job)?.iter().skip(KEEP) {
            for extension in ["toml", "log"] {
                let path = dir.join(format!("{id}.{extension}"));
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("cannot remove {}", path.display()));
                    }
                }
            }
        }
        Ok(())
    }
}

/// How many runs of a job are remembered.
pub const KEEP: usize = 50;

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Scheduled,
    Manual,
}

impl Trigger {
    pub fn label(self) -> &'static str {
        match self {
            Trigger::Scheduled => "scheduled",
            Trigger::Manual => "manual",
        }
    }
}

/// How a run ended, or that it has not yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Running,
    Ok,
    Failed,
    /// A scheduled run that did not start the agent: skip-next, or already running.
    Skipped,
    /// A scheduled run that did not start the agent because the job is paused.
    Paused,
    /// The record was left open and its process is gone.
    Interrupted,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Running => "running",
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::Skipped => "skipped",
            Outcome::Paused => "paused",
            Outcome::Interrupted => "interrupted",
        }
    }
}

/// One run of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// The start time in UTC, `YYYYMMDDTHHMMSSZ`: ids sort in time order.
    pub id: String,
    pub started: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<Timestamp>,
    pub trigger: Trigger,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The otto process that owns the run, while it lasts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

impl Run {
    /// Seconds from start to finish; `None` while it runs.
    pub fn seconds(&self) -> Option<i64> {
        self.finished
            .map(|finished| finished.duration_since(self.started).as_secs())
    }

    /// How the run ended, with the exit code of one that failed: `failed (3)`.
    pub fn outcome_label(&self) -> String {
        match (self.outcome, self.exit_code) {
            (Outcome::Failed, Some(code)) => format!("failed ({code})"),
            (outcome, _) => outcome.label().to_owned(),
        }
    }

    /// How long the agent ran. A run that did not start the agent, or has not
    /// ended, has no duration.
    pub fn duration_label(&self) -> Option<String> {
        match self.outcome {
            Outcome::Running | Outcome::Skipped | Outcome::Paused => None,
            _ => self.seconds().map(duration_label),
        }
    }
}

/// `9s`, `2m 14s`, `1h 02m`.
pub fn duration_label(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60)
    }
}

/// Digits, `T`, `Z` and `-`: a run id never names a path.
fn is_run_id(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'T' | 'Z' | '-'))
}

/// The time order of an id: its second, then its place within that second.
/// Plain text order would put `-9` after `-12`.
fn id_order(id: &str) -> (&str, u32) {
    match id.split_once('-') {
        Some((second, place)) => (second, place.parse().unwrap_or(0)),
        None => (id, 1),
    }
}

/// A record that cannot be read or understood is `None`.
fn read_run(path: &Path) -> Option<Run> {
    let text = fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::scheduler::runner::{Output, Recorder};

    fn store() -> (TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    /// Every process the store asks about is still there.
    fn alive() -> Recorder {
        Recorder::new()
    }

    /// Every process the store asks about is gone.
    fn dead() -> Recorder {
        Recorder::new().answering("kill -0", &[false])
    }

    #[test]
    fn a_run_says_how_it_ended_and_how_long_it_took() {
        let (_dir, store) = store();
        let open = store
            .begin("report", Trigger::Manual, 1, at("2026-10-05T19:16:26Z"))
            .unwrap();
        assert_eq!(open.outcome_label(), "running");
        assert_eq!(open.duration_label(), None);

        let failed = store
            .finish("report", &open, Some(3), at("2026-10-05T19:18:40Z"))
            .unwrap();
        assert_eq!(failed.outcome_label(), "failed (3)");
        assert_eq!(failed.duration_label().as_deref(), Some("2m 14s"));

        let ok = Run {
            outcome: Outcome::Ok,
            exit_code: Some(0),
            ..failed.clone()
        };
        assert_eq!(ok.outcome_label(), "ok");

        let skipped = store
            .record(
                "report",
                Trigger::Scheduled,
                Outcome::Skipped,
                at("2026-10-05T20:00:00Z"),
            )
            .unwrap();
        assert_eq!(skipped.outcome_label(), "skipped");
        assert_eq!(skipped.duration_label(), None);
    }

    #[test]
    fn a_run_opens_then_closes_with_its_exit_code() {
        let (_dir, store) = store();
        let run = store
            .begin(
                "report",
                Trigger::Scheduled,
                4242,
                at("2026-10-05T19:16:26Z"),
            )
            .unwrap();
        assert_eq!(run.id, "20261005T191626Z");
        assert_eq!(run.outcome, Outcome::Running);
        assert!(
            store
                .is_running("report", &alive(), at("2026-10-05T19:17:00Z"))
                .unwrap()
        );

        let done = store
            .finish("report", &run, Some(3), at("2026-10-05T19:18:40Z"))
            .unwrap();
        assert_eq!(done.outcome, Outcome::Failed);
        assert_eq!(done.exit_code, Some(3));
        assert_eq!(done.seconds(), Some(134));
        assert!(
            !store
                .is_running("report", &alive(), at("2026-10-05T19:19:00Z"))
                .unwrap()
        );
        assert_eq!(
            store
                .runs("report", &alive(), at("2026-10-05T19:19:00Z"))
                .unwrap(),
            [done]
        );
    }

    #[test]
    fn exit_zero_is_ok_and_a_signal_is_failed() {
        let (_dir, store) = store();
        let first = store
            .begin("report", Trigger::Manual, 1, at("2026-10-05T19:00:00Z"))
            .unwrap();
        let ok = store
            .finish("report", &first, Some(0), at("2026-10-05T19:00:05Z"))
            .unwrap();
        assert_eq!(ok.outcome, Outcome::Ok);

        let second = store
            .begin("report", Trigger::Manual, 1, at("2026-10-05T19:01:00Z"))
            .unwrap();
        let killed = store
            .finish("report", &second, None, at("2026-10-05T19:01:05Z"))
            .unwrap();
        assert_eq!(killed.outcome, Outcome::Failed);
        assert_eq!(killed.exit_code, None);
    }

    #[test]
    fn the_liveness_check_asks_for_the_recorded_pid() {
        let (_dir, store) = store();
        store
            .begin("report", Trigger::Manual, 4242, at("2026-10-05T19:00:00Z"))
            .unwrap();
        let runner = alive();
        store
            .is_running("report", &runner, at("2026-10-05T19:00:01Z"))
            .unwrap();
        assert_eq!(runner.calls(), ["kill -0 4242"]);
    }

    #[test]
    fn an_open_run_whose_process_is_gone_becomes_interrupted() {
        let (_dir, store) = store();
        store
            .begin(
                "report",
                Trigger::Scheduled,
                4242,
                at("2026-10-05T19:00:00Z"),
            )
            .unwrap();

        let later = at("2026-10-05T20:00:00Z");
        let runs = store.runs("report", &dead(), later).unwrap();
        assert_eq!(runs[0].outcome, Outcome::Interrupted);
        assert_eq!(runs[0].finished, Some(later));

        // It stays closed: a later read does not need the process check to agree.
        let again = store.runs("report", &alive(), later).unwrap();
        assert_eq!(again[0].outcome, Outcome::Interrupted);
        assert!(!store.is_running("report", &alive(), later).unwrap());
    }

    #[test]
    fn a_skipped_run_is_recorded_already_closed() {
        let (_dir, store) = store();
        let run = store
            .record(
                "report",
                Trigger::Scheduled,
                Outcome::Skipped,
                at("2026-10-05T19:16:26Z"),
            )
            .unwrap();
        assert_eq!(run.finished, Some(run.started));
        assert_eq!(run.pid, None);
        assert!(!store.is_running("report", &alive(), run.started).unwrap());
    }

    #[test]
    fn two_runs_in_the_same_second_do_not_overwrite_each_other() {
        let (_dir, store) = store();
        let now = at("2026-10-05T19:16:26Z");
        let skipped = store
            .record("report", Trigger::Scheduled, Outcome::Skipped, now)
            .unwrap();
        let manual = store.begin("report", Trigger::Manual, 7, now).unwrap();
        assert_eq!(skipped.id, "20261005T191626Z");
        assert_eq!(manual.id, "20261005T191626Z-2");
        assert_eq!(store.runs("report", &alive(), now).unwrap().len(), 2);
    }

    #[test]
    fn runs_come_newest_first() {
        let (_dir, store) = store();
        for minute in ["19:00", "19:02", "19:01"] {
            store
                .record(
                    "report",
                    Trigger::Scheduled,
                    Outcome::Paused,
                    at(&format!("2026-10-05T{minute}:00Z")),
                )
                .unwrap();
        }
        let ids: Vec<String> = store
            .runs("report", &alive(), at("2026-10-05T20:00:00Z"))
            .unwrap()
            .into_iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(
            ids,
            ["20261005T190200Z", "20261005T190100Z", "20261005T190000Z"]
        );
    }

    #[test]
    fn only_the_newest_fifty_runs_are_kept() {
        let (dir, store) = store();
        let start = at("2026-10-05T00:00:00Z");
        let mut ids = Vec::new();
        for minute in 0..51 {
            let now = start + jiff::SignedDuration::from_mins(minute);
            let run = store.begin("report", Trigger::Scheduled, 1, now).unwrap();
            fs::write(store.log_path("report", &run.id).unwrap(), "output").unwrap();
            store.finish("report", &run, Some(0), now).unwrap();
            ids.push(run.id);
        }

        let runs = store
            .runs("report", &alive(), at("2026-10-05T02:00:00Z"))
            .unwrap();
        assert_eq!(runs.len(), KEEP);
        let job = dir.path().join("report");
        assert!(!job.join(format!("{}.toml", ids[0])).exists());
        assert!(!job.join(format!("{}.log", ids[0])).exists());
        assert!(job.join(format!("{}.toml", ids[50])).exists());
        assert!(job.join(format!("{}.log", ids[50])).exists());
    }

    #[test]
    fn a_corrupt_record_is_left_out_of_the_listing() {
        let (dir, store) = store();
        let good = store
            .record(
                "report",
                Trigger::Scheduled,
                Outcome::Skipped,
                at("2026-10-05T19:00:00Z"),
            )
            .unwrap();
        fs::write(
            dir.path().join("report/20261005T180000Z.toml"),
            "not toml [",
        )
        .unwrap();
        assert_eq!(
            store
                .runs("report", &alive(), at("2026-10-05T20:00:00Z"))
                .unwrap(),
            [good]
        );
    }

    #[test]
    fn a_job_that_never_ran_has_no_runs() {
        let (_dir, store) = store();
        assert!(
            store
                .runs("report", &alive(), at("2026-10-05T19:00:00Z"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn log_path_refuses_anything_that_is_not_a_run_id() {
        let (dir, store) = store();
        assert_eq!(
            store.log_path("report", "20261005T191626Z-2").unwrap(),
            dir.path().join("report/20261005T191626Z-2.log")
        );
        assert!(store.log_path("report", "../../etc/passwd").is_err());
        assert!(store.log_path("report", "").is_err());
    }

    /// A process check during which the run finishes: the check comes back
    /// "gone" for a run that has just closed its own record.
    struct FinishesMeanwhile<'a> {
        store: &'a Store,
        run: &'a Run,
    }

    impl Runner for FinishesMeanwhile<'_> {
        fn run(&self, _program: &str, _args: &[&str]) -> Result<Output> {
            self.store
                .finish("report", self.run, Some(0), at("2026-10-05T19:05:00Z"))?;
            Ok(Output {
                success: false,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn a_run_that_ends_while_being_checked_keeps_its_own_outcome() {
        let (_dir, store) = store();
        let run = store
            .begin(
                "report",
                Trigger::Scheduled,
                4242,
                at("2026-10-05T19:00:00Z"),
            )
            .unwrap();
        let racing = FinishesMeanwhile {
            store: &store,
            run: &run,
        };

        let seen = store
            .runs("report", &racing, at("2026-10-05T19:05:01Z"))
            .unwrap();

        assert_eq!(seen[0].outcome, Outcome::Ok);
        let later = store
            .runs("report", &alive(), at("2026-10-05T19:06:00Z"))
            .unwrap();
        assert_eq!(later[0].outcome, Outcome::Ok);
    }

    /// A process check during which the record is pruned away.
    struct PrunedMeanwhile {
        record: PathBuf,
    }

    impl Runner for PrunedMeanwhile {
        fn run(&self, _program: &str, _args: &[&str]) -> Result<Output> {
            fs::remove_file(&self.record)?;
            Ok(Output {
                success: false,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn a_record_removed_while_being_checked_is_not_brought_back() {
        let (dir, store) = store();
        let run = store
            .begin(
                "report",
                Trigger::Scheduled,
                4242,
                at("2026-10-05T19:00:00Z"),
            )
            .unwrap();
        let record = dir.path().join(format!("report/{}.toml", run.id));
        let pruning = PrunedMeanwhile {
            record: record.clone(),
        };

        let seen = store
            .runs("report", &pruning, at("2026-10-05T19:05:00Z"))
            .unwrap();

        assert!(seen.is_empty());
        assert!(!record.exists());
    }

    #[test]
    fn runs_in_the_same_second_keep_their_order_past_nine() {
        let (_dir, store) = store();
        let now = at("2026-10-05T19:00:00Z");
        for _ in 0..12 {
            store
                .record("report", Trigger::Scheduled, Outcome::Paused, now)
                .unwrap();
        }
        let ids: Vec<String> = store
            .runs("report", &alive(), now)
            .unwrap()
            .into_iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(ids[0], "20261005T190000Z-12");
        assert_eq!(ids[1], "20261005T190000Z-11");
        assert_eq!(ids[3], "20261005T190000Z-9");
        assert_eq!(ids[11], "20261005T190000Z");
    }

    #[test]
    fn pruning_in_a_busy_second_drops_the_oldest_runs() {
        let (_dir, store) = store();
        let now = at("2026-10-05T19:00:00Z");
        for _ in 0..55 {
            let run = store.begin("report", Trigger::Scheduled, 1, now).unwrap();
            store.finish("report", &run, Some(0), now).unwrap();
        }
        let ids: Vec<String> = store
            .runs("report", &alive(), now)
            .unwrap()
            .into_iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(ids.len(), KEEP);
        assert_eq!(ids[0], "20261005T190000Z-55");
        assert_eq!(ids[KEEP - 1], "20261005T190000Z-6");
    }

    /// A machine without a `kill` program.
    struct CannotStart;

    impl Runner for CannotStart {
        fn run(&self, program: &str, _args: &[&str]) -> Result<Output> {
            bail!("cannot start {program}")
        }
    }

    #[test]
    fn a_process_that_cannot_be_checked_counts_as_running() {
        let (_dir, store) = store();
        store
            .begin(
                "report",
                Trigger::Scheduled,
                4242,
                at("2026-10-05T19:00:00Z"),
            )
            .unwrap();

        let runs = store
            .runs("report", &CannotStart, at("2026-10-05T19:05:00Z"))
            .unwrap();

        assert_eq!(runs[0].outcome, Outcome::Running);
    }

    #[test]
    fn tasklist_answers_where_there_is_no_kill() {
        let (dir, _) = store();
        let store = Store::new(dir.path().join("jobs")).with_tasklist();
        store
            .begin("here", Trigger::Scheduled, 4242, at("2026-10-05T19:00:00Z"))
            .unwrap();
        store
            .begin("gone", Trigger::Scheduled, 7, at("2026-10-05T19:00:00Z"))
            .unwrap();
        let runner = Recorder::new()
            .printing(
                "tasklist /FI PID eq 4242",
                "\"otto.exe\",\"4242\",\"Console\",\"1\",\"5,000 K\"\r\n",
            )
            .printing(
                "tasklist /FI PID eq 7",
                "INFO: No tasks are running which match the specified criteria.\r\n",
            );
        let now = at("2026-10-05T19:05:00Z");

        assert!(store.is_running("here", &runner, now).unwrap());
        assert!(!store.is_running("gone", &runner, now).unwrap());
        assert_eq!(runner.calls()[0], "tasklist /FI PID eq 4242 /FO CSV /NH");
    }

    #[test]
    fn procfs_answers_without_running_a_program() {
        let (dir, _) = store();
        let proc_dir = dir.path().join("proc");
        fs::create_dir_all(proc_dir.join("4242")).unwrap();
        let store = Store::new(dir.path().join("jobs")).with_procfs(proc_dir);
        store
            .begin("here", Trigger::Scheduled, 4242, at("2026-10-05T19:00:00Z"))
            .unwrap();
        store
            .begin("gone", Trigger::Scheduled, 7, at("2026-10-05T19:00:00Z"))
            .unwrap();
        let runner = Recorder::new();
        let now = at("2026-10-05T19:05:00Z");

        assert!(store.is_running("here", &runner, now).unwrap());
        assert!(!store.is_running("gone", &runner, now).unwrap());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn runs_that_did_not_start_the_agent_are_pruned_too() {
        let (_dir, store) = store();
        let start = at("2026-10-05T00:00:00Z");
        for minute in 0..60 {
            let now = start + jiff::SignedDuration::from_mins(minute);
            store
                .record("report", Trigger::Scheduled, Outcome::Paused, now)
                .unwrap();
        }
        let runs = store
            .runs("report", &alive(), at("2026-10-05T02:00:00Z"))
            .unwrap();
        assert_eq!(runs.len(), KEEP);
    }

    #[test]
    fn durations_read_like_a_person_wrote_them() {
        assert_eq!(duration_label(9), "9s");
        assert_eq!(duration_label(134), "2m 14s");
        assert_eq!(duration_label(3720), "1h 02m");
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
