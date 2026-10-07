//! One run of a job: decide whether it happens, start the agent, keep its
//! output and close the record.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::notify::{Event, Level, Notifier, message};
use crate::scheduler::runner::Runner;
use crate::store::{Outcome, State, Store, Trigger};

pub struct Request<'a> {
    pub job: &'a str,
    /// Builds the command to start: the program and its arguments. Called only
    /// when the run is going to happen, after its record is open.
    pub command: &'a dyn Fn() -> Result<Vec<String>>,
    pub workdir: &'a Path,
    pub trigger: Trigger,
    /// Which of its runs the job tells the user about.
    pub notify: Level,
    /// The time zone a notification gives the hour in.
    pub zone: &'a TimeZone,
}

/// Runs the request and returns the code otto should exit with.
///
/// Pause and skip-next belong to the schedule: a scheduled run honours them,
/// a manual one neither reads nor changes them.
pub fn execute(
    request: &Request,
    store: &Store,
    runner: &dyn Runner,
    notifier: &dyn Notifier,
    now: &dyn Fn() -> Timestamp,
) -> Result<u8> {
    let job = request.job;
    let scheduled = request.trigger == Trigger::Scheduled;
    if scheduled {
        let state = store.state(job)?;
        if state.paused {
            store.record(job, request.trigger, Outcome::Paused, now())?;
            tell(request, notifier, &Event::Paused, None);
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
            let reason = "skip next";
            tell(request, notifier, &Event::Skipped { reason }, None);
            return Ok(0);
        }
    }
    // One run of a job at a time.
    if store.is_running(job, runner, now())? {
        if scheduled {
            store.record(job, request.trigger, Outcome::Skipped, now())?;
            let reason = "already running";
            tell(request, notifier, &Event::Skipped { reason }, None);
            return Ok(0);
        }
        bail!("{job} is already running");
    }

    // The record names this otto process: it lives exactly as long as the run.
    let run = store.begin(job, request.trigger, std::process::id(), now())?;
    let log = store.log_path(job, &run.id)?;
    // From here on a failure belongs to this run: nobody watches a scheduled
    // run, so the reason has to be where `otto log` finds it.
    // Told once the agent is a process: a run that cannot start is a failure
    // and nothing else.
    let started = || {
        let at = run.started.to_zoned(request.zone.clone());
        let trigger = request.trigger;
        tell(
            request,
            notifier,
            &Event::Started { trigger, at: &at },
            Some(&log),
        );
    };
    // The record is closed before its end is told: whatever becomes of the
    // notification, the history has the run as it ended.
    match (request.command)().and_then(|argv| start(request, &argv, &log, &started)) {
        Ok(status) => {
            let done = store.finish(job, &run, status.code(), now())?;
            let seconds = done.seconds().unwrap_or(0);
            let event = match status.code() {
                Some(0) => Event::Ok { seconds },
                code => Event::Failed {
                    code,
                    seconds,
                    reason: None,
                },
            };
            tell(request, notifier, &event, Some(&log));
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
            let reason = format!("{error:#}");
            let event = Event::Failed {
                code: None,
                seconds: 0,
                reason: Some(&reason),
            };
            tell(request, notifier, &event, Some(&log));
            Err(error)
        }
    }
}

/// Tells the user of `event` when the job tells of it. A notification that
/// cannot be shown never changes the run: why goes to the output of the run,
/// or to otto's own when the run has none.
fn tell(request: &Request, notifier: &dyn Notifier, event: &Event, log: Option<&Path>) {
    if !request.notify.tells(event) {
        return;
    }
    let Err(error) = notifier.notify(&message(request.job, event)) else {
        return;
    };
    let line = format!("otto: cannot notify: {error:#}");
    let kept = log.and_then(|log| OpenOptions::new().create(true).append(true).open(log).ok());
    match kept {
        Some(mut file) => {
            let _ = writeln!(file, "{line}");
        }
        None => eprintln!("{line}"),
    }
}

