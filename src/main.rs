mod agent;
mod atomic;
mod config;
mod jobs_file;
mod next;
mod notify;
mod process;
mod run;
mod scheduler;
mod store;
mod sync;
mod tui;
mod which;

use std::env;
use std::fs;
use std::io::{self, IsTerminal as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory as _, Parser, Subcommand};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use config::Config;
use next::Next;
use run::Request;
use scheduler::runner::System;
use store::{Run, State, Store, Trigger};
use tui::world::{Real, Setup};

#[derive(Parser)]
#[command(
    name = "otto",
    version,
    about = "Scheduled runs for coding agents, on the scheduler your OS already has."
)]
struct Cli {
    /// Jobs file. Defaults to ~/.config/otto/jobs.toml.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// With none, and on a terminal, otto opens its terminal UI.
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the configured jobs.
    List,
    /// Make the OS scheduler match the jobs file: add, update and remove units.
    Sync {
        /// Print what would change without touching the scheduler.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run a job now, in the foreground.
    Run {
        job: String,
        /// Print the agent command instead of running it.
        #[arg(long)]
        dry_run: bool,
        /// Set by the scheduler unit: honours pause and skip, and keeps the output off the terminal.
        #[arg(long, conflicts_with = "dry_run")]
        scheduled: bool,
    },
    /// Skip the next scheduled run of a job.
    Skip { job: String },
    /// Stop the scheduled runs of a job until it is resumed.
    Pause { job: String },
    /// Clear pause and skip: the job runs on schedule again.
    Resume { job: String },
    /// List the runs of a job, newest first.
    Runs { job: String },
    /// Print the output of a run: the latest one, or the one with this id.
    Log { job: String, run: Option<String> },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("otto: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// What a scheduler unit is built from: this binary, the jobs file and the
/// `PATH` of the terminal otto was called from.
fn context(config_path: &Path) -> Result<scheduler::Context> {
    let path = env::var("PATH").unwrap_or_default();
    if path.is_empty() {
        bail!("PATH is empty; run otto from your terminal");
    }
    Ok(scheduler::Context {
        otto: env::current_exe().context("cannot locate the otto binary")?,
        config: std::path::absolute(config_path)
            .with_context(|| format!("cannot resolve {}", config_path.display()))?,
        path,
        log_dir: config::log_dir()?,
    })
}

/// What otto remembers about each job, under the state directory.
fn store() -> Result<Store> {
    let store = Store::new(config::state_dir()?.join("jobs"));
    // Linux lists processes under /proc and Windows has `tasklist`; elsewhere
    // the store asks `kill`.
    let procfs = Path::new("/proc");
    Ok(if cfg!(windows) {
        store.with_tasklist()
    } else if procfs.join("self").exists() {
        store.with_procfs(procfs.to_path_buf())
    } else {
        store
    })
}

fn run(cli: Cli) -> Result<ExitCode> {
    let path = match cli.config {
        Some(path) => path,
        None => config::default_path()?,
    };
    // The UI reads the jobs file itself, and opens without one.
    let Some(command) = cli.command else {
        return screen(&path);
    };
    let config = Config::load(&path)?;

    match command {
        Cmd::List => {
            let store = store()?;
            let now = Timestamp::now();
            let zone = TimeZone::system();
            for (name, job) in &config.jobs {
                let state = store.state(name)?;
                let next = next::next(&job.schedule, state, now, &zone).ok();
                let last = store
                    .runs(name, &System, now)?
                    .first()
                    .map_or("never", |run| run.outcome.label());
                let days: Vec<&str> = job
                    .schedule
                    .days
                    .iter()
                    .map(|day| day.systemd_name())
                    .collect();
                println!(
                    "{name}\t{:?}\t{} {}\t{}\t{}\t{last}\t{}",
                    job.agent,
                    job.schedule.at,
                    days.join(","),
                    job.workdir.display(),
                    state.label(),
                    next_column(next.as_ref())
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Sync { dry_run } => {
            let ctx = context(&path)?;
            let scheduler = scheduler::native()?;
            let store = store()?;
            let is_running = |job: &str| store.is_running(job, &System, Timestamp::now());
            let outcomes = sync::sync(
                &config,
                scheduler.as_ref(),
                &ctx,
                &System,
                &is_running,
                dry_run,
            );
            let mut failed = false;
            for outcome in outcomes {
                match outcome.result {
                    Ok(action) => println!("{}\t{}", outcome.job, action.label()),
                    Err(error) => {
                        failed = true;
                        println!("{}\terror: {error:#}", outcome.job);
                    }
                }
            }
            Ok(if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Cmd::Run {
            job: name,
            dry_run,
            scheduled,
        } => {
            let job = config.job(&name)?;
            let read_prompt = || {
                fs::read_to_string(&job.prompt)
                    .with_context(|| format!("cannot read prompt {}", job.prompt.display()))
            };
            if dry_run {
                read_prompt()?;
                let shown = format!("<{}>", job.prompt.display());
                println!(
                    "cd {} && {}",
                    job.workdir.display(),
                    job.agent.command(&shown, &job.args).join(" ")
                );
                return Ok(ExitCode::SUCCESS);
            }
            // The prompt is read once the run has a record: a prompt file that
            // went missing must show up in the history, not only on stderr.
            let command = || Ok(job.agent.command(&read_prompt()?, &job.args));
            let zone = TimeZone::system();
            let request = Request {
                job: &name,
                command: &command,
                workdir: &job.workdir,
                trigger: if scheduled {
                    Trigger::Scheduled
                } else {
                    Trigger::Manual
                },
                notify: job.notify,
                zone: &zone,
            };
            let patient = notify::Timed {
                limit: notify::PATIENCE,
            };
            let notifier = notify::native(&patient);
            let code = run::execute(
                &request,
                &store()?,
                &System,
                notifier.as_ref(),
                &Timestamp::now,
            )?;
            Ok(ExitCode::from(code))
        }
        Cmd::Skip { job: name } => {
            config.job(&name)?;
            let store = store()?;
            let state = State {
                skip_next: true,
                ..store.state(&name)?
            };
            set_state(&store, &name, state)
        }
        Cmd::Pause { job: name } => {
            config.job(&name)?;
            let store = store()?;
            let state = State {
                paused: true,
                ..store.state(&name)?
            };
            set_state(&store, &name, state)
        }
        Cmd::Resume { job: name } => {
            config.job(&name)?;
            set_state(&store()?, &name, State::default())
        }
        Cmd::Runs { job: name } => {
            config.job(&name)?;
            let runs = store()?.runs(&name, &System, Timestamp::now())?;
            if runs.is_empty() {
                eprintln!("no runs yet");
            }
            let zone = TimeZone::system();
            for run in &runs {
                println!("{}", run_line(run, &zone));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Log { job: name, run } => {
            config.job(&name)?;
            let store = store()?;
            let path = match run {
                Some(id) => {
                    let path = store.log_path(&name, &id)?;
                    if !path.is_file() {
                        bail!("no output for run {id} of {name}");
                    }
                    path
                }
                // A skipped or paused run has no output: pass over it.
                None => store
                    .runs(&name, &System, Timestamp::now())?
                    .iter()
                    .filter_map(|run| store.log_path(&name, &run.id).ok())
                    .find(|path| path.is_file())
                    .with_context(|| format!("no output recorded for {name}"))?,
            };
            // Byte for byte: an agent may print what is not valid UTF-8.
            let output =
                fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
            io::stdout()
                .write_all(&output)
                .context("cannot write to the terminal")?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// `otto` with no subcommand opens the UI only when both ends are a terminal.
fn opens_screen(stdin_is_terminal: bool, stdout_is_terminal: bool) -> bool {
    stdin_is_terminal && stdout_is_terminal
}

/// The terminal UI, or the help where there is no terminal to draw on.
fn screen(config_path: &Path) -> Result<ExitCode> {
    if !opens_screen(io::stdin().is_terminal(), io::stdout().is_terminal()) {
        eprint!("{}", Cli::command().render_help());
        return Ok(ExitCode::from(2));
    }
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| env::var(name).ok())
        .find(|value| !value.trim().is_empty());
    let zone = TimeZone::system();
    let mut world = Real::new(Setup {
        config: std::path::absolute(config_path)
            .with_context(|| format!("cannot resolve {}", config_path.display()))?,
        store: store()?,
        runner: Box::new(System),
        otto: env::current_exe().context("cannot locate the otto binary")?,
        editor,
        zone: zone.clone(),
        path: env::var_os("PATH"),
        // Where otto has no scheduler, or the `PATH` is empty, the screen
        // still opens: it only has nothing to sync with.
        sync: scheduler::native().ok().zip(context(config_path).ok()),
    });
    tui::run(&mut world, &Timestamp::now, &zone)?;
    Ok(ExitCode::SUCCESS)
}

fn set_state(store: &Store, job: &str, state: State) -> Result<ExitCode> {
    store.set_state(job, state)?;
    println!("{job}\t{}", state.label());
    Ok(ExitCode::SUCCESS)
}

/// The last column of `otto list`: the local time of the run that will start
/// the agent, or `-` when no run is coming.
fn next_column(next: Option<&Next>) -> String {
    next.and_then(Next::runs_at).map_or_else(
        || "-".to_owned(),
        |when| when.strftime("%Y-%m-%d %H:%M").to_string(),
    )
}

/// One run as a line of `otto runs`: id, local start, trigger, outcome, duration.
fn run_line(run: &Run, zone: &TimeZone) -> String {
    let started = run
        .started
        .to_zoned(zone.clone())
        .strftime("%Y-%m-%d %H:%M");
    format!(
        "{}\t{started}\t{}\t{}\t{}",
        run.id,
        run.trigger.label(),
        run.outcome_label(),
        run.duration_label().unwrap_or_else(|| "-".to_owned())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Outcome;

    fn a_run(outcome: Outcome, trigger: Trigger, finished: Option<&str>) -> Run {
        Run {
            id: "20261005T191626Z".to_owned(),
            started: "2026-10-05T19:16:26Z".parse().unwrap(),
            finished: finished.map(|text| text.parse().unwrap()),
            trigger,
            outcome,
            exit_code: None,
            pid: None,
        }
    }

    #[test]
    fn a_run_line_shows_when_how_and_for_how_long() {
        let run = Run {
            exit_code: Some(3),
            ..a_run(
                Outcome::Failed,
                Trigger::Manual,
                Some("2026-10-05T19:18:40Z"),
            )
        };
        assert_eq!(
            run_line(&run, &TimeZone::UTC),
            "20261005T191626Z\t2026-10-05 19:16\tmanual\tfailed (3)\t2m 14s"
        );
    }

    #[test]
    fn a_run_line_has_no_duration_for_a_run_that_did_not_start() {
        let run = a_run(
            Outcome::Skipped,
            Trigger::Scheduled,
            Some("2026-10-05T19:16:26Z"),
        );
        assert!(run_line(&run, &TimeZone::UTC).ends_with("scheduled\tskipped\t-"));
    }

    #[test]
    fn the_screen_opens_only_when_both_ends_are_a_terminal() {
        assert!(opens_screen(true, true));
        assert!(!opens_screen(false, true));
        assert!(!opens_screen(true, false));
    }

    #[test]
    fn the_next_column_is_the_run_that_will_start_the_agent() {
        let zone = TimeZone::get("America/Sao_Paulo").unwrap();
        let at = |text: &str| {
            text.parse::<jiff::civil::DateTime>()
                .unwrap()
                .to_zoned(zone.clone())
                .unwrap()
        };
        let today = at("2026-10-06T16:05");
        let tomorrow = at("2026-10-07T16:05");
        assert_eq!(
            next_column(Some(&Next::At(today.clone()))),
            "2026-10-06 16:05"
        );
        assert_eq!(
            next_column(Some(&Next::Skipping {
                skipped: today,
                then: tomorrow
            })),
            "2026-10-07 16:05"
        );
        assert_eq!(next_column(Some(&Next::Paused)), "-");
        assert_eq!(next_column(None), "-");
    }

    #[test]
    fn a_run_line_has_no_duration_while_running() {
        let run = a_run(Outcome::Running, Trigger::Scheduled, None);
        assert!(run_line(&run, &TimeZone::UTC).ends_with("running\t-"));
    }
}
