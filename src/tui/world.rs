//! The boundary between the terminal UI and the machine. Everything the UI
//! reads from disk or asks of a process goes through [`World`], so the state
//! and the drawing can be tested without either.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{ErrorKind, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::config::{self, Config, Job};
use crate::jobs_file::{self, JobSpec};
use crate::next::{self, Next};
use crate::process::{self, Children};
use crate::scheduler::runner::Runner;
use crate::scheduler::{Context, Scheduler};
use crate::store::{Outcome, Run, State, Store};
use crate::sync::{self, Action};
use crate::which;

/// How much of the end of a run's output the UI loads.
pub const LOG_TAIL: u64 = 1_048_576;

/// One job as the UI shows it.
#[derive(Debug, Clone)]
pub struct JobView {
    pub name: String,
    pub job: Job,
    pub state: State,
    /// Newest first.
    pub runs: Vec<Run>,
    pub next: Option<Next>,
}

impl JobView {
    /// The run in progress, when there is one.
    pub fn running(&self) -> Option<&Run> {
        self.runs.iter().find(|run| run.outcome == Outcome::Running)
    }

    /// A run in progress wins over what the user asked of the schedule.
    pub fn status(&self) -> &'static str {
        if self.running().is_some() {
            Outcome::Running.label()
        } else {
            self.state.label()
        }
    }
}

/// Everything the UI shows, as it was at one moment.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The jobs file.
    pub config: PathBuf,
    pub jobs: Vec<JobView>,
    /// Why the jobs file, or what otto remembers of a job, could not be read.
    pub error: Option<String>,
    /// What a sync would do: every job whose unit is not what the jobs file
    /// says, and every unit whose job is gone.
    pub pending: Vec<Pending>,
    /// Whether there is a scheduler to sync with on this system.
    pub can_sync: bool,
}

impl Snapshot {
    /// What a sync would do with `job`, when it would do anything.
    pub fn pending_for(&self, job: &str) -> Option<&Pending> {
        self.pending.iter().find(|pending| pending.job == job)
    }
}

/// What a sync would do, or did, with one job: the action, or why it could
/// not be done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub job: String,
    pub change: Result<Action, String>,
}

/// The end of a run's output, ready for the screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Log {
    pub text: String,
    /// The output is longer than [`LOG_TAIL`] and its beginning was left out.
    pub truncated: bool,
}

pub trait World {
    fn snapshot(&mut self, now: Timestamp) -> Snapshot;
    fn log(&self, job: &str, run: &str) -> Result<Log>;
    fn set_state(&self, job: &str, state: State) -> Result<()>;
    /// Starts a manual run of `job` that does not depend on this process.
    fn start(&mut self, job: &str) -> Result<()>;
    /// Stops the run of `job` owned by process `pid`.
    fn stop(&self, job: &str, pid: u32) -> Result<()>;
    /// Makes the scheduler match the jobs file, and says what was done with
    /// each job. This is the one thing that touches the OS scheduler.
    fn apply(&mut self, now: Timestamp) -> Vec<Pending>;
    /// Opens `path` in the user's editor and waits for it to close.
    fn edit(&self, path: &Path) -> Result<()>;
    /// The jobs file as it is on disk; empty when there is none yet.
    fn text(&self) -> Result<String>;
    /// Writes `text` over the jobs file, which must still read as `read`.
    fn save(&mut self, read: &str, text: &str) -> Result<()>;
    /// What a sync would refuse this job for: a working directory that is
    /// not there, an agent that is not on the `PATH`.
    fn warnings(&self, spec: &JobSpec) -> Vec<String>;
    /// Sees that the prompt file exists, creating it empty when it does not.
    /// Returns the file when it was created, which is when it needs writing.
    fn ensure_prompt(&self, prompt: &str) -> Result<Option<PathBuf>>;
}

/// What the real world is built from. Everything read from the environment
/// arrives here, already read.
pub struct Setup {
    /// The jobs file.
    pub config: PathBuf,
    pub store: Store,
    pub runner: Box<dyn Runner>,
    /// The otto binary a run is started with.
    pub otto: PathBuf,
    /// `$VISUAL`, or else `$EDITOR`.
    pub editor: Option<String>,
    pub zone: TimeZone,
    /// `$PATH`, where an agent is looked for.
    pub path: Option<OsString>,
    /// The scheduler of this system and what its units are built from; `None`
    /// where otto has no backend, or cannot say what a unit would hold.
    pub sync: Option<(Box<dyn Scheduler>, Context)>,
}

/// The machine otto is running on.
pub struct Real {
    setup: Setup,
    children: Children,
    /// The jobs file as last read, so an unchanged file is not parsed again.
    text: Option<String>,
    /// The jobs of the last read that was valid.
    jobs: BTreeMap<String, Job>,
    /// Why the jobs file as it is now cannot be used.
    broken: Option<String>,
}

impl Real {
    pub fn new(setup: Setup) -> Real {
        Real {
            setup,
            children: Children::new(),
            text: None,
            jobs: BTreeMap::new(),
            broken: None,
        }
    }

    /// What a sync does with each job, leaving out those it leaves alone.
    /// With `dry_run` nothing is touched: it is what a sync would do.
    fn sync(&self, now: Timestamp, dry_run: bool) -> Vec<Pending> {
        let Some((scheduler, ctx)) = &self.setup.sync else {
            return Vec::new();
        };
        // A jobs file that cannot be read is not synced from the last one
        // that could: the units would follow a file that is not there.
        // Nor is one that is not there synced as a file with no jobs, which
        // would remove every unit.
        if self.broken.is_some() || self.text.is_none() {
            return Vec::new();
        }
        let config = Config {
            jobs: self.jobs.clone(),
        };
        let Setup { store, runner, .. } = &self.setup;
        let is_running = |job: &str| store.is_running(job, runner.as_ref(), now);
        sync::sync(
            &config,
            scheduler.as_ref(),
            ctx,
            runner.as_ref(),
            &is_running,
            dry_run,
        )
        .into_iter()
        .filter(|outcome| !matches!(outcome.result, Ok(Action::Unchanged)))
        .map(|outcome| Pending {
            job: outcome.job,
            change: outcome.result.map_err(|error| format!("{error:#}")),
        })
        .collect()
    }

