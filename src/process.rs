//! Starting a run that outlives the terminal UI, and stopping one. The record
//! of a run names the `otto run` process, not the agent it started, so a run is
//! stopped as a process group.

use std::process::{Child, Command, Stdio};

use anyhow::{Context as _, Result};

use crate::scheduler::runner::{Runner, must};

/// The processes this otto started and has not collected yet.
#[derive(Default)]
pub struct Children {
    running: Vec<Child>,
}

impl Children {
    pub fn new() -> Children {
        Children::default()
    }

    /// Starts `command` with no terminal, in a process group of its own, and
    /// does not wait for it. Returns its process id.
    pub fn start(&mut self, mut command: Command) -> Result<u32> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut command);
        let child = command
            .spawn()
            .with_context(|| format!("cannot start {}", command.get_program().to_string_lossy()))?;
        let pid = child.id();
        self.running.push(child);
        Ok(pid)
    }

    /// Collects the children that have ended. One left uncollected stays in
    /// the process table, where it still answers as a live process.
    pub fn reap(&mut self) {
        self.running
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
    }
}

/// A group of its own keeps the terminal's Ctrl-C and hang-up away from the
/// run, and makes it something `stop_group` can end as a whole.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Ends the process group `pid` leads: the run of `job` and everything it
/// started.
///
/// Two things are checked first. The process must be `otto … run <job>`: a
/// record left open by a crash can name a process id that the system has since
/// given to something else. And it must lead its group: ending it alone would
/// leave the agent running with nobody recording it.
#[cfg(not(windows))]
pub fn stop_group(pid: u32, job: &str, runner: &dyn Runner) -> Result<()> {
    let pid = pid.to_string();
    let found = runner.run("ps", &["-o", "pgid=,args=", "-p", &pid])?;
    let mut words = found.stdout.split_whitespace();
    let Some(group) = words.next().filter(|_| found.success) else {
        anyhow::bail!("process {pid} is not running");
    };
    let command: Vec<&str> = words.collect();
    if !command.windows(2).any(|pair| pair == ["run", job]) {
        anyhow::bail!("process {pid} is not a run of {job}");
    }
    if group != pid {
        anyhow::bail!("process {pid} does not lead its process group");
    }
    must(runner, "kill", &["-s", "TERM", "--", &format!("-{pid}")])
}

/// Windows has no signal to ask a windowless process to end: the tree is
/// ended outright.
#[cfg(windows)]
pub fn stop_group(pid: u32, _job: &str, runner: &dyn Runner) -> Result<()> {
    must(runner, "taskkill", &["/T", "/F", "/PID", &pid.to_string()])
}

// These tests start real processes with `sh` and ask `ps` and `kill` about them.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::fs;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::scheduler::runner::{Recorder, System};

    #[test]
    fn a_leader_gets_the_signal_as_a_group() {
        let runner = Recorder::new().printing(
            "ps -o pgid=,args= -p 77",
            "   77 /usr/local/bin/otto --config /c/jobs.toml run report --scheduled\n",
        );
        stop_group(77, "report", &runner).unwrap();
        assert_eq!(
            runner.calls(),
            ["ps -o pgid=,args= -p 77", "kill -s TERM -- -77"]
        );
    }

    #[test]
    fn a_process_that_does_not_lead_its_group_is_refused() {
        let runner = Recorder::new().printing("ps -o pgid=,args= -p 77", " 4242 otto run report\n");
        let error = stop_group(77, "report", &runner).unwrap_err();
        assert!(format!("{error:#}").contains("does not lead its process group"));
        assert!(!runner.calls().iter().any(|call| call.starts_with("kill")));
    }

    /// A record left open by a crash can name a process id that now belongs
    /// to something else.
    #[test]
    fn a_process_that_is_not_that_run_is_refused() {
        for line in [
            "   77 /Applications/Safari.app/Contents/MacOS/Safari\n",
            "   77 otto --config /c/jobs.toml run reports\n",
            "   77 otto --config /c/jobs.toml list\n",
        ] {
            let runner = Recorder::new().printing("ps -o pgid=,args= -p 77", line);
            let error = stop_group(77, "report", &runner).unwrap_err();
            assert!(
                format!("{error:#}").contains("is not a run of report"),
                "{line}"
            );
            assert!(!runner.calls().iter().any(|call| call.starts_with("kill")));
        }
    }

    #[test]
    fn a_process_that_is_gone_is_reported() {
        let runner = Recorder::new().answering("ps", &[false]);
        let error = stop_group(77, "report", &runner).unwrap_err();
        assert!(format!("{error:#}").contains("is not running"));
    }

    fn sh(script: &str, argument: &str) -> Command {
        let mut command = Command::new("sh");
        // The trailing words make it read as a run of `report` to `stop_group`.
        command.args(["-c", script, argument, "run", "report"]);
        command
    }

    fn exists(pid: &str) -> bool {
        System.run("kill", &["-0", pid]).unwrap().success
    }

    /// Waits up to five seconds for `done`, collecting ended children meanwhile.
    fn eventually(children: &mut Children, done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            children.reap();
            if done() {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_started_child_leads_its_own_group() {
        let mut children = Children::new();
        let pid = children
            .start(sh("sleep 30 & wait", ""))
            .unwrap()
            .to_string();
        let group = System.run("ps", &["-o", "pgid=", "-p", &pid]).unwrap();
        assert_eq!(group.stdout.trim(), pid);
        stop_group(pid.parse().unwrap(), "report", &System).unwrap();
        assert!(eventually(&mut children, || !exists(&pid)));
    }

    #[test]
    fn stopping_a_group_ends_the_child_and_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("pid");
        let mut children = Children::new();
        let pid = children
            .start(sh(
                "sleep 30 & echo $! > \"$0\"; wait",
                note.to_str().unwrap(),
            ))
            .unwrap();
        let read = || {
            fs::read_to_string(&note)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        assert!(eventually(&mut children, || !read().is_empty()));
        let grandchild = read();
        assert!(exists(&grandchild));

        stop_group(pid, "report", &System).unwrap();

        let pid = pid.to_string();
        assert!(eventually(&mut children, || !exists(&pid)));
        assert!(eventually(&mut children, || !exists(&grandchild)));
    }

    #[test]
    fn an_exited_child_does_not_linger() {
        let mut children = Children::new();
        let pid = children.start(sh("exit 0", "")).unwrap().to_string();
        // Without `reap` it would stay as a zombie, which `kill -0` still finds.
        assert!(eventually(&mut children, || !exists(&pid)));
    }
}
