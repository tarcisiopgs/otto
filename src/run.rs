//! One run of a job: decide whether it happens, start the agent, keep its
//! output and close the record.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;

use crate::scheduler::runner::Runner;
use crate::store::{Outcome, State, Store, Trigger};

pub struct Request<'a> {
    pub job: &'a str,
    /// The command, ready to start: the program and its arguments.
    pub argv: &'a [String],
    pub workdir: &'a Path,
    pub trigger: Trigger,
}

/// Runs the request and returns the code otto should exit with.
///
/// Pause and skip-next belong to the schedule: a scheduled run honours them,
/// a manual one neither reads nor changes them.
pub fn execute(
    request: &Request,
    store: &Store,
    runner: &dyn Runner,
    now: &dyn Fn() -> Timestamp,
) -> Result<u8> {
    let job = request.job;
    let scheduled = request.trigger == Trigger::Scheduled;
    if scheduled {
        let state = store.state(job)?;
        if state.paused {
            store.record(job, request.trigger, Outcome::Paused, now())?;
            return Ok(0);
        }
        if state.skip_next {
            store.record(job, request.trigger, Outcome::Skipped, now())?;
            store.set_state(
                job,
                State {
                    skip_next: false,
                    ..state
                },
            )?;
            return Ok(0);
        }
    }
    // One run of a job at a time.
    if store.is_running(job, runner, now())? {
        if scheduled {
            store.record(job, request.trigger, Outcome::Skipped, now())?;
            return Ok(0);
        }
        bail!("{job} is already running");
    }

    // The record names this otto process: it lives exactly as long as the run.
    let run = store.begin(job, request.trigger, std::process::id(), now())?;
    let log = store.log_path(job, &run.id)?;
    match start(request, &log) {
        Ok(status) => {
            store.finish(job, &run, status.code(), now())?;
            Ok(match status.code() {
                Some(0) => 0,
                Some(code) => u8::try_from(code).unwrap_or(1),
                None => 1,
            })
        }
        Err(error) => {
            // Closed here, or the job would count as running until someone looked.
            store.finish(job, &run, None, now())?;
            Err(error)
        }
    }
}

/// Starts the agent and waits for it, with its output kept in `log`.
fn start(request: &Request, log: &Path) -> Result<ExitStatus> {
    let (program, args) = request.argv.split_first().context("empty agent command")?;
    let file = File::create(log).with_context(|| format!("cannot write {}", log.display()))?;
    let mut command = Command::new(program);
    // No terminal is attached on a scheduled run, so the agent gets no stdin.
    command
        .args(args)
        .current_dir(request.workdir)
        .stdin(Stdio::null());
    let cannot_start = || format!("cannot start {program}");

    if request.trigger == Trigger::Scheduled {
        let errors = file
            .try_clone()
            .with_context(|| format!("cannot write {}", log.display()))?;
        return command
            .stdout(file)
            .stderr(errors)
            .status()
            .with_context(cannot_start);
    }

    // A manual run is watched: the output goes to the terminal and to the log.
    drop(file);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(cannot_start)?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    thread::scope(|scope| {
        if let Some(stdout) = stdout {
            scope.spawn(|| tee(stdout, io::stdout(), log));
        }
        if let Some(stderr) = stderr {
            scope.spawn(|| tee(stderr, io::stderr(), log));
        }
    });
    child.wait().with_context(cannot_start)
}

