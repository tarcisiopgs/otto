mod agent;
mod config;
mod scheduler;
mod store;
mod sync;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use config::Config;
use scheduler::runner::System;

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

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the configured jobs.
    List,
    /// Print what the OS scheduler would be given for a job, without installing it.
    Plan { job: String },
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
    },
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

fn run(cli: Cli) -> Result<ExitCode> {
    let path = match cli.config {
        Some(path) => path,
        None => config::default_path()?,
    };
    let config = Config::load(&path)?;

    match cli.command {
        Cmd::List => {
            for (name, job) in &config.jobs {
                let days: Vec<&str> = job
                    .schedule
                    .days
                    .iter()
                    .map(|day| day.systemd_name())
                    .collect();
                println!(
                    "{name}\t{:?}\t{} {}\t{}",
                    job.agent,
                    job.schedule.at,
                    days.join(","),
                    job.workdir.display()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Plan { job: name } => {
            let job = config.job(&name)?;
            let ctx = context(&path)?;
            let scheduler = scheduler::native()?;
            for unit in scheduler.units(&name, job, &ctx)? {
                println!(
                    "# {}: {}\n{}",
                    scheduler.name(),
                    unit.path.display(),
                    unit.contents
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Sync { dry_run } => {
            let ctx = context(&path)?;
            let scheduler = scheduler::native()?;
            let outcomes = sync::sync(&config, scheduler.as_ref(), &ctx, &System, dry_run);
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
        Cmd::Run { job: name, dry_run } => {
            let job = config.job(&name)?;
            let prompt = fs::read_to_string(&job.prompt)
                .with_context(|| format!("cannot read prompt {}", job.prompt.display()))?;
            if dry_run {
                let shown = format!("<{}>", job.prompt.display());
                println!(
                    "cd {} && {}",
                    job.workdir.display(),
                    job.agent.command(&shown, &job.args).join(" ")
                );
                return Ok(ExitCode::SUCCESS);
            }
            let argv = job.agent.command(&prompt, &job.args);
            let (program, args) = argv.split_first().context("empty agent command")?;
            // No terminal is attached on a scheduled run, so the agent gets no stdin.
            let status = Command::new(program)
                .args(args)
                .current_dir(&job.workdir)
                .stdin(Stdio::null())
                .status()
                .with_context(|| format!("cannot start {program}"))?;
            Ok(match status.code() {
                Some(0) => ExitCode::SUCCESS,
                Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
                None => ExitCode::FAILURE,
            })
        }
    }
}