    /// A path of the jobs file as the run will see it: `~` expanded, and a
    /// relative one taken from where the jobs file is.
    fn resolved(&self, path: &str) -> PathBuf {
        let base = self.setup.config.parent().unwrap_or(Path::new("."));
        config::resolve(Path::new(path), base, config::home_dir().as_deref())
    }

    /// Reads the jobs file again. One that is missing is an empty list; one
    /// that is invalid keeps the jobs of the last valid read.
    fn reload(&mut self) {
        let path = &self.setup.config;
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                self.text = None;
                self.jobs.clear();
                self.broken = None;
                return;
            }
            Err(error) => {
                self.text = None;
                self.broken = Some(format!("cannot read config {}: {error}", path.display()));
                return;
            }
        };
        if self.text.as_deref() == Some(text.as_str()) {
            return;
        }
        let base = path.parent().unwrap_or(Path::new("."));
        match Config::parse(&text, base, config::home_dir().as_deref())
            .with_context(|| format!("invalid config {}", path.display()))
        {
            Ok(config) => {
                self.jobs = config.jobs;
                self.broken = None;
            }
            Err(error) => self.broken = Some(format!("{error:#}")),
        }
        self.text = Some(text);
    }
}

impl World for Real {
    fn snapshot(&mut self, now: Timestamp) -> Snapshot {
        // A run this otto started and that has ended must leave the process
        // table before the store is asked whether it is still alive.
        self.children.reap();
        self.reload();
        let mut error = self.broken.clone();
        let mut note = |problem: anyhow::Error| {
            error.get_or_insert_with(|| format!("{problem:#}"));
        };
        let Setup {
            store,
            runner,
            zone,
            ..
        } = &self.setup;
        let mut jobs = Vec::new();
        for (name, job) in &self.jobs {
            let state = store.state(name).unwrap_or_else(|problem| {
                note(problem);
                State::default()
            });
            let runs = store
                .runs(name, runner.as_ref(), now)
                .unwrap_or_else(|problem| {
                    note(problem);
                    Vec::new()
                });
            let next = next::next(&job.schedule, state, now, zone)
                .map_err(&mut note)
                .ok();
            jobs.push(JobView {
                name: name.clone(),
                job: job.clone(),
                state,
                runs,
                next,
            });
        }
        Snapshot {
            config: self.setup.config.clone(),
            jobs,
            error,
            pending: self.sync(now, true),
            can_sync: self.setup.sync.is_some(),
        }
    }

    fn apply(&mut self, now: Timestamp) -> Vec<Pending> {
        self.children.reap();
        // What is applied is what was looked at: a jobs file that changed
        // since is one nobody reviewed.
        let seen = self.text.clone();
        self.reload();
        if self.text != seen {
            return vec![Pending {
                job: "*".to_owned(),
                change: Err("the jobs file changed: review the sync again".to_owned()),
            }];
        }
        self.sync(now, false)
    }

    fn log(&self, job: &str, run: &str) -> Result<Log> {
        let path = self.setup.store.log_path(job, run)?;
        let mut file =
            File::open(&path).with_context(|| format!("no output for run {run} of {job}"))?;
        let cannot_read = || format!("cannot read {}", path.display());
        let length = file.metadata().with_context(cannot_read)?.len();
        let truncated = length > LOG_TAIL;
        if truncated {
            file.seek(SeekFrom::Start(length - LOG_TAIL))
                .with_context(cannot_read)?;
        }
        let mut bytes = Vec::new();
        file.take(LOG_TAIL)
            .read_to_end(&mut bytes)
            .with_context(cannot_read)?;
        let mut shown = bytes.as_slice();
        if truncated {
            // The cut falls inside a line: start at the next one.
            if let Some(end) = shown.iter().position(|byte| *byte == b'\n') {
                shown = &shown[end + 1..];
            }
        }
        Ok(Log {
            text: printable(shown),
            truncated,
        })
    }

    fn set_state(&self, job: &str, state: State) -> Result<()> {
        self.setup.store.set_state(job, state)
    }

    fn start(&mut self, job: &str) -> Result<()> {
        // The run has no terminal to complain on: what would stop it before
        // it opens its record is checked here, where the screen can say it.
        Config::load(&self.setup.config)?.job(job)?;
        let mut command = Command::new(&self.setup.otto);
        command
            .arg("--config")
            .arg(&self.setup.config)
            .arg("run")
            .arg(job);
        self.children.start(command)?;
        Ok(())
    }

    fn stop(&self, job: &str, pid: u32) -> Result<()> {
        process::stop_group(pid, job, self.setup.runner.as_ref())
    }

    fn edit(&self, path: &Path) -> Result<()> {
        // The value may carry arguments, as in `code -w`, and quotes around
        // a path with a space in it.
        let parts = words(self.setup.editor.as_deref().unwrap_or_default());
        let (program, arguments) = parts
            .split_first()
            .filter(|(program, _)| !program.is_empty())
            .context("set $VISUAL or $EDITOR to edit the prompt")?;
        // Looked up as an agent is: on Windows an editor is often a `.cmd`.
        let status = crate::run::command_for(program)
            .args(arguments)
            .arg(path)
            .status()
            .with_context(|| format!("cannot start {program}"))?;
        if !status.success() {
            bail!("{program} ended with {status}");
        }
        Ok(())
    }

