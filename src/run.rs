//! One run of a job: decide whether it happens, start the agent, keep its
//! output and close the record.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;

use crate::scheduler::runner::Runner;
use crate::store::{Outcome, State, Store, Trigger};

pub struct Request<'a> {
    pub job: &'a str,
    /// Builds the command to start: the program and its arguments. Called only
    /// when the run is going to happen, after its record is open.
    pub command: &'a dyn Fn() -> Result<Vec<String>>,
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
    // From here on a failure belongs to this run: nobody watches a scheduled
    // run, so the reason has to be where `otto log` finds it.
    match (request.command)().and_then(|argv| start(request, &argv, &log)) {
        Ok(status) => {
            store.finish(job, &run, status.code(), now())?;
            Ok(match status.code() {
                Some(0) => 0,
                Some(code) => u8::try_from(code).unwrap_or(1),
                None => 1,
            })
        }
        Err(error) => {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log) {
                let _ = writeln!(file, "otto: {error:#}");
            }
            // Closed here, or the job would count as running until someone looked.
            store.finish(job, &run, None, now())?;
            Err(error)
        }
    }
}

/// Starts the agent and waits for it, with its output kept in `log`.
fn start(request: &Request, argv: &[String], log: &Path) -> Result<ExitStatus> {
    let (program, args) = argv.split_first().context("empty agent command")?;
    // Checked here because the OS reports a missing directory as if the
    // program were the thing not found.
    if !request.workdir.is_dir() {
        bail!("working directory not found: {}", request.workdir.display());
    }
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
    let (done, drained) = mpsc::channel();
    let mut copies = 0;
    if let Some(stdout) = child.stdout.take() {
        let (done, log) = (done.clone(), log.to_path_buf());
        thread::spawn(move || {
            tee(stdout, io::stdout(), &log);
            let _ = done.send(());
        });
        copies += 1;
    }
    if let Some(stderr) = child.stderr.take() {
        let (done, log) = (done.clone(), log.to_path_buf());
        thread::spawn(move || {
            tee(stderr, io::stderr(), &log);
            let _ = done.send(());
        });
        copies += 1;
    }
    let status = child.wait().with_context(cannot_start)?;
    // The agent is done. A process it left behind (a dev server, an MCP server)
    // can hold the pipe open for as long as it lives, so the copies get a
    // moment to drain what is already written and are then left behind.
    let deadline = Instant::now() + DRAIN;
    for _ in 0..copies {
        let left = deadline.saturating_duration_since(Instant::now());
        if drained.recv_timeout(left).is_err() {
            break;
        }
    }
    Ok(status)
}

/// How long a manual run waits for its output to drain after the agent exits.
const DRAIN: Duration = Duration::from_millis(500);

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
        /// Held so the temporary directory outlives the test.
        _root: TempDir,
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
                _root: root,
                store,
                workdir,
            }
        }

        fn run_with(
            &self,
            trigger: Trigger,
            command: &dyn Fn() -> Result<Vec<String>>,
            now: &str,
        ) -> Result<u8> {
            let now: Timestamp = now.parse().unwrap();
            execute(
                &Request {
                    job: "report",
                    command,
                    workdir: &self.workdir,
                    trigger,
                },
                &self.store,
                &Recorder::new(),
                &|| now,
            )
        }

        fn run_at(&self, trigger: Trigger, argv: &[String], now: &str) -> Result<u8> {
            self.run_with(trigger, &|| Ok(argv.to_vec()), now)
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
        assert!(
            world
                .log(&world.last())
                .contains("cannot start otto-no-such-program")
        );
    }

    #[test]
    fn a_missing_working_directory_is_named_in_the_record() {
        let world = World::new();
        fs::remove_dir(&world.workdir).unwrap();

        let error = world
            .run(Trigger::Scheduled, &sh("printf hello"))
            .unwrap_err();

        assert!(error.to_string().contains("working directory not found"));
        let run = world.last();
        assert_eq!(run.outcome, Outcome::Failed);
        assert!(world.log(&run).contains("working directory not found"));
    }

    #[test]
    fn a_command_that_cannot_be_built_still_leaves_a_record() {
        let world = World::new();

        let error = world
            .run_with(
                Trigger::Scheduled,
                &|| bail!("cannot read prompt /gone/prompt.md"),
                NOW,
            )
            .unwrap_err();

        assert!(error.to_string().contains("cannot read prompt"));
        let run = world.last();
        assert_eq!(run.outcome, Outcome::Failed);
        assert!(
            world
                .log(&run)
                .contains("cannot read prompt /gone/prompt.md")
        );
    }

    #[test]
    fn a_manual_run_does_not_wait_for_what_the_agent_left_behind() {
        let world = World::new();
        let started = std::time::Instant::now();

        // The background sleep inherits the output pipe and holds it open.
        let code = world
            .run(Trigger::Manual, &sh("sleep 5 & printf done"))
            .unwrap();

        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        assert_eq!(code, 0);
        let run = world.last();
        assert_eq!(run.outcome, Outcome::Ok);
        assert!(world.log(&run).contains("done"));
    }
}