/// Starts the agent and waits for it, with its output kept in `log`.
/// `started` is called once the agent is a process.
fn start(request: &Request, argv: &[String], log: &Path, started: &dyn Fn()) -> Result<ExitStatus> {
    let (program, args) = argv.split_first().context("empty agent command")?;
    // Checked here because the OS reports a missing directory as if the
    // program were the thing not found.
    if !request.workdir.is_dir() {
        bail!("working directory not found: {}", request.workdir.display());
    }
    // Appending, because the agent is not the only one that writes here: a
    // line of otto's own must not be written over.
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("cannot write {}", log.display()))?;
    let mut command = command_for(program);
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
        let mut child = command
            .stdout(file)
            .stderr(errors)
            .spawn()
            .with_context(cannot_start)?;
        started();
        return child.wait().with_context(cannot_start);
    }

    // A manual run is watched: the output goes to the terminal and to the log.
    drop(file);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(cannot_start)?;
    started();
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

/// The command that starts `program`. Windows only finds a bare name when the
/// file is an `.exe`, and an agent CLI is often a `.cmd`, so the file is
/// looked up here first. A name that is not found is left to the system,
/// which reports it.
#[cfg(windows)]
pub(crate) fn command_for(program: &str) -> Command {
    let found = std::env::var_os("PATH").and_then(|path| crate::which::find(program, &path));
    match found {
        Some(file) => Command::new(file),
        None => Command::new(program),
    }
}