    fn text(&self) -> Result<String> {
        let path = &self.setup.config;
        match fs::read_to_string(path) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    fn save(&mut self, read: &str, text: &str) -> Result<()> {
        jobs_file::save(&self.setup.config, read, text)
    }

    fn warnings(&self, spec: &JobSpec) -> Vec<String> {
        let mut warnings = Vec::new();
        let workdir = self.resolved(&spec.workdir);
        if !spec.workdir.is_empty() && !workdir.is_dir() {
            // Without the path: the form shows it on the row above, and the
            // line this goes on is too short for it and a second note.
            warnings.push("workdir not found".to_owned());
        }
        let program = spec.agent.program();
        let on_the_path = self
            .setup
            .path
            .as_deref()
            .is_some_and(|path| which::find(program, path).is_some());
        if !on_the_path {
            warnings.push(format!("{program} not found in PATH"));
        }
        warnings
    }

    fn ensure_prompt(&self, prompt: &str) -> Result<Option<PathBuf>> {
        let path = self.resolved(prompt);
        if path.exists() {
            return Ok(None);
        }
        let cannot_create = || format!("cannot create the prompt file {}", path.display());
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(cannot_create)?;
        }
        File::create_new(&path).with_context(cannot_create)?;
        Ok(Some(path))
    }
}

/// The words of a command line as a shell reads them: split at spaces, with
/// single and double quotes and a backslash keeping a space inside a word. A
/// quote that is never closed takes the rest.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, quote) {
            (c, Some(open)) if c == open => quote = None,
            ('\'' | '"', None) => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            // A backslash escapes a space, a quote or another backslash, and
            // is itself anywhere else: `C:\tools\vim.exe` is a path.
            ('\\', None) => {
                let mut ahead = chars.clone();
                let escaped = ahead
                    .next()
                    .filter(|next| next.is_whitespace() || matches!(next, '\'' | '"' | '\\'));
                word.get_or_insert_default().push(escaped.unwrap_or('\\'));
                if escaped.is_some() {
                    chars = ahead;
                }
            }
            (c, None) if c.is_whitespace() => words.extend(word.take()),
            (c, _) => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    words
}

const ESCAPE: char = '\u{1b}';
const BELL: char = '\u{7}';

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Skips the rest of a control sequence: parameters, then one final byte.
fn skip_control(chars: &mut Chars) {
    for c in chars.by_ref() {
        if ('@'..='~').contains(&c) {
            break;
        }
    }
}

/// Skips the rest of a string sequence (a title, a device control string): it
/// ends at a bell or at a terminator. One that is never closed ends with its
/// line.
fn skip_string(chars: &mut Chars) {
    while let Some(c) = chars.next_if(|next| *next != '\n') {
        match c {
            BELL => break,
            ESCAPE => {
                chars.next_if_eq(&'\\');
                break;
            }
            _ => {}
        }
    }
}

/// What fits a terminal cell: no escape sequence, a tab as four spaces and no
/// other control character. Line feeds stay. Text after a carriage return
/// rewrites its line, as a progress indicator means it to. Bytes that are not
/// UTF-8 become the replacement character.
pub fn printable(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    // Where the line being written starts in `out`, and whether a carriage
    // return has taken the cursor back there.
    let mut line = 0;
    let mut rewrite = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // An escape followed by a control character is cut short: the
            // line feed after it is still a line feed.
            ESCAPE => match chars.next_if(|next| !next.is_control()) {
                Some('[') => skip_control(&mut chars),
                // An operating system command or a device control string.
                Some(']' | 'P') => skip_string(&mut chars),
                // Intermediate bytes, then one final byte: `ESC ( B`.
                Some(' '..='/') => {
                    while chars.next_if(|next| (' '..='/').contains(next)).is_some() {}
                    chars.next_if(|next| !next.is_control());
                }
                // Any other escape is two characters long.
                _ => {}
            },
            '\n' => {
                out.push('\n');
                line = out.len();
                rewrite = false;
            }
            // It takes the cursor back to the start of the line. The line
            // is only rewritten if text follows: before a line feed, or with
            // nothing but a colour reset after it, the line stays.
            '\r' => rewrite = true,
            c if c.is_control() && c != '\t' => {}
            c => {
                if rewrite {
                    out.truncate(line);
                    rewrite = false;
                }
                match c {
                    '\t' => out.push_str("    "),
                    c => out.push(c),
                }
            }
        }
    }
    out
}

/// A world that records what it is asked and answers from a script.
#[cfg(test)]
#[derive(Default)]
pub struct Fake {
    pub snapshot: Snapshot,
    pub log: Log,
    /// When set, everything that can fail fails with this message.
    pub fail: Option<String>,
    /// When set as well, only the call whose line starts with this fails.
    pub fail_on: Option<String>,
    pub calls: std::cell::RefCell<Vec<String>>,
    /// The jobs file as `text` gives it.
    pub text: String,
    pub warnings: Vec<String>,
    /// The prompt file `ensure_prompt` says it created.
    pub created: Option<PathBuf>,
    /// What `apply` says it did.
    pub applied: Vec<Pending>,
}

