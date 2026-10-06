//! How a scheduler backend runs `launchctl` or `systemctl`, behind a trait so
//! tests can record the commands instead of touching the real scheduler.

use std::process::Command;

use anyhow::{Context, Result};

#[derive(Debug)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait Runner {
    /// Runs `program` with `args` and waits for it. `Err` only when the process
    /// could not be started; a failing exit status is `success: false`.
    fn run(&self, program: &str, args: &[&str]) -> Result<Output>;
}

/// Runs the command for real.
pub struct System;

impl Runner for System {
    fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
        let output = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("cannot start {program}"))?;
        Ok(Output {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Records every command and answers from a script, without running anything.
#[cfg(test)]
pub struct Recorder {
    calls: std::cell::RefCell<Vec<String>>,
    answers: std::cell::RefCell<Vec<(String, Vec<bool>)>>,
}

#[cfg(test)]
impl Recorder {
    pub fn new() -> Recorder {
        Recorder {
            calls: std::cell::RefCell::new(Vec::new()),
            answers: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// Commands whose line starts with `prefix` answer with `results` in order
    /// (true = success); the last one repeats.
    pub fn answering(self, prefix: &str, results: &[bool]) -> Recorder {
        self.answers
            .borrow_mut()
            .push((prefix.to_owned(), results.to_vec()));
        self
    }

    /// Each call as one line: the program and its arguments, space separated.
    pub fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

#[cfg(test)]
impl Runner for Recorder {
    fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
        let line = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        let success = self
            .answers
            .borrow_mut()
            .iter_mut()
            .find(|(prefix, _)| line.starts_with(prefix.as_str()))
            .map(|(_, results)| {
                if results.len() > 1 {
                    results.remove(0)
                } else {
                    results.first().copied().unwrap_or(true)
                }
            })
            .unwrap_or(true);
        let stdout = if line == "id -u" { "501\n" } else { "" };
        self.calls.borrow_mut().push(line);
        Ok(Output {
            success,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_reports_exit_status_and_output() {
        let out = System
            .run("sh", &["-c", "printf hi; printf oops >&2; exit 3"])
            .unwrap();
        assert!(!out.success);
        assert_eq!(out.stdout, "hi");
        assert_eq!(out.stderr, "oops");
    }

    #[test]
    fn system_names_a_program_it_cannot_start() {
        let error = System.run("otto-no-such-program", &[]).unwrap_err();
        assert!(format!("{error:#}").contains("otto-no-such-program"));
    }

    #[test]
    fn recorder_replays_programmed_answers_and_repeats_the_last() {
        let runner = Recorder::new().answering("launchctl print", &[true, false]);
        assert!(
            runner
                .run("launchctl", &["print", "gui/501/x"])
                .unwrap()
                .success
        );
        assert!(
            !runner
                .run("launchctl", &["print", "gui/501/x"])
                .unwrap()
                .success
        );
        assert!(
            !runner
                .run("launchctl", &["print", "gui/501/x"])
                .unwrap()
                .success
        );
        assert_eq!(runner.run("id", &["-u"]).unwrap().stdout, "501\n");
        assert_eq!(runner.calls().len(), 4);
        assert_eq!(runner.calls()[0], "launchctl print gui/501/x");
    }
}
