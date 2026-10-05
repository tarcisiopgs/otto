mod agent;
mod config;
mod scheduler;

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use config::Config;

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
            let otto = env::current_exe().context("cannot locate the otto binary")?;
            let scheduler = scheduler::native()?;
            for unit in scheduler.units(&name, job, &otto)? {
                println!(
                    "# {}: {}\n{}",
                    scheduler.name(),
                    unit.path.display(),
                    unit.contents
                );
            }
            Ok(ExitCode::SUCCESS)
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