#[cfg(test)]
impl Fake {
    fn call(&self, line: String) -> Result<()> {
        let chosen = self
            .fail_on
            .as_deref()
            .is_none_or(|start| line.starts_with(start));
        self.calls.borrow_mut().push(line);
        match &self.fail {
            Some(message) if chosen => bail!("{message}"),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
impl World for Fake {
    fn snapshot(&mut self, _now: Timestamp) -> Snapshot {
        self.calls.borrow_mut().push("snapshot".to_owned());
        self.snapshot.clone()
    }

    fn log(&self, job: &str, run: &str) -> Result<Log> {
        self.call(format!("log {job} {run}"))?;
        Ok(self.log.clone())
    }

    fn set_state(&self, job: &str, state: State) -> Result<()> {
        self.call(format!("set_state {job} {}", state.label()))
    }

    fn start(&mut self, job: &str) -> Result<()> {
        self.call(format!("start {job}"))
    }

    fn stop(&self, job: &str, pid: u32) -> Result<()> {
        self.call(format!("stop {job} {pid}"))
    }

    fn edit(&self, path: &Path) -> Result<()> {
        self.call(format!("edit {}", path.display()))
    }

    fn apply(&mut self, _now: Timestamp) -> Vec<Pending> {
        self.calls.borrow_mut().push("apply".to_owned());
        self.applied.clone()
    }

    fn text(&self) -> Result<String> {
        self.call("text".to_owned())?;
        Ok(self.text.clone())
    }

    fn save(&mut self, _read: &str, text: &str) -> Result<()> {
        self.call(format!("save {text}"))
    }

    fn warnings(&self, _spec: &JobSpec) -> Vec<String> {
        self.warnings.clone()
    }

    fn ensure_prompt(&self, prompt: &str) -> Result<Option<PathBuf>> {
        self.call(format!("ensure_prompt {prompt}"))?;
        Ok(self.created.clone())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::scheduler::runner::Recorder;
    use crate::store::Trigger;

    pub(super) const JOBS: &str = "\
[jobs.report]
agent = \"claude\"
prompt = \"report.md\"
workdir = \".\"
schedule = { at = \"16:05\", days = [\"mon\", \"tue\", \"wed\", \"thu\", \"fri\"] }

[jobs.triage]
agent = \"codex\"
prompt = \"triage.md\"
workdir = \".\"
schedule = { at = \"07:00\" }
";

    /// Tuesday, 09:00 in São Paulo.
    fn now() -> Timestamp {
        at("2026-10-06T12:00:00Z")
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    pub(super) fn setup(dir: &TempDir, runner: Box<dyn Runner>) -> Setup {
        Setup {
            config: dir.path().join("jobs.toml"),
            store: Store::new(dir.path().join("state")),
            runner,
            otto: PathBuf::from("otto"),
            editor: None,
            zone: TimeZone::get("America/Sao_Paulo").unwrap(),
            path: None,
            sync: None,
        }
    }

    /// A machine whose jobs file holds `jobs`, and a second handle on its store.
    fn machine(jobs: Option<&str>) -> (TempDir, Real, Store) {
        let dir = tempfile::tempdir().unwrap();
        if let Some(jobs) = jobs {
            fs::write(dir.path().join("jobs.toml"), jobs).unwrap();
        }
        let real = Real::new(setup(&dir, Box::new(Recorder::new())));
        let store = Store::new(dir.path().join("state"));
        (dir, real, store)
    }

    #[test]
    fn escapes_and_control_bytes_do_not_reach_the_screen() {
        assert_eq!(printable(b"\x1b[31mred\x1b[0m\tok\r\n\x07"), "red    ok\n");
        assert_eq!(printable(b"\x1b]0;title\x07after"), "after");
        assert_eq!(printable(b"\x1b]8;;http://x\x1b\\link"), "link");
        assert_eq!(printable(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn a_progress_line_shows_where_it_ended() {
        // A carriage return alone takes the cursor back to rewrite the line.
        assert_eq!(printable(b"10%\r20%\r100%\r\nnext"), "100%\nnext");
        assert_eq!(printable(b"kept\nold\rnew"), "kept\nnew");
    }

    #[test]
    fn a_device_control_string_is_left_out() {
        // A device control string, closed by `ESC \`.
        assert_eq!(printable(b"a\x1bP1$r0m\x1b\\b"), "ab");
    }

    /// Text decoded with the wrong character set is full of these characters,
    /// and what follows one of them on the line is still text.
    #[test]
    fn a_stray_control_character_takes_nothing_with_it() {
        assert_eq!(
            printable("quote â€\u{9d} rest of line\nnext".as_bytes()),
            "quote â€ rest of line\nnext"
        );
        assert_eq!(
            printable("hyphen â€\u{90} rest\nnext".as_bytes()),
            "hyphen â€ rest\nnext"
        );
        assert_eq!(
            printable(b"keep\x1bXthis text\nnext"),
            "keepthis text\nnext"
        );
    }

    #[test]
    fn a_carriage_return_erases_only_when_text_follows_it() {
        // What a Windows text-mode writer leaves at the end of a line.
        assert_eq!(printable(b"hello\r\r\nworld"), "hello\nworld");
        // A colour reset after it is not text.
        assert_eq!(printable(b"\x1b[31merror\r\x1b[0m\nnext"), "error\nnext");
        assert_eq!(printable(b"a\r\x00\nb"), "a\nb");
        assert_eq!(printable(b"done\r"), "done");
        assert_eq!(printable(b"1\r\t2\n"), "    2\n");
    }

    #[test]
    fn a_windows_path_keeps_its_backslashes() {
        assert_eq!(words("C:\\tools\\vim.exe -n"), ["C:\\tools\\vim.exe", "-n"]);
        assert_eq!(
            words("\"C:\\Program Files\\x\\e.exe\" -w"),
            ["C:\\Program Files\\x\\e.exe", "-w"]
        );
        // Only a space, a quote or another backslash is escaped by one.
        assert_eq!(words("a\\ b c\\\\d e\\\"f"), ["a b", "c\\d", "e\"f"]);
        assert_eq!(words("vim \\"), ["vim", "\\"]);
    }

    #[test]
    fn an_editor_that_is_only_quotes_counts_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let real = Real::new(Setup {
            editor: Some("\"\" -w".to_owned()),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        let error = real.edit(Path::new("prompt.md")).unwrap_err();
        assert!(format!("{error:#}").contains("$EDITOR"));
    }

    #[test]
    fn the_editor_value_is_read_like_a_shell_would() {
        assert_eq!(words("code -w"), ["code", "-w"]);
        assert_eq!(
            words("\"/Applications/My Editor/bin/edit\" --wait"),
            ["/Applications/My Editor/bin/edit", "--wait"]
        );
        assert_eq!(words("'/opt/my editor/e' -n"), ["/opt/my editor/e", "-n"]);
        assert_eq!(words("/opt/my\\ editor/e"), ["/opt/my editor/e"]);
        assert_eq!(words("  vim  "), ["vim"]);
        assert!(words("   ").is_empty());
        // A quote that is never closed takes the rest.
        assert_eq!(words("vim 'a b"), ["vim", "a b"]);
    }

    #[test]
    fn the_jobs_file_is_read_and_written_as_it_is() {
        let (dir, mut real, _store) = machine(None);
        assert_eq!(real.text().unwrap(), "");
        real.save("", JOBS).unwrap();
        assert_eq!(real.text().unwrap(), JOBS);
        assert_eq!(
            fs::read_to_string(dir.path().join("jobs.toml")).unwrap(),
            JOBS
        );
        // Written from a reading that is no longer what is on disk: refused.
        let error = real.save("", "# other\n").unwrap_err();
        assert!(format!("{error:#}").contains("changed on disk"));
        assert_eq!(real.text().unwrap(), JOBS);
    }

    fn spec(workdir: &str) -> JobSpec {
        JobSpec {
            agent: crate::agent::Agent::Claude,
            prompt: "prompts/nightly.md".to_owned(),
            workdir: workdir.to_owned(),
            at: "02:00".to_owned(),
            days: Vec::new(),
            args: Vec::new(),
            notify: crate::notify::Level::default(),
        }
    }

    #[test]
    fn a_job_a_sync_would_refuse_is_warned_about() {
        let (dir, real, _store) = machine(Some(JOBS));
        let warnings = real.warnings(&spec("not-there"));
        assert_eq!(
            warnings,
            [
                "workdir not found".to_owned(),
                "claude not found in PATH".to_owned(),
            ]
        );
        let _ = &dir;
        // A directory is taken from where the jobs file is; none typed yet is
        // not a directory that is missing.
        fs::create_dir(dir.path().join("work")).unwrap();
        assert_eq!(real.warnings(&spec("work")), ["claude not found in PATH"]);
        assert_eq!(real.warnings(&spec("")), ["claude not found in PATH"]);
    }

    #[test]
    fn a_missing_prompt_file_is_created_beside_the_jobs_file() {
        let (dir, real, _store) = machine(Some(JOBS));
        let path = dir.path().join("prompts").join("nightly.md");
        assert_eq!(
            real.ensure_prompt("prompts/nightly.md").unwrap(),
            Some(path.clone())
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        // One that is there is left as it is.
        fs::write(&path, "do the thing\n").unwrap();
        assert_eq!(real.ensure_prompt("prompts/nightly.md").unwrap(), None);
        assert_eq!(fs::read_to_string(&path).unwrap(), "do the thing\n");
    }

    #[test]
    fn a_broken_escape_does_not_eat_the_output() {
        // The character-set reset tput prints, three bytes long.
        assert_eq!(printable(b"\x1b(B\x1b[mhello"), "hello");
        assert_eq!(printable(b"a\x1b\nb"), "a\nb");
        // A title that is never closed ends with its line.
        assert_eq!(printable(b"\x1b]0;never closed\nnext line"), "\nnext line");
    }

    #[test]
    fn a_run_is_not_started_for_a_job_the_file_no_longer_has() {
        let (dir, mut real, _store) = machine(Some(JOBS));
        let error = real.start("gone").unwrap_err();
        assert!(format!("{error:#}").contains("no job named"));

        fs::write(dir.path().join("jobs.toml"), "jobs = 3\n").unwrap();
        let error = real.start("report").unwrap_err();
        assert!(format!("{error:#}").contains("invalid config"));
    }

    #[test]
    fn a_missing_jobs_file_is_an_empty_list() {
        let (dir, mut real, _store) = machine(None);
        let snapshot = real.snapshot(now());
        assert!(snapshot.jobs.is_empty());
        assert_eq!(snapshot.error, None);
        assert_eq!(snapshot.config, dir.path().join("jobs.toml"));
    }

    #[test]
    fn a_job_carries_its_state_runs_and_next_run() {
        let (_dir, mut real, store) = machine(Some(JOBS));
        store
            .set_state(
                "report",
                State {
                    paused: true,
                    skip_next: false,
                },
            )
            .unwrap();
        let run = store
            .begin("report", Trigger::Scheduled, 1, at("2026-10-05T19:16:26Z"))
            .unwrap();
        store
            .finish("report", &run, Some(3), at("2026-10-05T19:18:40Z"))
            .unwrap();

        let snapshot = real.snapshot(now());

        let names: Vec<&str> = snapshot.jobs.iter().map(|job| job.name.as_str()).collect();
        assert_eq!(names, ["report", "triage"]);
        let report = &snapshot.jobs[0];
        assert_eq!(report.status(), "paused");
        assert_eq!(report.runs[0].outcome_label(), "failed (3)");
        assert_eq!(report.next, Some(Next::Paused));
        let triage = &snapshot.jobs[1];
        assert_eq!(triage.status(), "active");
        assert!(triage.runs.is_empty());
        let next = triage.next.as_ref().and_then(Next::runs_at).unwrap();
        assert_eq!(next.timestamp(), at("2026-10-07T10:00:00Z"));
    }

    #[test]
    fn a_run_in_progress_wins_the_status() {
        let (_dir, mut real, store) = machine(Some(JOBS));
        store
            .set_state(
                "report",
                State {
                    paused: true,
                    skip_next: false,
                },
            )
            .unwrap();
        store
            .begin("report", Trigger::Manual, 4242, at("2026-10-06T11:59:00Z"))
            .unwrap();

        let snapshot = real.snapshot(now());

        let report = &snapshot.jobs[0];
        assert_eq!(report.status(), "running");
        assert_eq!(report.running().and_then(|run| run.pid), Some(4242));
        assert!(snapshot.jobs[1].running().is_none());
    }

    #[test]
    fn a_broken_jobs_file_keeps_the_last_good_list() {
        let (dir, mut real, _store) = machine(Some(JOBS));
        let path = dir.path().join("jobs.toml");
        assert_eq!(real.snapshot(now()).jobs.len(), 2);

        fs::write(&path, "jobs = 3\n").unwrap();
        let broken = real.snapshot(now());
        assert_eq!(broken.jobs.len(), 2);
        assert!(broken.error.unwrap().contains("invalid config"));
        // The error stays for as long as the file does.
        assert!(real.snapshot(now()).error.is_some());

        fs::write(&path, JOBS).unwrap();
        let fixed = real.snapshot(now());
        assert_eq!(fixed.jobs.len(), 2);
        assert_eq!(fixed.error, None);
    }

    #[test]
    fn a_jobs_file_that_is_removed_empties_the_list() {
        let (dir, mut real, _store) = machine(Some(JOBS));
        assert_eq!(real.snapshot(now()).jobs.len(), 2);
        fs::remove_file(dir.path().join("jobs.toml")).unwrap();
        let snapshot = real.snapshot(now());
        assert!(snapshot.jobs.is_empty());
        assert_eq!(snapshot.error, None);
    }

    /// A run of `report` whose output is `output`.
    fn run_with_output(store: &Store, output: &[u8]) -> String {
        let run = store
            .begin("report", Trigger::Manual, 1, at("2026-10-06T11:00:00Z"))
            .unwrap();
        fs::write(store.log_path("report", &run.id).unwrap(), output).unwrap();
        run.id
    }

    #[test]
    fn a_long_log_is_cut_at_a_line() {
        let (_dir, real, store) = machine(Some(JOBS));
        // Lines of exactly 100 bytes, a little more than the tail holds.
        let lines = usize::try_from(LOG_TAIL).unwrap() / 100 + 5;
        let output: String = (0..lines).map(|line| format!("{line:099}\n")).collect();
        let id = run_with_output(&store, output.as_bytes());

        let log = real.log("report", &id).unwrap();

        assert!(log.truncated);
        assert!(log.text.len() <= usize::try_from(LOG_TAIL).unwrap());
        assert_eq!(log.text.lines().next().unwrap().len(), 99);
        assert!(log.text.ends_with(&format!("{:099}\n", lines - 1)));
    }

    #[test]
    fn a_short_log_is_whole() {
        let (_dir, real, store) = machine(Some(JOBS));
        let id = run_with_output(&store, b"one\ntwo\n");
        assert_eq!(
            real.log("report", &id).unwrap(),
            Log {
                text: "one\ntwo\n".to_owned(),
                truncated: false
            }
        );
    }

    #[test]
    fn a_run_without_output_is_an_error() {
        let (_dir, real, store) = machine(Some(JOBS));
        let run = store
            .record("report", Trigger::Scheduled, Outcome::Skipped, now())
            .unwrap();
        let error = real.log("report", &run.id).unwrap_err();
        assert!(format!("{error:#}").contains("no output"));
        assert!(real.log("report", "../../etc").is_err());
    }

    #[test]
    fn state_goes_to_the_store() {
        let (_dir, real, store) = machine(Some(JOBS));
        let state = State {
            paused: false,
            skip_next: true,
        };
        real.set_state("report", state).unwrap();
        assert_eq!(store.state("report").unwrap(), state);
    }

    #[test]
    fn no_editor_is_an_error_that_says_what_to_set() {
        let (_dir, real, _store) = machine(Some(JOBS));
        let error = real.edit(Path::new("prompt.md")).unwrap_err();
        assert!(format!("{error:#}").contains("$EDITOR"));
    }

    #[test]
    fn a_blank_editor_counts_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let real = Real::new(Setup {
            editor: Some("  ".to_owned()),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        let error = real.edit(Path::new("prompt.md")).unwrap_err();
        assert!(format!("{error:#}").contains("$EDITOR"));
    }
}

// These tests start scripts with an execute bit and ask `kill` about them.
#[cfg(test)]
#[cfg(unix)]
mod unix_tests {
    use std::os::unix::fs::PermissionsExt;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use std::cell::RefCell;
    use std::rc::Rc;

    use super::tests::{JOBS, setup};
    use super::*;
    use crate::scheduler::Context;
    use crate::scheduler::fake::Fake;
    use crate::scheduler::runner::{Recorder, System};
    use crate::sync::Action;

    /// An executable that writes `line` (a shell expression) to `<dir>/out`.
    fn script(dir: &TempDir, line: &str) -> PathBuf {
        let path = dir.path().join("script");
        let out = dir.path().join("out");
        fs::write(
            &path,
            format!("#!/bin/sh\necho {line} > {}\n", out.display()),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// What the script wrote, once it has.
    fn output(dir: &TempDir) -> String {
        let out = dir.path().join("out");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = fs::read_to_string(&out).unwrap_or_default();
            if text.ends_with('\n') || Instant::now() > deadline {
                return text.trim().to_owned();
            }
            sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn start_runs_otto_with_the_same_jobs_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("jobs.toml"), JOBS).unwrap();
        let mut real = Real::new(Setup {
            otto: script(&dir, "\"$@\""),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        real.start("report").unwrap();
        assert_eq!(
            output(&dir),
            format!(
                "--config {} run report",
                dir.path().join("jobs.toml").display()
            )
        );
    }

    #[test]
    fn a_child_that_ended_is_gone_by_the_next_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("jobs.toml"), JOBS).unwrap();
        let mut real = Real::new(Setup {
            otto: script(&dir, "$$"),
            ..setup(&dir, Box::new(System))
        });
        real.start("report").unwrap();
        let pid = output(&dir);
        assert!(!pid.is_empty());

        let deadline = Instant::now() + Duration::from_secs(5);
        let gone = loop {
            real.snapshot("2026-10-06T12:00:00Z".parse().unwrap());
            if !System.run("kill", &["-0", &pid]).unwrap().success {
                break true;
            }
            if Instant::now() > deadline {
                break false;
            }
            sleep(Duration::from_millis(20));
        };
        assert!(gone, "the child stayed in the process table");
    }

    #[test]
    fn the_editor_gets_its_arguments_and_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let editor = format!("{} --wait", script(&dir, "\"$@\"").display());
        let real = Real::new(Setup {
            editor: Some(editor),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        real.edit(Path::new("/prompts/report.md")).unwrap();
        assert_eq!(output(&dir), "--wait /prompts/report.md");
    }

    #[test]
    fn an_editor_in_a_directory_with_a_space_is_started() {
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("my editor");
        fs::create_dir(&spaced).unwrap();
        let editor = spaced.join("edit");
        fs::copy(script(&dir, "\"$@\""), &editor).unwrap();
        let real = Real::new(Setup {
            editor: Some(format!("\"{}\" --wait", editor.display())),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        real.edit(Path::new("/prompts/report.md")).unwrap();
        assert_eq!(output(&dir), "--wait /prompts/report.md");
    }

    /// A machine with the two jobs of `JOBS`, their prompts, both agents on
    /// the `PATH` and a scheduler that keeps its units in `units`.
    struct Synced {
        dir: TempDir,
        real: Real,
        /// What the scheduler was asked to do.
        asked: Rc<RefCell<Vec<String>>>,
        store: Store,
    }

    fn synced_with(broken: Option<&str>) -> Synced {
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name);
        fs::write(at("jobs.toml"), JOBS).unwrap();
        for dir in ["bin", "units"] {
            fs::create_dir(at(dir)).unwrap();
        }
        for prompt in ["report.md", "triage.md"] {
            fs::write(at(prompt), "do it").unwrap();
        }
        for agent in ["claude", "codex"] {
            fs::write(at("bin").join(agent), "").unwrap();
            fs::set_permissions(at("bin").join(agent), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut scheduler = Fake::new(at("units"));
        scheduler.broken = broken.map(str::to_owned);
        let asked = Rc::clone(&scheduler.calls);
        let ctx = Context {
            otto: PathBuf::from("/usr/bin/otto"),
            config: at("jobs.toml"),
            path: at("bin").to_string_lossy().into_owned(),
            log_dir: at("logs"),
            version: "1.2.3",
        };
        let real = Real::new(Setup {
            sync: Some((Box::new(scheduler), ctx)),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        let store = Store::new(at("state"));
        let mut machine = Synced {
            dir,
            real,
            asked,
            store,
        };
        // The screen looks before it applies: so does every machine here.
        machine.real.snapshot(now());
        machine
    }

    fn synced() -> Synced {
        synced_with(None)
    }

    fn now() -> Timestamp {
        "2026-10-06T12:00:00Z".parse().unwrap()
    }

    /// What is pending, as `job action` or `job: reason`.
    fn pending(list: &[Pending]) -> Vec<String> {
        list.iter()
            .map(|pending| match &pending.change {
                Ok(action) => format!("{} {}", pending.job, action.label()),
                Err(reason) => format!("{}: {reason}", pending.job),
            })
            .collect()
    }

    #[test]
    fn a_job_without_a_unit_is_pending_as_an_addition() {
        let mut machine = synced();
        let snapshot = machine.real.snapshot(now());
        assert!(snapshot.can_sync);
        assert_eq!(pending(&snapshot.pending), ["report added", "triage added"]);
        assert_eq!(
            snapshot
                .pending_for("report")
                .map(|pending| &pending.change),
            Some(&Ok(Action::Add))
        );
        assert!(snapshot.pending_for("gone").is_none());
    }

    #[test]
    fn looking_never_asks_the_scheduler_for_anything() {
        let mut machine = synced();
        for _ in 0..3 {
            machine.real.snapshot(now());
        }
        assert!(machine.asked.borrow().is_empty());
        assert_eq!(
            fs::read_dir(machine.dir.path().join("units"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn applying_loads_what_was_pending_and_nothing_is_pending_after() {
        let mut machine = synced();
        let applied = machine.real.apply(now());
        assert_eq!(pending(&applied), ["report added", "triage added"]);
        assert_eq!(*machine.asked.borrow(), ["load report", "load triage"]);
        assert!(machine.real.snapshot(now()).pending.is_empty());
        // With nothing to do, applying does nothing.
        assert!(machine.real.apply(now()).is_empty());
        assert_eq!(machine.asked.borrow().len(), 2);
    }

    #[test]
    fn a_changed_schedule_is_pending_as_an_update() {
        let mut machine = synced();
        machine.real.apply(now());
        let later = JOBS.replace("16:05", "17:30");
        fs::write(machine.dir.path().join("jobs.toml"), later).unwrap();
        assert_eq!(
            pending(&machine.real.snapshot(now()).pending),
            ["report updated"]
        );
    }

    #[test]
    fn a_unit_without_a_job_is_pending_as_a_removal() {
        let mut machine = synced();
        machine.real.apply(now());
        let only_report = &JOBS[..JOBS.find("[jobs.triage]").unwrap()];
        fs::write(machine.dir.path().join("jobs.toml"), only_report).unwrap();
        assert_eq!(
            pending(&machine.real.snapshot(now()).pending),
            ["triage removed"]
        );
        assert_eq!(pending(&machine.real.apply(now())), ["triage removed"]);
        assert_eq!(machine.asked.borrow().last().unwrap(), "unload triage");
    }

    #[test]
    fn a_job_the_sync_would_refuse_carries_the_reason() {
        let mut machine = synced();
        fs::remove_file(machine.dir.path().join("report.md")).unwrap();
        let snapshot = machine.real.snapshot(now());
        let report = snapshot.pending_for("report").unwrap();
        let reason = report.change.clone().unwrap_err();
        assert!(reason.contains("prompt file not found"), "{reason}");
        assert_eq!(snapshot.pending.len(), 2);
    }

    #[test]
    fn a_running_job_that_changed_is_busy_and_stays_pending() {
        let mut machine = synced();
        machine.real.apply(now());
        machine
            .store
            .begin("report", crate::store::Trigger::Manual, 4242, now())
            .unwrap();
        let later = JOBS.replace("16:05", "17:30");
        fs::write(machine.dir.path().join("jobs.toml"), later).unwrap();
        assert_eq!(
            pending(&machine.real.snapshot(now()).pending),
            ["report busy"]
        );
        assert_eq!(pending(&machine.real.apply(now())), ["report busy"]);
        assert_eq!(
            pending(&machine.real.snapshot(now()).pending),
            ["report busy"]
        );
        assert_eq!(
            machine.asked.borrow().len(),
            2,
            "the running job was left alone"
        );
    }

    #[test]
    fn a_job_the_scheduler_refuses_is_reported_and_the_others_go_through() {
        let mut machine = synced_with(Some("report"));
        let applied = pending(&machine.real.apply(now()));
        assert_eq!(applied.len(), 2);
        assert!(
            applied[0].starts_with("report: the scheduler refused report"),
            "{applied:?}"
        );
        assert_eq!(applied[1], "triage added");
        // The one that was refused is still to do.
        assert_eq!(
            pending(&machine.real.snapshot(now()).pending),
            ["report added"]
        );
    }

    #[test]
    fn nothing_is_pending_while_the_jobs_file_is_broken() {
        let mut machine = synced();
        machine.real.snapshot(now());
        fs::write(machine.dir.path().join("jobs.toml"), "jobs = 3\n").unwrap();
        let snapshot = machine.real.snapshot(now());
        assert!(snapshot.error.is_some());
        assert!(snapshot.pending.is_empty());
        // And a file that cannot be read is not applied from memory.
        assert!(machine.real.apply(now()).is_empty());
        assert!(machine.asked.borrow().is_empty());
    }

    #[test]
    fn a_jobs_file_that_is_gone_removes_no_unit() {
        let mut machine = synced();
        machine.real.apply(now());
        fs::remove_file(machine.dir.path().join("jobs.toml")).unwrap();
        let snapshot = machine.real.snapshot(now());
        assert!(snapshot.jobs.is_empty());
        assert!(snapshot.pending.is_empty());
        assert!(machine.real.apply(now()).is_empty());
        assert_eq!(*machine.asked.borrow(), ["load report", "load triage"]);
    }

    #[test]
    fn a_jobs_file_left_empty_still_removes_its_units() {
        let mut machine = synced();
        machine.real.apply(now());
        fs::write(machine.dir.path().join("jobs.toml"), "").unwrap();
        assert_eq!(
            pending(&machine.real.snapshot(now()).pending),
            ["report removed", "triage removed"]
        );
    }

    #[test]
    fn a_jobs_file_that_changed_since_the_look_is_not_applied() {
        let mut machine = synced();
        let later = JOBS.replace("16:05", "17:30");
        fs::write(machine.dir.path().join("jobs.toml"), later).unwrap();
        let refused = pending(&machine.real.apply(now()));
        assert_eq!(refused, ["*: the jobs file changed: review the sync again"]);
        assert!(machine.asked.borrow().is_empty());
        // Looked at again, it is applied.
        machine.real.snapshot(now());
        assert_eq!(
            pending(&machine.real.apply(now())),
            ["report added", "triage added"]
        );
    }

    #[test]
    fn a_jobs_file_that_went_away_since_the_look_is_not_applied() {
        let mut machine = synced();
        machine.real.apply(now());
        fs::remove_file(machine.dir.path().join("jobs.toml")).unwrap();
        let refused = pending(&machine.real.apply(now()));
        assert_eq!(refused, ["*: the jobs file changed: review the sync again"]);
        assert_eq!(*machine.asked.borrow(), ["load report", "load triage"]);
    }

    #[test]
    fn without_a_scheduler_nothing_is_pending_and_nothing_applies() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("jobs.toml"), JOBS).unwrap();
        let mut real = Real::new(setup(&dir, Box::new(Recorder::new())));
        let snapshot = real.snapshot(now());
        assert!(!snapshot.can_sync);
        assert!(snapshot.pending.is_empty());
        assert!(real.apply(now()).is_empty());
    }

    #[test]
    fn an_agent_on_the_path_is_not_warned_about() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("claude"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
        let real = Real::new(Setup {
            path: Some(bin.into_os_string()),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        let here = JobSpec {
            agent: crate::agent::Agent::Claude,
            prompt: "p.md".to_owned(),
            workdir: ".".to_owned(),
            at: "02:00".to_owned(),
            days: Vec::new(),
            args: Vec::new(),
            notify: crate::notify::Level::default(),
        };
        assert!(real.warnings(&here).is_empty());
        let codex = JobSpec {
            agent: crate::agent::Agent::Codex,
            ..here
        };
        assert_eq!(real.warnings(&codex), ["codex not found in PATH"]);
    }

    #[test]
    fn an_editor_that_fails_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let real = Real::new(Setup {
            editor: Some("false".to_owned()),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        let error = real.edit(Path::new("prompt.md")).unwrap_err();
        assert!(format!("{error:#}").contains("false"));
    }

    #[test]
    fn an_editor_that_does_not_exist_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let real = Real::new(Setup {
            editor: Some("no-such-editor-otto".to_owned()),
            ..setup(&dir, Box::new(Recorder::new()))
        });
        let error = real.edit(Path::new("prompt.md")).unwrap_err();
        assert!(format!("{error:#}").contains("no-such-editor-otto"));
    }
}