/// Copies a stream to the terminal and appends it to the log. Best effort: a
/// closed terminal or a full disk does not stop the agent.
fn tee(mut from: impl Read, mut terminal: impl Write, log: &Path) {
    let mut file = OpenOptions::new().append(true).open(log).ok();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let _ = terminal.write_all(&buffer[..read]);
        let _ = terminal.flush();
        if let Some(file) = file.as_mut() {
            let _ = file.write_all(&buffer[..read]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::scheduler::runner::Recorder;
    use crate::store::{Outcome, Run, State};

    const NOW: &str = "2026-10-05T19:16:26Z";

    struct World {
        root: TempDir,
        store: Store,
        workdir: PathBuf,
    }

    impl World {
        fn new() -> World {
            let root = tempfile::tempdir().unwrap();
            let workdir = root.path().join("work");
            fs::create_dir(&workdir).unwrap();
            let store = Store::new(root.path().join("jobs"));
            World {
                root,
                store,
                workdir,
            }
        }

        fn run_at(&self, trigger: Trigger, argv: &[String], now: &str) -> Result<u8> {
            let now: Timestamp = now.parse().unwrap();
            execute(
                &Request {
                    job: "report",
                    argv,
                    workdir: &self.workdir,
                    trigger,
                },
                &self.store,
                &Recorder::new(),
                &|| now,
            )
        }

        fn run(&self, trigger: Trigger, argv: &[String]) -> Result<u8> {
            self.run_at(trigger, argv, NOW)
        }

        fn runs(&self) -> Vec<Run> {
            self.store
                .runs("report", &Recorder::new(), NOW.parse().unwrap())
                .unwrap()
        }

        fn last(&self) -> Run {
            self.runs().remove(0)
        }

        fn log(&self, run: &Run) -> String {
            fs::read_to_string(self.store.log_path("report", &run.id).unwrap()).unwrap()
        }

        fn ran(&self) -> bool {
            self.workdir.join("ran").exists()
        }
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
    }

    #[test]
    fn a_scheduled_run_captures_both_streams_and_the_exit_code() {
        let world = World::new();
        let code = world
            .run(
                Trigger::Scheduled,
                &sh("printf out; printf err >&2; exit 3"),
            )
            .unwrap();
        assert_eq!(code, 3);
        let run = world.last();
        assert_eq!(
            (run.outcome, run.exit_code, run.trigger),
            (Outcome::Failed, Some(3), Trigger::Scheduled)
        );
        let log = world.log(&run);
        assert!(log.contains("out") && log.contains("err"));
    }

    #[test]
    fn a_run_happens_in_the_working_directory() {
        let world = World::new();
        world.run(Trigger::Scheduled, &sh("pwd")).unwrap();
        let expected = world.workdir.canonicalize().unwrap();
        assert_eq!(
            world.log(&world.last()).trim_end(),
            expected.to_string_lossy()
        );
    }

    #[test]
    fn a_manual_run_is_captured_too() {
        let world = World::new();
        let code = world.run(Trigger::Manual, &sh("printf hello")).unwrap();
        assert_eq!(code, 0);
        let run = world.last();
        assert_eq!(run.outcome, Outcome::Ok);
        assert_eq!(world.log(&run), "hello");
    }

    #[test]
    fn a_paused_job_is_recorded_and_not_started() {
        let world = World::new();
        let paused = State {
            paused: true,
            skip_next: false,
        };
        world.store.set_state("report", paused).unwrap();

        let code = world.run(Trigger::Scheduled, &sh("touch ran")).unwrap();

        assert_eq!(code, 0);
        assert_eq!(world.last().outcome, Outcome::Paused);
        assert!(!world.ran());
        assert_eq!(world.store.state("report").unwrap(), paused);
    }

    #[test]
    fn skip_next_skips_once_and_clears_itself() {
        let world = World::new();
        let skip = State {
            paused: false,
            skip_next: true,
        };
        world.store.set_state("report", skip).unwrap();

        world.run(Trigger::Scheduled, &sh("touch ran")).unwrap();
        assert_eq!(world.last().outcome, Outcome::Skipped);
        assert!(!world.ran());
        assert_eq!(world.store.state("report").unwrap(), State::default());

        world
            .run_at(Trigger::Scheduled, &sh("touch ran"), "2026-10-06T19:16:26Z")
            .unwrap();
        assert_eq!(world.last().outcome, Outcome::Ok);
        assert!(world.ran());
    }

    #[test]
    fn a_manual_run_ignores_pause_and_leaves_it_in_place() {
        let world = World::new();
        let both = State {
            paused: true,
            skip_next: true,
        };
        world.store.set_state("report", both).unwrap();

        let code = world.run(Trigger::Manual, &sh("touch ran")).unwrap();

        assert_eq!(code, 0);
        assert!(world.ran());
        assert_eq!(world.store.state("report").unwrap(), both);
    }

    #[test]
    fn a_scheduled_run_of_a_running_job_is_skipped() {
        let world = World::new();
        world
            .store
            .begin(
                "report",
                Trigger::Manual,
                4242,
                "2026-10-05T19:00:00Z".parse().unwrap(),
            )
            .unwrap();

        let code = world.run(Trigger::Scheduled, &sh("touch ran")).unwrap();

        assert_eq!(code, 0);
        assert!(!world.ran());
        let outcomes: Vec<Outcome> = world.runs().iter().map(|run| run.outcome).collect();
        assert_eq!(outcomes, [Outcome::Skipped, Outcome::Running]);
    }

    #[test]
    fn a_manual_run_of_a_running_job_is_refused() {
        let world = World::new();
        world
            .store
            .begin(
                "report",
                Trigger::Scheduled,
                4242,
                "2026-10-05T19:00:00Z".parse().unwrap(),
            )
            .unwrap();

        let error = world.run(Trigger::Manual, &sh("touch ran")).unwrap_err();

        assert!(error.to_string().contains("already running"));
        assert!(!world.ran());
        assert_eq!(world.runs().len(), 1);
    }

    #[test]
    fn a_program_that_cannot_start_closes_its_record_as_failed() {
        let world = World::new();
        let error = world
            .run(Trigger::Scheduled, &["otto-no-such-program".to_owned()])
            .unwrap_err();

        assert!(format!("{error:#}").contains("otto-no-such-program"));
        assert_eq!(world.last().outcome, Outcome::Failed);
        assert!(
            !world
                .store
                .is_running("report", &Recorder::new(), NOW.parse().unwrap())
                .unwrap()
        );
        // The root is only kept alive for the directory's lifetime.
        assert!(world.root.path().exists());
    }
}