/// Everywhere else the system's own lookup is the one to trust: it runs after
/// the working directory changes and skips a file it may not execute.
#[cfg(not(windows))]
pub(crate) fn command_for(program: &str) -> Command {
    Command::new(program)
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

// These tests start `sh` in the place of the agent.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::notify::{Message, Recording};
    use crate::scheduler::runner::Recorder;
    use crate::store::{Outcome, Run, State};

    const NOW: &str = "2026-10-05T19:16:26Z";

    struct World {
        /// Held so the temporary directory outlives the test.
        _root: TempDir,
        store: Store,
        workdir: PathBuf,
        level: Level,
        notifier: Recording,
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
                level: Level::default(),
                notifier: Recording::default(),
            }
        }

        fn telling(level: Level) -> World {
            World {
                level,
                ..World::new()
            }
        }

        /// What the user was told, as `title: body`.
        fn told(&self) -> Vec<String> {
            let told = self.notifier.told.borrow();
            told.iter()
                .map(|Message { title, body }| format!("{title}: {body}"))
                .collect()
        }

        fn execute_with(
            &self,
            notifier: &dyn Notifier,
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
                    notify: self.level,
                    zone: &TimeZone::UTC,
                },
                &self.store,
                &Recorder::new(),
                notifier,
                &|| now,
            )
        }

        fn run_with(
            &self,
            trigger: Trigger,
            command: &dyn Fn() -> Result<Vec<String>>,
            now: &str,
        ) -> Result<u8> {
            self.execute_with(&self.notifier, trigger, command, now)
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

    #[test]
    fn a_failed_run_tells_by_default() {
        let world = World::new();
        world.run(Trigger::Scheduled, &sh("exit 3")).unwrap();
        assert_eq!(world.told(), ["report failed: exit 3 after 0s"]);
    }

    #[test]
    fn a_run_that_ends_well_is_quiet_by_default() {
        let world = World::new();
        world.run(Trigger::Scheduled, &sh("true")).unwrap();
        assert!(world.told().is_empty());
    }

    #[test]
    fn finish_tells_of_a_good_run_too() {
        let world = World::telling(Level::Finish);
        world.run(Trigger::Scheduled, &sh("true")).unwrap();
        assert_eq!(world.told(), ["report ok: 0s"]);
    }

    #[test]
    fn all_tells_of_the_start_and_then_of_the_end() {
        let world = World::telling(Level::All);
        world.run(Trigger::Scheduled, &sh("true")).unwrap();
        assert_eq!(
            world.told(),
            ["report started: scheduled, 19:16", "report ok: 0s"]
        );
        let world = World::telling(Level::All);
        world.run(Trigger::Manual, &sh("exit 2")).unwrap();
        assert_eq!(
            world.told(),
            [
                "report started: manual, 19:16",
                "report failed: exit 2 after 0s"
            ]
        );
    }

    #[test]
    fn all_tells_of_a_run_that_did_not_happen() {
        let world = World::telling(Level::All);
        let paused = State {
            paused: true,
            skip_next: false,
        };
        world.store.set_state("report", paused).unwrap();
        world.run(Trigger::Scheduled, &sh("true")).unwrap();
        assert_eq!(world.told(), ["report paused: the job is paused"]);

        let world = World::telling(Level::All);
        let skip = State {
            paused: false,
            skip_next: true,
        };
        world.store.set_state("report", skip).unwrap();
        world.run(Trigger::Scheduled, &sh("true")).unwrap();
        assert_eq!(world.told(), ["report skipped: skip next"]);

        let world = World::telling(Level::All);
        let earlier = "2026-10-05T19:00:00Z".parse().unwrap();
        world
            .store
            .begin("report", Trigger::Manual, 4242, earlier)
            .unwrap();
        world.run(Trigger::Scheduled, &sh("true")).unwrap();
        assert_eq!(world.told(), ["report skipped: already running"]);
    }

    #[test]
    fn a_run_that_does_not_happen_is_quiet_by_default() {
        let world = World::new();
        let paused = State {
            paused: true,
            skip_next: false,
        };
        world.store.set_state("report", paused).unwrap();
        world.run(Trigger::Scheduled, &sh("true")).unwrap();
        assert!(world.told().is_empty());
    }

    #[test]
    fn off_never_tells() {
        let world = World::telling(Level::Off);
        world.run(Trigger::Scheduled, &sh("exit 3")).unwrap();
        assert!(world.told().is_empty());
    }

    #[test]
    fn a_run_that_could_not_start_tells_why_and_has_no_start() {
        let world = World::telling(Level::All);
        fs::remove_dir(&world.workdir).unwrap();
        world.run(Trigger::Scheduled, &sh("true")).unwrap_err();
        let told = world.told();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            told[0].starts_with("report failed: working directory not found"),
            "{told:?}"
        );

        let world = World::telling(Level::All);
        world
            .run(Trigger::Manual, &["otto-no-such-program".to_owned()])
            .unwrap_err();
        let told = world.told();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            told[0].starts_with("report failed: cannot start otto-no-such-program"),
            "{told:?}"
        );
    }

    #[test]
    fn a_notifier_that_fails_changes_nothing_of_the_run() {
        let world = World {
            notifier: Recording {
                broken: Some("no notification service".to_owned()),
                ..Recording::default()
            },
            ..World::telling(Level::All)
        };
        let code = world
            .run(Trigger::Scheduled, &sh("echo hi; exit 3"))
            .unwrap();
        assert_eq!(code, 3);
        let run = world.last();
        assert_eq!((run.outcome, run.exit_code), (Outcome::Failed, Some(3)));
        let log = world.log(&run);
        assert!(log.contains("hi"), "{log}");
        assert_eq!(
            log.matches("otto: cannot notify: no notification service")
                .count(),
            2,
            "{log}"
        );
    }

    /// Looks at the history each time it is asked to tell something.
    struct Looking<'a> {
        store: &'a Store,
        seen: std::cell::RefCell<Vec<(String, Outcome)>>,
    }

    impl Notifier for Looking<'_> {
        fn notify(&self, message: &Message) -> Result<()> {
            let runs = self
                .store
                .runs("report", &Recorder::new(), NOW.parse().unwrap())?;
            self.seen
                .borrow_mut()
                .push((message.title.clone(), runs[0].outcome));
            Ok(())
        }
    }

    #[test]
    fn the_record_is_closed_before_the_end_is_told() {
        let world = World::telling(Level::All);
        let looking = Looking {
            store: &world.store,
            seen: std::cell::RefCell::new(Vec::new()),
        };
        world
            .execute_with(&looking, Trigger::Scheduled, &|| Ok(sh("exit 3")), NOW)
            .unwrap();
        assert_eq!(
            *looking.seen.borrow(),
            [
                ("report started".to_owned(), Outcome::Running),
                ("report failed".to_owned(), Outcome::Failed)
            ]
        );
    }

    #[test]
    fn a_manual_run_refused_tells_nothing() {
        let world = World::telling(Level::All);
        let earlier = "2026-10-05T19:00:00Z".parse().unwrap();
        world
            .store
            .begin("report", Trigger::Scheduled, 4242, earlier)
            .unwrap();
        world.run(Trigger::Manual, &sh("true")).unwrap_err();
        assert!(world.told().is_empty());
    }
}
