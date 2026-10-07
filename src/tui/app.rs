//! The state of the terminal UI. Actions come in, effects go out: nothing here
//! reads a file, the clock or the environment.

use std::path::PathBuf;

use crate::jobs_file::JobSpec;
use crate::store::{Outcome, Run, State};
use crate::sync;
use crate::tui::form::{Edit, Form};
use crate::tui::text::{rows, tail, wrapped};
use crate::tui::world::{JobView, Log, Pending, Snapshot};

/// The rows a job takes on the list: two lines and the rule that closes it.
pub const ENTRY: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Every job, one per line.
    Jobs,
    /// One job and its runs.
    Job,
    /// The output of one run.
    Log,
    Help,
    /// The form that creates or edits a job; what it holds is in `App::form`.
    Form,
    /// What a sync would do, and after it was applied what it did.
    Sync,
}

/// What the user asked for, whichever key it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Open,
    Back,
    Quit,
    Help,
    RunNow,
    Stop,
    Pause,
    Skip,
    Resume,
    EditPrompt,
    Yes,
    No,
    /// A new job.
    New,
    /// The selected job in the form.
    EditJob,
    Delete,
    Save,
    NextField,
    PrevField,
    /// The choice under the cursor, on or off.
    Toggle,
    /// A keystroke of editing, for the field in focus.
    Input(Edit),
    /// What a sync would do.
    Sync,
    /// Do it.
    Apply,
}

/// Something to do outside the UI's own state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    SetState {
        job: String,
        state: State,
    },
    Start {
        job: String,
    },
    Stop {
        job: String,
        pid: u32,
    },
    Edit {
        path: PathBuf,
    },
    Quit,
    /// Read the jobs file and open the form on it: for `job`, or for a new one.
    OpenForm {
        job: Option<String>,
    },
    /// What the form would warn about, for the job as it stands.
    Check {
        spec: JobSpec,
    },
    /// Write `text` over the jobs file read as `read`, then see that the
    /// prompt file of `job` exists.
    Save {
        job: String,
        read: String,
        text: String,
        prompt: String,
    },
    /// Take `job` out of the jobs file.
    Delete {
        job: String,
    },
    /// Make the scheduler match the jobs file.
    Apply,
}

/// A question the UI is waiting on before it acts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    Stop {
        job: String,
        pid: u32,
    },
    Delete {
        job: String,
    },
    /// Leave the form and lose what was typed.
    Discard,
    /// Hand the scheduler this many changes.
    Apply {
        changes: usize,
    },
}

/// One line told to the user: what could not be done, or what happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

pub struct App {
    pub snapshot: Snapshot,
    pub screen: Screen,
    /// The selected job, by name: the list changes under the selection.
    pub job: Option<String>,
    /// The selected run of that job, by id.
    pub run: Option<String>,
    pub log: Option<Log>,
    /// The log screen keeps to the end of the output as it grows.
    pub follow: bool,
    /// The first line shown when the log screen does not follow.
    pub top: usize,
    /// How many lines a page moves by: the height of what scrolls.
    pub page: usize,
    /// How many columns a line of the log has before it goes on in the next row.
    pub columns: usize,
    pub confirm: Option<Confirm>,
    pub notice: Option<Notice>,
    /// The form, while `Screen::Form` shows it.
    pub form: Option<Form>,
    /// What saving the form as it stands would run into later: a working
    /// directory that is not there, an agent that is not on the `PATH`.
    pub warnings: Vec<String>,
    /// Why the form could not be saved. It stays on screen until the job
    /// changes: a notice goes with the next key, and the next key is the one
    /// that takes the user to the field.
    pub reason: Option<String>,
    /// What the last sync did with each job, while the sync screen shows it.
    applied: Option<Vec<Pending>>,
    /// The first row shown of a sync screen that does not fit.
    sync_top: usize,
    /// Where `Back` leaves the help screen.
    behind_help: Screen,
    /// Where closing the form goes back to.
    behind_form: Screen,
    /// The log on screen was read while its run was still going, so it may
    /// be missing the end.
    log_is_partial: bool,
}

impl App {
    pub fn new(snapshot: Snapshot) -> App {
        let job = snapshot.jobs.first().map(|job| job.name.clone());
        App {
            snapshot,
            screen: Screen::Jobs,
            job,
            run: None,
            log: None,
            follow: true,
            top: 0,
            page: 20,
            columns: 80,
            confirm: None,
            notice: None,
            form: None,
            warnings: Vec::new(),
            reason: None,
            applied: None,
            sync_top: 0,
            behind_help: Screen::Jobs,
            behind_form: Screen::Jobs,
            log_is_partial: false,
        }
    }

    pub fn selected(&self) -> Option<&JobView> {
        let name = self.job.as_deref()?;
        self.snapshot.jobs.iter().find(|job| job.name == name)
    }

    pub fn selected_run(&self) -> Option<&Run> {
        let id = self.run.as_deref()?;
        self.selected()?.runs.iter().find(|run| run.id == id)
    }

    /// Takes a newer picture of the machine, keeping the selection on the same
    /// job and run when they are still there.
    pub fn refresh(&mut self, snapshot: Snapshot) {
        // The help screen sits on top of another one, which goes stale just
        // the same: settle that one, then put the help back.
        let helping = self.screen == Screen::Help;
        if helping {
            self.screen = self.behind_help;
        }
        self.settle(snapshot);
        if helping {
            self.behind_help = self.screen;
            self.screen = Screen::Help;
        }
    }

    fn settle(&mut self, snapshot: Snapshot) {
        let was_at = self.job_index();
        self.snapshot = snapshot;
        // A question about a run that is over, or about a job that is gone,
        // has nothing left to answer.
        let listed = |name: &str| self.snapshot.jobs.iter().any(|view| view.name == name);
        let answerable = match &self.confirm {
            Some(Confirm::Stop { job, pid }) => self.snapshot.jobs.iter().any(|view| {
                view.name == *job && view.running().is_some_and(|run| run.pid == Some(*pid))
            }),
            Some(Confirm::Delete { job }) => listed(job),
            Some(Confirm::Discard | Confirm::Apply { .. }) | None => true,
        };
        if !answerable {
            self.confirm = None;
        }
        // The question counts what would be applied now, and goes with it.
        if let Some(Confirm::Apply { .. }) = self.confirm {
            self.confirm = match self.applicable() {
                0 => None,
                changes => Some(Confirm::Apply { changes }),
            };
        }
        // The same for the form of a job that is no longer in the file: what
        // it would save over is gone.
        let orphaned = self
            .form
            .as_ref()
            .and_then(|form| form.editing.clone())
            .filter(|job| !listed(job));
        if let Some(job) = orphaned {
            self.close_form();
            self.confirm = None;
            self.screen = Screen::Jobs;
            self.say(format!("{job} is no longer in the jobs file"));
        }
        // Something else did the sync: there is nothing left to preview.
        if self.screen == Screen::Sync && self.applied.is_none() && self.snapshot.pending.is_empty()
        {
            self.leave_sync();
            self.say("nothing left to apply".to_owned());
        }
        let jobs = &self.snapshot.jobs;
        if self.selected().is_none() {
            let gone = self.job.take();
            self.job = was_at
                .and_then(|index| jobs.get(index).or(jobs.last()))
                .or(jobs.first())
                .map(|job| job.name.clone());
            self.run = None;
            if let (Screen::Job | Screen::Log, Some(gone)) = (self.screen, gone) {
                self.leave_log();
                self.screen = Screen::Jobs;
                self.say(format!("{gone} is no longer in the jobs file"));
            }
        }
        if self.screen != Screen::Jobs && self.selected_run().is_none() {
            let had_one = self.run.is_some();
            self.run = self.newest_run();
            if self.screen == Screen::Log {
                self.leave_log();
                self.screen = Screen::Job;
                if had_one {
                    self.say("that run is no longer kept".to_owned());
                }
            }
        }
    }

    pub fn act(&mut self, action: Action) -> Vec<Effect> {
        if let Some(confirm) = self.confirm.clone() {
            return match action {
                Action::Yes => {
                    self.confirm = None;
                    match confirm {
                        Confirm::Stop { job, pid } => vec![Effect::Stop { job, pid }],
                        Confirm::Delete { job } => vec![Effect::Delete { job }],
                        Confirm::Apply { .. } => vec![Effect::Apply],
                        Confirm::Discard => {
                            self.close_form();
                            Vec::new()
                        }
                    }
                }
                Action::No => {
                    self.confirm = None;
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        self.notice = None;
        if self.screen == Screen::Form {
            return self.fill(action);
        }
        match action {
            Action::Up => self.step(-1),
            Action::Down => self.step(1),
            Action::PageUp => self.step(-self.page_step()),
            Action::PageDown => self.step(self.page_step()),
            Action::Top => self.step(isize::MIN),
            Action::Bottom => self.step(isize::MAX),
            Action::Open => self.open(),
            Action::Back => self.back(),
            Action::Help => {
                if self.screen != Screen::Help {
                    self.behind_help = self.screen;
                    self.screen = Screen::Help;
                }
            }
            Action::Quit => return vec![Effect::Quit],
            Action::Yes | Action::No => {}
            Action::New if self.screen == Screen::Jobs => {
                return vec![Effect::OpenForm { job: None }];
            }
            Action::EditJob | Action::Delete => return self.manage(action),
            Action::Sync if self.screen == Screen::Jobs => self.preview(),
            Action::Apply if self.screen == Screen::Sync => self.ask_to_apply(),
            // Each of the two has one screen it means something on.
            Action::Sync | Action::Apply => {}
            // The keys of the form mean nothing anywhere else.
            Action::New
            | Action::Save
            | Action::NextField
            | Action::PrevField
            | Action::Toggle
            | Action::Input(_) => {}
            Action::RunNow
            | Action::Stop
            | Action::Pause
            | Action::Skip
            | Action::Resume
            | Action::EditPrompt => return self.operate(action),
        }
        Vec::new()
    }

    /// The form, opened on the jobs file as `read` gives it: for `job`, or
    /// empty for a new one. A file or a job that cannot be read is told.
    pub fn open_form(&mut self, read: Result<String, String>, job: Option<&str>) {
        let form = read.and_then(|read| match job {
            Some(job) => Form::edit(read, job).map_err(|error| format!("{error:#}")),
            None => Ok(Form::create(read)),
        });
        match form {
            Ok(form) => {
                if self.screen != Screen::Form {
                    self.behind_form = self.screen;
                }
                self.form = Some(form);
                self.warnings.clear();
                self.screen = Screen::Form;
            }
            Err(text) => self.notice = Some(Notice { text, error: true }),
        }
    }

    pub fn show_warnings(&mut self, warnings: Vec<String>) {
        self.warnings = warnings;
    }

    /// The form was written to the jobs file: back to the list, on that job.
    pub fn saved(&mut self, job: &str) {
        self.close_form();
        self.screen = Screen::Jobs;
        self.job = Some(job.to_owned());
        self.run = None;
        // That it is not scheduled yet is on its entry, with the way there.
        self.say(format!("{job} saved"));
    }

    /// The job was taken out of the jobs file.
    pub fn deleted(&mut self, job: &str) {
        self.say(format!("{job} deleted"));
    }

    /// What the sync screen lists: what the sync did, once it was applied,
    /// and until then what it would do.
    pub fn changes(&self) -> &[Pending] {
        self.applied.as_deref().unwrap_or(&self.snapshot.pending)
    }

    /// Whether the sync screen shows a sync that was applied.
    pub fn applied(&self) -> bool {
        self.applied.is_some()
    }

    /// How many of the pending changes a sync would make now: not the ones
    /// that wait for a run to end, and not the ones it cannot make.
    pub fn applicable(&self) -> usize {
        use sync::Action::{Add, Remove, Update};
        self.snapshot
            .pending
            .iter()
            .filter(|pending| matches!(pending.change, Ok(Add | Update | Remove)))
            .count()
    }

    /// The sync was applied: the screen now says what it did with each job.
    pub fn show_applied(&mut self, done: Vec<Pending>) {
        // Nothing to show is a sync that had nothing to do by the time it
        // ran, or a jobs file that stopped being readable.
        if done.is_empty() {
            self.leave_sync();
            self.say("nothing was applied".to_owned());
            return;
        }
        self.applied = Some(done);
        self.sync_top = 0;
        self.screen = Screen::Sync;
    }

    /// Why a change cannot be made, as the rows the sync screen gives it: two
    /// at most. A reason ends in the file or the directory it is about, so
    /// one that does not fit loses its middle, not its end.
    pub fn reason_rows(&self, reason: &str) -> Vec<String> {
        let said = reason.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut rows = wrapped(&said, self.columns);
        if rows.len() > 2 {
            rows.truncate(1);
            let rest = said.strip_prefix(rows[0].as_str()).unwrap_or(&said);
            rows.push(tail(rest.trim_start(), self.columns));
        }
        rows
    }

    /// The rows the sync screen has to show: one for each job and those of
    /// each reason.
    pub fn sync_rows(&self) -> usize {
        self.changes()
            .iter()
            .map(|pending| match &pending.change {
                Ok(_) => 1,
                Err(reason) => 1 + self.reason_rows(reason).len(),
            })
            .sum()
    }

    /// The first row the sync screen shows.
    pub fn sync_top(&self) -> usize {
        self.sync_top.min(self.last_sync_top())
    }

    fn last_sync_top(&self) -> usize {
        self.sync_rows().saturating_sub(self.page)
    }

    fn preview(&mut self) {
        if !self.snapshot.can_sync {
            self.say("no scheduler on this system".to_owned());
        } else if !self.snapshot.pending.is_empty() {
            self.applied = None;
            self.sync_top = 0;
            self.screen = Screen::Sync;
        } else if self.snapshot.error.is_some() {
            self.say("fix the jobs file before syncing".to_owned());
        } else {
            self.say("nothing to apply".to_owned());
        }
    }

    fn ask_to_apply(&mut self) {
        // What is on screen was applied already: the next one starts from
        // a new preview.
        if self.applied.is_some() {
            return;
        }
        match self.applicable() {
            0 => self.say("nothing can be applied yet".to_owned()),
            changes => self.confirm = Some(Confirm::Apply { changes }),
        }
    }

    fn leave_sync(&mut self) {
        self.applied = None;
        self.sync_top = 0;
        if self.screen == Screen::Sync {
            self.screen = Screen::Jobs;
        }
    }

    /// The form could not be written. It stays, with the reason, unless the
    /// file `changed` under it: then what it was built on is gone, and so is
    /// it, and the list reads the file again.
    pub fn save_failed(&mut self, text: String, changed: bool) {
        let text = if changed {
            self.close_form();
            // What to do about it goes with what happened.
            "not saved: the jobs file changed; open the form again".to_owned()
        } else {
            text
        };
        self.notice = Some(Notice { text, error: true });
    }

    /// Text pasted into the terminal. It goes into the field in focus as
    /// text: a line break in it is not Enter, and a `y` is not an answer.
    pub fn paste(&mut self, text: &str) -> Vec<Effect> {
        if self.screen != Screen::Form || self.confirm.is_some() {
            return Vec::new();
        }
        let Some(form) = self.form.as_mut() else {
            return Vec::new();
        };
        let before = form.spec();
        let lines = form.focus == crate::tui::form::Focus::Args;
        // Windows ends a line with both; one of them is enough.
        for c in text.replace("\r\n", "\n").chars() {
            match c {
                '\n' | '\r' if lines => form.input(Edit::Insert('\n')),
                '\n' | '\r' | '\t' => form.input(Edit::Insert(' ')),
                c if c.is_control() => {}
                c => form.input(Edit::Insert(c)),
            }
        }
        self.changed_from(&before)
    }

    /// What follows from the job in the form no longer being `before`: the
    /// reason it could not be saved is about a job that is gone, and what to
    /// warn about is to be asked again.
    fn changed_from(&mut self, before: &JobSpec) -> Vec<Effect> {
        match self.form.as_ref().map(Form::spec) {
            Some(spec) if spec != *before => {
                self.reason = None;
                vec![Effect::Check { spec }]
            }
            _ => Vec::new(),
        }
    }

    fn close_form(&mut self) {
        self.form = None;
        self.warnings.clear();
        self.reason = None;
        if self.screen == Screen::Form {
            self.screen = self.behind_form;
        }
    }

    /// An action while the form is on screen.
    fn fill(&mut self, action: Action) -> Vec<Effect> {
        let Some(form) = self.form.as_mut() else {
            self.close_form();
            return Vec::new();
        };
        let before = form.spec();
        match action {
            Action::Input(edit) => form.input(edit),
            Action::Toggle => form.toggle(),
            Action::NextField => form.next_field(),
            Action::PrevField => form.prev_field(),
            // A job that exists and was not touched has nothing to save. A
            // new one is always tried, so an empty form says what it lacks.
            Action::Save if form.editing.is_some() && !form.dirty() => self.close_form(),
            Action::Save => match form.result() {
                Ok(text) => {
                    let spec = form.spec();
                    return vec![Effect::Save {
                        job: form.name.text().to_owned(),
                        read: form.read.clone(),
                        text,
                        prompt: spec.prompt,
                    }];
                }
                Err(error) => {
                    // The line is short: what is wrong goes first, without
                    // the words that only say the file was checked.
                    let causes: Vec<String> = error.chain().map(ToString::to_string).collect();
                    let said = match causes.split_first() {
                        Some((first, rest)) if first.contains("would not be valid") => rest,
                        _ => causes.as_slice(),
                    };
                    let said = said.join(": ");
                    // The field it is about, as far as the words tell.
                    use crate::tui::form::Focus;
                    let named = [
                        ("schedule.at", Focus::At),
                        ("schedule.days", Focus::Days),
                        ("workdir", Focus::Workdir),
                        ("prompt", Focus::Prompt),
                        ("job name", Focus::Name),
                        ("already exists", Focus::Name),
                    ];
                    if let Some((_, focus)) = named.iter().find(|(word, _)| said.contains(word)) {
                        form.focus = *focus;
                    }
                    self.reason = Some(said);
                }
            },
            // Leaving otto from the form would lose it without a word.
            Action::Back | Action::Quit => {
                if form.dirty() {
                    self.confirm = Some(Confirm::Discard);
                } else {
                    self.close_form();
                }
            }
            _ => {}
        }
        self.changed_from(&before)
    }

    /// The actions that change the jobs file for the selected job.
    fn manage(&mut self, action: Action) -> Vec<Effect> {
        if !matches!(self.screen, Screen::Jobs | Screen::Job) {
            return Vec::new();
        }
        let Some(selected) = self.selected() else {
            return Vec::new();
        };
        let job = selected.name.clone();
        let running = selected.running().is_some();
        match action {
            Action::EditJob => vec![Effect::OpenForm { job: Some(job) }],
            // Out of the file, its run could no longer be seen or stopped.
            Action::Delete if running => {
                self.say(format!("{job} is running: stop it before deleting it"));
                Vec::new()
            }
            Action::Delete => {
                self.confirm = Some(Confirm::Delete { job });
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The outcome of an effect: a failure is told to the user.
    pub fn done(&mut self, result: Result<(), String>) {
        if let Err(text) = result {
            self.notice = Some(Notice { text, error: true });
        }
    }

    /// The job and run whose output the screen needs read now: on entering the
    /// log screen, for as long as the run lasts, and once more after it ends.
    pub fn wants_log(&self) -> Option<(String, String)> {
        if self.screen != Screen::Log {
            return None;
        }
        let run = self.selected_run()?;
        let wanted = self.log.is_none() || self.log_is_partial || run.outcome == Outcome::Running;
        wanted.then(|| (self.job.clone().unwrap_or_default(), run.id.clone()))
    }

    pub fn show_log(&mut self, log: Result<Log, String>) {
        match log {
            Ok(log) => {
                self.log_is_partial = self
                    .selected_run()
                    .is_some_and(|run| run.outcome == Outcome::Running);
                self.log = Some(log);
            }
            Err(text) => {
                self.leave_log();
                self.screen = Screen::Job;
                self.notice = Some(Notice { text, error: true });
            }
        }
    }

    /// The first line of the log the screen shows.
    pub fn first_line(&self) -> usize {
        let last = self.last_top();
        if self.follow {
            last
        } else {
            self.top.min(last)
        }
    }

    /// The log as the rows the screen shows: a line longer than the screen
    /// is wide goes on in the next row.
    pub fn log_rows(&self) -> Vec<&str> {
        let text = self.log.as_ref().map_or("", |log| log.text.as_str());
        text.lines()
            .flat_map(|line| rows(line, self.columns))
            .collect()
    }

    /// The highest first line that still fills the page.
    fn last_top(&self) -> usize {
        self.log_rows().len().saturating_sub(self.page)
    }

    /// How far a page moves: the rows of the page, or on the list the jobs
    /// that fit in it.
    fn page_step(&self) -> isize {
        let step = match self.screen {
            Screen::Jobs => self.page / ENTRY,
            _ => self.page,
        };
        isize::try_from(step.max(1)).unwrap_or(isize::MAX)
    }

    fn job_index(&self) -> Option<usize> {
        let name = self.job.as_deref()?;
        self.snapshot.jobs.iter().position(|job| job.name == name)
    }

    fn newest_run(&self) -> Option<String> {
        let job = self.selected()?;
        job.runs.first().map(|run| run.id.clone())
    }

    fn say(&mut self, text: String) {
        self.notice = Some(Notice { text, error: false });
    }

    fn leave_log(&mut self) {
        self.log = None;
        self.log_is_partial = false;
        self.follow = true;
        self.top = 0;
    }

    /// Moves the selection, or scrolls the log, by `by` lines.
    fn step(&mut self, by: isize) {
        match self.screen {
            Screen::Jobs => {
                let names: Vec<&String> = self.snapshot.jobs.iter().map(|job| &job.name).collect();
                let at = self.job_index();
                self.job = moved(at, by, names.len()).map(|index| names[index].clone());
                self.run = None;
            }
            Screen::Job => {
                let Some(job) = self.selected() else {
                    return;
                };
                let at = job
                    .runs
                    .iter()
                    .position(|run| Some(&run.id) == self.run.as_ref());
                self.run = moved(at, by, job.runs.len()).map(|index| job.runs[index].id.clone());
            }
            Screen::Log => {
                let last = self.last_top();
                let top = self.first_line().saturating_add_signed(by).min(last);
                self.top = top;
                // Reaching the end by scrolling down is asking to follow again.
                self.follow = by > 0 && top == last;
            }
            Screen::Sync => {
                let top = self.sync_top().saturating_add_signed(by);
                self.sync_top = top.min(self.last_sync_top());
            }
            Screen::Help | Screen::Form => {}
        }
    }

    fn open(&mut self) {
        match self.screen {
            Screen::Jobs => {
                if self.selected().is_some() {
                    self.run = self.newest_run();
                    self.screen = Screen::Job;
                }
            }
            Screen::Job => match self.selected_run().map(|run| run.outcome) {
                None => self.say("no runs yet".to_owned()),
                // These two record a run that did not start the agent.
                Some(Outcome::Skipped | Outcome::Paused) => {
                    self.say("no output for this run".to_owned());
                }
                Some(_) => {
                    self.leave_log();
                    self.screen = Screen::Log;
                }
            },
            Screen::Log | Screen::Help | Screen::Form | Screen::Sync => {}
        }
    }

    fn back(&mut self) {
        match self.screen {
            Screen::Jobs => {}
            Screen::Job => self.screen = Screen::Jobs,
            Screen::Log => {
                self.leave_log();
                self.screen = Screen::Job;
            }
            Screen::Help => self.screen = self.behind_help,
            Screen::Form => self.close_form(),
            Screen::Sync => self.leave_sync(),
        }
    }

    /// The actions that act on the selected job. They work on the two
    /// screens whose shortcut bar offers them, and nowhere else.
    fn operate(&mut self, action: Action) -> Vec<Effect> {
        if !matches!(self.screen, Screen::Jobs | Screen::Job) {
            return Vec::new();
        }
        let Some(job) = self.selected() else {
            return Vec::new();
        };
        let name = job.name.clone();
        let state = job.state;
        let running = job.running().map(|run| run.pid);
        let set = |state| {
            vec![Effect::SetState {
                job: name.clone(),
                state,
            }]
        };
        match action {
            Action::RunNow if running.is_some() => self.say(format!("{name} is already running")),
            Action::RunNow => return vec![Effect::Start { job: name }],
            Action::Stop => match running {
                None => self.say(format!("{name} is not running")),
                Some(None) => self.say(format!("{name} has no process to stop")),
                Some(Some(pid)) => self.confirm = Some(Confirm::Stop { job: name, pid }),
            },
            Action::Pause => {
                return set(State {
                    paused: true,
                    ..state
                });
            }
            Action::Skip => {
                if state.paused {
                    self.say(format!(
                        "{name} is paused: the skip counts once it is resumed"
                    ));
                }
                return set(State {
                    skip_next: true,
                    ..state
                });
            }
            Action::Resume => return set(State::default()),
            Action::EditPrompt => {
                return vec![Effect::Edit {
                    path: job.job.prompt.clone(),
                }];
            }
            _ => {}
        }
        Vec::new()
    }
}

/// The index `by` places from `at` in a list of `len`, kept inside it. With
/// nothing selected the first place is taken.
fn moved(at: Option<usize>, by: isize, len: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(at.map_or(0, |at| at.saturating_add_signed(by).min(last)))
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::agent::Agent;
    use crate::config::{Job, Schedule, Weekday};
    use crate::store::{Outcome, Run, Trigger};
    use crate::tui::form::{Edit, Focus};
    use crate::tui::world::Pending;

    fn job(name: &str) -> JobView {
        JobView {
            name: name.to_owned(),
            job: Job {
                agent: Agent::Claude,
                prompt: PathBuf::from(format!("/prompts/{name}.md")),
                workdir: PathBuf::from("/work"),
                schedule: Schedule {
                    at: "16:05".to_owned(),
                    days: Weekday::every_day(),
                },
                args: Vec::new(),
            },
            state: State::default(),
            runs: Vec::new(),
            next: None,
        }
    }

    fn run(id: &str, outcome: Outcome) -> Run {
        let started: Timestamp = "2026-10-06T12:00:00Z".parse().unwrap();
        Run {
            id: id.to_owned(),
            started,
            finished: (outcome != Outcome::Running).then_some(started),
            trigger: Trigger::Manual,
            outcome,
            exit_code: None,
            pid: (outcome == Outcome::Running).then_some(77),
        }
    }

    fn snapshot(jobs: Vec<JobView>) -> Snapshot {
        Snapshot {
            jobs,
            ..Snapshot::default()
        }
    }

    fn three() -> Snapshot {
        snapshot(vec![job("alpha"), job("beta"), job("gamma")])
    }

    /// `alpha` with these runs, newest first.
    fn alpha_with(runs: Vec<Run>) -> Snapshot {
        snapshot(vec![JobView {
            runs,
            ..job("alpha")
        }])
    }

    fn log(text: &str) -> Log {
        Log {
            text: text.to_owned(),
            truncated: false,
        }
    }

    fn text(app: &App) -> &str {
        app.notice
            .as_ref()
            .map_or("", |notice| notice.text.as_str())
    }

    #[test]
    fn selection_starts_on_the_first_job_and_stops_at_the_ends() {
        let mut app = App::new(three());
        assert_eq!(app.job.as_deref(), Some("alpha"));
        app.act(Action::Up);
        assert_eq!(app.job.as_deref(), Some("alpha"));
        app.act(Action::Down);
        app.act(Action::Down);
        app.act(Action::Down);
        assert_eq!(app.job.as_deref(), Some("gamma"));
        app.act(Action::Top);
        assert_eq!(app.job.as_deref(), Some("alpha"));
        app.act(Action::Bottom);
        assert_eq!(app.job.as_deref(), Some("gamma"));
    }

    #[test]
    fn an_empty_list_selects_nothing_and_survives_every_action() {
        let mut app = App::new(Snapshot::default());
        assert_eq!(app.job, None);
        for action in [
            Action::Up,
            Action::Down,
            Action::Open,
            Action::RunNow,
            Action::Stop,
            Action::Pause,
            Action::Skip,
            Action::Resume,
            Action::EditPrompt,
        ] {
            assert_eq!(app.act(action), []);
        }
        assert_eq!(app.screen, Screen::Jobs);
    }

    #[test]
    fn selection_follows_the_job_by_name() {
        let mut app = App::new(three());
        app.act(Action::Down);
        app.refresh(snapshot(vec![
            job("aaa"),
            job("alpha"),
            job("beta"),
            job("gamma"),
        ]));
        assert_eq!(app.job.as_deref(), Some("beta"));
    }

    #[test]
    fn a_selected_job_that_vanishes_hands_the_selection_to_a_neighbour() {
        let mut app = App::new(three());
        app.act(Action::Down);
        app.refresh(snapshot(vec![job("alpha"), job("gamma")]));
        assert_eq!(app.job.as_deref(), Some("gamma"));
        app.refresh(snapshot(vec![job("alpha")]));
        assert_eq!(app.job.as_deref(), Some("alpha"));
        app.refresh(Snapshot::default());
        assert_eq!(app.job, None);
    }

    #[test]
    fn a_job_that_vanishes_closes_its_screens() {
        let mut jobs = three();
        jobs.jobs[1].runs = vec![run("r1", Outcome::Ok)];
        let mut app = App::new(jobs);
        app.act(Action::Down);
        app.act(Action::Open);
        app.act(Action::Open);
        app.show_log(Ok(log("output\n")));
        assert_eq!(app.screen, Screen::Log);

        app.refresh(snapshot(vec![job("alpha"), job("gamma")]));

        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(app.log, None);
        assert_eq!(text(&app), "beta is no longer in the jobs file");
    }

    #[test]
    fn a_run_that_was_pruned_closes_its_log() {
        let mut app = App::new(alpha_with(vec![
            run("r2", Outcome::Ok),
            run("r1", Outcome::Ok),
        ]));
        app.act(Action::Open);
        app.act(Action::Down);
        app.act(Action::Open);
        assert_eq!(app.run.as_deref(), Some("r1"));

        app.refresh(alpha_with(vec![
            run("r3", Outcome::Ok),
            run("r2", Outcome::Ok),
        ]));

        assert_eq!(app.screen, Screen::Job);
        assert_eq!(app.run.as_deref(), Some("r3"));
        assert_eq!(text(&app), "that run is no longer kept");
    }

    #[test]
    fn open_goes_in_and_back_comes_out() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Ok)]));
        assert_eq!(app.act(Action::Back), []);
        assert_eq!(app.screen, Screen::Jobs);
        app.act(Action::Open);
        assert_eq!(app.screen, Screen::Job);
        assert_eq!(app.run.as_deref(), Some("r1"));
        app.act(Action::Open);
        assert_eq!(app.screen, Screen::Log);
        app.act(Action::Back);
        assert_eq!(app.screen, Screen::Job);
        app.act(Action::Back);
        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(app.act(Action::Quit), [Effect::Quit]);
    }

    #[test]
    fn help_returns_to_the_screen_it_was_opened_from() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Ok)]));
        app.act(Action::Open);
        app.act(Action::Help);
        assert_eq!(app.screen, Screen::Help);
        app.act(Action::Back);
        assert_eq!(app.screen, Screen::Job);
    }

    #[test]
    fn a_run_that_started_no_agent_has_no_log_to_open() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Skipped)]));
        app.act(Action::Open);
        app.act(Action::Open);
        assert_eq!(app.screen, Screen::Job);
        assert_eq!(text(&app), "no output for this run");
    }

    #[test]
    fn a_job_that_never_ran_has_no_log_to_open() {
        let mut app = App::new(three());
        app.act(Action::Open);
        app.act(Action::Open);
        assert_eq!(app.screen, Screen::Job);
        assert_eq!(text(&app), "no runs yet");
    }

    #[test]
    fn a_page_on_the_list_is_as_many_jobs_as_fit() {
        let jobs = (0..20).map(|n| job(&format!("job-{n:02}"))).collect();
        let mut app = App::new(snapshot(jobs));
        // Nine rows hold three entries of three rows each.
        app.page = 9;
        app.act(Action::PageDown);
        assert_eq!(app.job.as_deref(), Some("job-03"));
        app.act(Action::PageUp);
        assert_eq!(app.job.as_deref(), Some("job-00"));
        // Too short for a whole entry: a page is still one job.
        app.page = 2;
        app.act(Action::PageDown);
        assert_eq!(app.job.as_deref(), Some("job-01"));
    }

    const FILE: &str = "[jobs.alpha]\nagent = \"claude\"\nprompt = \"alpha.md\"\nworkdir = \".\"\nschedule = { at = \"16:05\" }\n";

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.act(Action::Input(Edit::Insert(c)));
        }
    }

    /// The form for a new job, open over the three jobs.
    fn creating() -> App {
        let mut app = App::new(three());
        app.act(Action::New);
        app.open_form(Ok(FILE.to_owned()), None);
        app
    }

    /// A new job filled in well enough to be saved.
    fn filled() -> App {
        let mut app = creating();
        typed(&mut app, "nightly");
        on(&mut app, Focus::Workdir);
        typed(&mut app, "~/app");
        on(&mut app, Focus::At);
        typed(&mut app, "02:00");
        app
    }

    /// Puts the focus of the form on a field.
    fn on(app: &mut App, focus: Focus) {
        app.form.as_mut().unwrap().focus = focus;
    }

    #[test]
    fn new_asks_for_the_jobs_file_and_opens_an_empty_form() {
        let mut app = App::new(three());
        assert_eq!(app.act(Action::New), [Effect::OpenForm { job: None }]);
        assert_eq!(app.screen, Screen::Jobs);
        app.open_form(Ok(FILE.to_owned()), None);
        assert_eq!(app.screen, Screen::Form);
        let form = app.form.as_ref().unwrap();
        assert_eq!(form.editing, None);
        assert_eq!(form.read, FILE);
    }

    #[test]
    fn edit_opens_the_selected_job() {
        let mut app = App::new(three());
        assert_eq!(
            app.act(Action::EditJob),
            [Effect::OpenForm {
                job: Some("alpha".to_owned())
            }]
        );
        app.open_form(Ok(FILE.to_owned()), Some("alpha"));
        let form = app.form.as_ref().unwrap();
        assert_eq!(form.editing.as_deref(), Some("alpha"));
        assert_eq!(form.at.text(), "16:05");
        // With nothing selected there is nothing to edit or delete.
        let mut empty = App::new(Snapshot::default());
        assert_eq!(empty.act(Action::EditJob), []);
        assert_eq!(empty.act(Action::Delete), []);
        assert_eq!(empty.confirm, None);
    }

    #[test]
    fn a_form_that_cannot_open_is_a_notice() {
        let mut app = App::new(three());
        app.open_form(Err("cannot read jobs.toml".to_owned()), None);
        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(text(&app), "cannot read jobs.toml");
        app.open_form(Ok(FILE.to_owned()), Some("gone"));
        assert_eq!(app.screen, Screen::Jobs);
        assert!(text(&app).contains("no job named"));
        assert!(app.form.is_none());
    }

    #[test]
    fn typing_goes_to_the_focused_field_and_asks_what_to_warn_about() {
        let mut app = creating();
        let effects = app.act(Action::Input(Edit::Insert('n')));
        let form = app.form.as_ref().unwrap();
        assert_eq!(form.name.text(), "n");
        assert_eq!(effects, [Effect::Check { spec: form.spec() }]);
        // Moving the cursor changes nothing to check.
        assert_eq!(app.act(Action::Input(Edit::Left)), []);
    }

    #[test]
    fn the_fields_are_walked_and_the_choices_toggled() {
        let mut app = creating();
        app.act(Action::NextField);
        assert_eq!(app.form.as_ref().unwrap().focus, Focus::Agent);
        assert_eq!(app.act(Action::Toggle).len(), 1);
        assert_eq!(app.form.as_ref().unwrap().agent, Agent::Codex);
        app.act(Action::PrevField);
        assert_eq!(app.form.as_ref().unwrap().focus, Focus::Name);
    }

    #[test]
    fn save_with_a_bad_time_stays_on_the_form_with_the_reason() {
        let mut app = filled();
        typed(&mut app, "x");
        on(&mut app, Focus::Args);
        assert_eq!(app.act(Action::Save), []);
        assert_eq!(app.screen, Screen::Form);
        let reason = app.reason.clone().unwrap();
        assert!(reason.contains("HH:MM"), "{reason}");
        // The focus goes to the field the reason names.
        assert_eq!(app.form.as_ref().unwrap().focus, Focus::At);

        // It stays through moving about, and goes when the job changes.
        app.act(Action::NextField);
        app.act(Action::Input(Edit::Left));
        assert_eq!(app.reason.as_deref(), Some(reason.as_str()));
        on(&mut app, Focus::At);
        app.act(Action::Input(Edit::Backspace));
        assert_eq!(app.reason, None);
    }

    #[test]
    fn a_new_form_saved_empty_says_what_is_missing() {
        let mut app = creating();
        assert_eq!(app.act(Action::Save), []);
        assert_eq!(app.screen, Screen::Form);
        assert!(app.reason.as_deref().unwrap().contains("lowercase"));
        assert_eq!(app.form.as_ref().unwrap().focus, Focus::Name);

        let mut app = creating();
        typed(&mut app, "nightly");
        app.act(Action::Save);
        assert_eq!(app.reason.as_deref(), Some("workdir is empty"));
        assert_eq!(app.form.as_ref().unwrap().focus, Focus::Workdir);
    }

    #[test]
    fn save_returns_the_new_file_and_the_prompt_path() {
        let mut app = filled();
        let form = app.form.as_ref().unwrap();
        let expected = Effect::Save {
            job: "nightly".to_owned(),
            read: FILE.to_owned(),
            text: form.result().unwrap(),
            prompt: "prompts/nightly.md".to_owned(),
        };
        assert_eq!(app.act(Action::Save), [expected]);
        // The form stays until the file is written.
        assert_eq!(app.screen, Screen::Form);
        app.saved("nightly");
        assert_eq!(app.screen, Screen::Jobs);
        assert!(app.form.is_none());
        assert_eq!(app.job.as_deref(), Some("nightly"));
        // The list would show it with a next run: it is not scheduled yet.
        assert_eq!(text(&app), "nightly saved");
        assert!(!app.notice.unwrap().error);
    }

    #[test]
    fn a_deleted_job_still_has_its_unit_until_the_sync() {
        let mut app = App::new(three());
        app.deleted("alpha");
        assert_eq!(text(&app), "alpha deleted");
    }

    #[test]
    fn a_job_that_is_running_is_not_deleted() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Running)]));
        assert_eq!(app.act(Action::Delete), []);
        assert_eq!(app.confirm, None);
        assert_eq!(text(&app), "alpha is running: stop it before deleting it");
    }

    #[test]
    fn a_question_about_a_job_that_vanished_is_dropped() {
        let mut app = App::new(three());
        app.act(Action::Delete);
        app.refresh(snapshot(vec![job("beta"), job("gamma")]));
        assert_eq!(app.confirm, None);
    }

    #[test]
    fn the_form_of_a_job_that_vanished_closes_with_a_word() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Ok)]));
        app.act(Action::Open);
        app.open_form(Ok(FILE.to_owned()), Some("alpha"));
        app.refresh(snapshot(vec![job("beta")]));
        assert_eq!(app.screen, Screen::Jobs);
        assert!(app.form.is_none());
        assert_eq!(text(&app), "alpha is no longer in the jobs file");
        assert_eq!(app.job.as_deref(), Some("beta"));

        // A new job is nobody's to vanish.
        let mut app = creating();
        app.refresh(Snapshot::default());
        assert_eq!(app.screen, Screen::Form);
    }

    #[test]
    fn pasted_text_goes_into_the_field_and_nowhere_else() {
        let mut app = creating();
        on(&mut app, Focus::Workdir);
        let effects = app.paste("~/Work space/app\n--model y\r\nsonnet\u{7}");
        let form = app.form.as_ref().unwrap();
        // A line break is not a key: the focus stays, the breaks are spaces.
        assert_eq!(form.focus, Focus::Workdir);
        assert_eq!(form.workdir.text(), "~/Work space/app --model y sonnet");
        assert_eq!(effects, [Effect::Check { spec: form.spec() }]);

        // In the arguments a line is an argument.
        on(&mut app, Focus::Args);
        app.paste("--one\n--two");
        assert_eq!(app.form.as_ref().unwrap().spec().args, ["--one", "--two"]);

        // On a choice, and under a question, it is nothing.
        on(&mut app, Focus::Days);
        assert_eq!(app.paste("y y y"), []);
        assert_eq!(app.form.as_ref().unwrap().days, [false; 7]);
        app.act(Action::Back);
        assert_eq!(app.confirm, Some(Confirm::Discard));
        assert_eq!(app.paste("say yes"), []);
        assert_eq!(app.confirm, Some(Confirm::Discard));
        assert!(app.form.is_some());
        // And on the list it starts nothing.
        let mut list = App::new(three());
        assert_eq!(list.paste("rxd"), []);
        assert_eq!(list.confirm, None);
    }

    #[test]
    fn saving_an_untouched_job_only_closes_the_form() {
        let mut app = App::new(three());
        app.open_form(Ok(FILE.to_owned()), Some("alpha"));
        assert_eq!(app.act(Action::Save), []);
        assert_eq!(app.screen, Screen::Jobs);
        assert!(app.form.is_none());
    }

    #[test]
    fn leaving_a_changed_form_asks_first() {
        let mut app = filled();
        assert_eq!(app.act(Action::Back), []);
        assert_eq!(app.confirm, Some(Confirm::Discard));
        app.act(Action::No);
        assert_eq!(app.screen, Screen::Form);
        assert!(app.form.is_some());
        app.act(Action::Back);
        assert_eq!(app.act(Action::Yes), []);
        assert_eq!(app.screen, Screen::Jobs);
        assert!(app.form.is_none());
    }

    #[test]
    fn leaving_an_untouched_form_does_not_ask() {
        let mut app = creating();
        app.act(Action::Back);
        assert_eq!(app.confirm, None);
        assert_eq!(app.screen, Screen::Jobs);
        // Quitting from the form is leaving the form, not otto.
        let mut app = creating();
        assert_eq!(app.act(Action::Quit), []);
        assert_eq!(app.screen, Screen::Jobs);
    }

    #[test]
    fn the_form_goes_back_to_the_screen_it_was_opened_from() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Ok)]));
        app.act(Action::Open);
        app.act(Action::EditJob);
        app.open_form(Ok(FILE.to_owned()), Some("alpha"));
        app.act(Action::Back);
        assert_eq!(app.screen, Screen::Job);
    }

    #[test]
    fn delete_asks_first_and_names_the_job() {
        let mut app = App::new(three());
        assert_eq!(app.act(Action::Delete), []);
        assert_eq!(
            app.confirm,
            Some(Confirm::Delete {
                job: "alpha".to_owned()
            })
        );
        assert_eq!(
            app.act(Action::Yes),
            [Effect::Delete {
                job: "alpha".to_owned()
            }]
        );
        app.act(Action::Delete);
        assert_eq!(app.act(Action::No), []);
        assert_eq!(app.confirm, None);
    }

    #[test]
    fn a_save_that_failed_keeps_the_form_unless_the_file_changed() {
        let mut app = filled();
        app.save_failed("the disk is full".to_owned(), false);
        assert_eq!(app.screen, Screen::Form);
        assert_eq!(text(&app), "the disk is full");
        // A path that happens to hold those words is still only a path.
        app.save_failed(
            "cannot write /notes changed on disk/jobs.toml".to_owned(),
            false,
        );
        assert_eq!(app.screen, Screen::Form);

        // What the form was built on is gone: it closes, and the list reloads.
        app.save_failed(
            "the jobs file changed on disk since it was read".to_owned(),
            true,
        );
        assert_eq!(app.screen, Screen::Jobs);
        assert!(app.form.is_none());
        assert_eq!(
            text(&app),
            "not saved: the jobs file changed; open the form again"
        );
    }

    #[test]
    fn the_form_takes_no_job_action_and_survives_a_refresh() {
        let mut app = filled();
        for action in [Action::RunNow, Action::Stop, Action::Delete, Action::New] {
            assert_eq!(app.act(action), [], "{action:?}");
        }
        assert_eq!(app.confirm, None);
        app.refresh(snapshot(vec![job("beta")]));
        assert_eq!(app.screen, Screen::Form);
        assert_eq!(app.form.as_ref().unwrap().name.text(), "nightly");
    }

    #[test]
    fn warnings_are_kept_for_the_form_on_screen() {
        let mut app = creating();
        app.show_warnings(vec!["working directory not found: ~/app".to_owned()]);
        assert_eq!(app.warnings.len(), 1);
        app.act(Action::Back);
        assert!(app.warnings.is_empty());
    }

    #[test]
    fn run_now_starts_an_idle_job() {
        let mut app = App::new(three());
        assert_eq!(
            app.act(Action::RunNow),
            [Effect::Start {
                job: "alpha".to_owned()
            }]
        );
    }

    #[test]
    fn run_now_is_refused_while_the_job_runs() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Running)]));
        assert_eq!(app.act(Action::RunNow), []);
        assert_eq!(text(&app), "alpha is already running");
    }

    #[test]
    fn stop_asks_first() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Running)]));
        assert_eq!(app.act(Action::Stop), []);
        assert_eq!(
            app.confirm,
            Some(Confirm::Stop {
                job: "alpha".to_owned(),
                pid: 77
            })
        );
        assert_eq!(
            app.act(Action::Yes),
            [Effect::Stop {
                job: "alpha".to_owned(),
                pid: 77
            }]
        );
        assert_eq!(app.confirm, None);

        app.act(Action::Stop);
        assert_eq!(app.act(Action::No), []);
        assert_eq!(app.confirm, None);
    }

    #[test]
    fn a_confirmation_is_dropped_when_its_run_is_over() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Running)]));
        app.act(Action::Stop);
        assert!(app.confirm.is_some());
        app.refresh(alpha_with(vec![run("r1", Outcome::Ok)]));
        assert_eq!(app.confirm, None);
        assert_eq!(app.act(Action::Yes), []);
    }

    #[test]
    fn a_confirmation_is_dropped_when_its_job_vanishes() {
        let mut jobs = three();
        jobs.jobs[0].runs = vec![run("r1", Outcome::Running)];
        let mut app = App::new(jobs);
        app.act(Action::Stop);
        app.refresh(snapshot(vec![job("beta"), job("gamma")]));
        assert_eq!(app.confirm, None);
        assert_eq!(app.act(Action::Yes), []);
    }

    #[test]
    fn help_does_not_keep_the_log_of_a_job_that_vanished() {
        let mut jobs = three();
        jobs.jobs[1].runs = vec![run("r1", Outcome::Ok)];
        let mut app = App::new(jobs);
        app.act(Action::Down);
        app.act(Action::Open);
        app.act(Action::Open);
        app.show_log(Ok(log("beta output\n")));
        app.act(Action::Help);

        app.refresh(snapshot(vec![job("alpha"), job("gamma")]));
        app.act(Action::Back);

        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(app.log, None);
        assert_eq!(app.job.as_deref(), Some("gamma"));
    }

    #[test]
    fn a_job_is_operated_only_from_the_screens_that_offer_it() {
        let mut app = App::new(alpha_with(vec![run("r1", Outcome::Ok)]));
        app.act(Action::Open);
        app.act(Action::Open);
        assert_eq!(app.screen, Screen::Log);
        for action in [
            Action::RunNow,
            Action::Stop,
            Action::Pause,
            Action::Skip,
            Action::Resume,
            Action::EditPrompt,
        ] {
            assert_eq!(app.act(action), [], "{action:?} on the log");
        }
        app.act(Action::Help);
        assert_eq!(app.act(Action::RunNow), []);
    }

    #[test]
    fn a_skip_on_a_paused_job_says_when_it_counts() {
        let mut jobs = three();
        jobs.jobs[0].state = State {
            paused: true,
            skip_next: false,
        };
        let mut app = App::new(jobs);
        assert_eq!(app.act(Action::Skip).len(), 1);
        assert_eq!(
            text(&app),
            "alpha is paused: the skip counts once it is resumed"
        );
    }

    #[test]
    fn a_long_line_of_the_log_takes_several_rows() {
        let mut app = on_the_log(Outcome::Ok);
        app.columns = 10;
        app.page = 2;
        app.show_log(Ok(log("0123456789abcdef\n\nação\n")));
        assert_eq!(app.log_rows(), ["0123456789", "abcdef", "", "ação"]);
        // Four rows, two to a page: following starts at the third.
        assert_eq!(app.first_line(), 2);
    }

    #[test]
    fn stop_needs_a_run_in_progress() {
        let mut app = App::new(three());
        assert_eq!(app.act(Action::Stop), []);
        assert_eq!(app.confirm, None);
        assert_eq!(text(&app), "alpha is not running");
    }

    #[test]
    fn a_run_without_a_process_cannot_be_stopped() {
        let mut running = run("r1", Outcome::Running);
        running.pid = None;
        let mut app = App::new(alpha_with(vec![running]));
        assert_eq!(app.act(Action::Stop), []);
        assert_eq!(app.confirm, None);
        assert_eq!(text(&app), "alpha has no process to stop");
    }

    #[test]
    fn only_yes_and_no_answer_a_confirmation() {
        let mut jobs = three();
        jobs.jobs[0].runs = vec![run("r1", Outcome::Running)];
        let mut app = App::new(jobs);
        app.act(Action::Stop);
        let asked = app.confirm.clone();
        for action in [Action::Down, Action::RunNow, Action::Quit, Action::Open] {
            assert_eq!(app.act(action), []);
        }
        assert_eq!(app.confirm, asked);
        assert_eq!(app.job.as_deref(), Some("alpha"));
        assert_eq!(app.screen, Screen::Jobs);
    }

    #[test]
    fn pause_skip_and_resume_set_the_state() {
        let mut jobs = three();
        jobs.jobs[0].state = State {
            paused: false,
            skip_next: true,
        };
        let mut app = App::new(jobs);
        let set = |state| {
            vec![Effect::SetState {
                job: "alpha".to_owned(),
                state,
            }]
        };
        assert_eq!(
            app.act(Action::Pause),
            set(State {
                paused: true,
                skip_next: true
            })
        );
        assert_eq!(
            app.act(Action::Skip),
            set(State {
                paused: false,
                skip_next: true
            })
        );
        assert_eq!(app.act(Action::Resume), set(State::default()));
    }

    #[test]
    fn edit_prompt_opens_the_job_prompt() {
        let mut app = App::new(three());
        assert_eq!(
            app.act(Action::EditPrompt),
            [Effect::Edit {
                path: PathBuf::from("/prompts/alpha.md")
            }]
        );
    }

    #[test]
    fn a_failed_effect_becomes_a_notice_and_the_next_action_clears_it() {
        let mut app = App::new(three());
        app.done(Ok(()));
        assert_eq!(app.notice, None);
        app.done(Err("the disk is full".to_owned()));
        assert_eq!(
            app.notice,
            Some(Notice {
                text: "the disk is full".to_owned(),
                error: true
            })
        );
        app.act(Action::Down);
        assert_eq!(app.notice, None);
    }

    fn on_the_log(outcome: Outcome) -> App {
        let mut app = App::new(alpha_with(vec![run("r1", outcome)]));
        app.act(Action::Open);
        app.act(Action::Open);
        app
    }

    #[test]
    fn the_log_is_wanted_on_entering_and_while_the_run_lasts() {
        let wanted = Some(("alpha".to_owned(), "r1".to_owned()));
        let mut app = on_the_log(Outcome::Running);
        assert_eq!(app.wants_log(), wanted);
        app.show_log(Ok(log("one\n")));
        assert_eq!(app.wants_log(), wanted);

        // The run ends: one more load picks up its last lines.
        app.refresh(alpha_with(vec![run("r1", Outcome::Ok)]));
        assert_eq!(app.wants_log(), wanted);
        app.show_log(Ok(log("one\ntwo\n")));
        assert_eq!(app.wants_log(), None);
    }

    #[test]
    fn the_log_of_a_finished_run_is_loaded_once() {
        let mut app = on_the_log(Outcome::Ok);
        assert!(app.wants_log().is_some());
        app.show_log(Ok(log("one\n")));
        assert_eq!(app.wants_log(), None);
        assert_eq!(App::new(three()).wants_log(), None);
    }

    fn lines(count: usize) -> Log {
        log(&(0..count)
            .map(|line| format!("{line}\n"))
            .collect::<String>())
    }

    #[test]
    fn the_log_follows_the_end_until_the_user_scrolls_up() {
        let mut app = on_the_log(Outcome::Running);
        app.page = 10;
        app.show_log(Ok(lines(100)));
        assert!(app.follow);
        assert_eq!(app.first_line(), 90);

        app.act(Action::PageUp);
        assert!(!app.follow);
        assert_eq!(app.first_line(), 80);
        app.show_log(Ok(lines(200)));
        assert_eq!(app.first_line(), 80);

        app.act(Action::Bottom);
        assert!(app.follow);
        assert_eq!(app.first_line(), 190);
        app.act(Action::Top);
        assert_eq!(app.first_line(), 0);
        app.act(Action::Up);
        assert_eq!(app.first_line(), 0);
    }

    #[test]
    fn scrolling_down_to_the_end_follows_again() {
        let mut app = on_the_log(Outcome::Running);
        app.page = 10;
        app.show_log(Ok(lines(100)));
        app.act(Action::Up);
        assert_eq!(app.first_line(), 89);
        assert!(!app.follow);
        app.act(Action::Down);
        assert!(app.follow);
        app.act(Action::PageDown);
        assert_eq!(app.first_line(), 90);
    }

    #[test]
    fn a_log_shorter_than_the_screen_starts_at_the_top() {
        let mut app = on_the_log(Outcome::Ok);
        app.page = 10;
        app.show_log(Ok(lines(3)));
        assert_eq!(app.first_line(), 0);
        app.act(Action::PageUp);
        assert_eq!(app.first_line(), 0);
    }

    #[test]
    fn a_log_that_cannot_be_read_is_a_notice() {
        let mut app = on_the_log(Outcome::Ok);
        app.show_log(Err("no output for run r1 of alpha".to_owned()));
        assert_eq!(app.screen, Screen::Job);
        assert_eq!(app.log, None);
        assert_eq!(text(&app), "no output for run r1 of alpha");
    }

    fn pend(job: &str, change: Result<sync::Action, &str>) -> Pending {
        Pending {
            job: job.to_owned(),
            change: change.map_err(str::to_owned),
        }
    }

    /// The three jobs, with this still to apply.
    fn unsynced(pending: Vec<Pending>) -> Snapshot {
        Snapshot {
            pending,
            can_sync: true,
            ..three()
        }
    }

    /// One change that can be applied, one that waits and one that cannot.
    fn mixed() -> Vec<Pending> {
        vec![
            pend("alpha", Ok(sync::Action::Add)),
            pend("beta", Err("prompt file not found: /prompts/beta.md")),
            pend("gamma", Ok(sync::Action::Busy)),
        ]
    }

    fn previewing(pending: Vec<Pending>) -> App {
        let mut app = App::new(unsynced(pending));
        assert_eq!(app.act(Action::Sync), []);
        assert_eq!(app.screen, Screen::Sync);
        app
    }

    #[test]
    fn sync_opens_the_preview_of_what_is_pending() {
        let app = previewing(mixed());
        assert_eq!(app.changes(), mixed());
        assert_eq!(app.notice, None);
    }

    #[test]
    fn sync_with_nothing_pending_says_so() {
        let mut app = App::new(unsynced(Vec::new()));
        assert_eq!(app.act(Action::Sync), []);
        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(text(&app), "nothing to apply");
    }

    #[test]
    fn sync_without_a_scheduler_says_so() {
        let mut app = App::new(three());
        app.act(Action::Sync);
        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(text(&app), "no scheduler on this system");
    }

    #[test]
    fn a_jobs_file_that_cannot_be_read_is_not_synced() {
        let mut app = App::new(Snapshot {
            error: Some("jobs.toml: invalid".to_owned()),
            ..unsynced(Vec::new())
        });
        app.act(Action::Sync);
        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(text(&app), "fix the jobs file before syncing");
    }

    #[test]
    fn the_preview_opens_from_the_list_only() {
        let mut app = App::new(unsynced(mixed()));
        app.act(Action::Open);
        app.act(Action::Sync);
        assert_eq!(app.screen, Screen::Job);
    }

    #[test]
    fn apply_asks_first_and_counts_what_would_change() {
        let mut app = previewing(mixed());
        assert_eq!(app.act(Action::Apply), []);
        // The one that waits and the one that cannot are not counted.
        assert_eq!(app.confirm, Some(Confirm::Apply { changes: 1 }));
        assert_eq!(app.act(Action::No), []);
        assert_eq!(app.confirm, None);
        app.act(Action::Apply);
        assert_eq!(app.act(Action::Yes), [Effect::Apply]);
        assert_eq!(app.confirm, None);
    }

    #[test]
    fn with_nothing_that_can_be_applied_there_is_no_question() {
        let mut app = previewing(vec![
            pend("beta", Err("workdir not found")),
            pend("gamma", Ok(sync::Action::Busy)),
        ]);
        assert_eq!(app.act(Action::Apply), []);
        assert_eq!(app.confirm, None);
        assert_eq!(text(&app), "nothing can be applied yet");
    }

    #[test]
    fn apply_means_nothing_off_the_preview() {
        let mut app = App::new(unsynced(mixed()));
        assert_eq!(app.act(Action::Apply), []);
        assert_eq!(app.confirm, None);
    }

    #[test]
    fn the_keys_of_a_job_do_nothing_on_the_preview() {
        let mut app = previewing(mixed());
        for action in [
            Action::RunNow,
            Action::Pause,
            Action::Skip,
            Action::EditPrompt,
            Action::New,
            Action::EditJob,
            Action::Delete,
            Action::Open,
        ] {
            assert_eq!(app.act(action), [], "{action:?}");
            assert_eq!(app.screen, Screen::Sync);
            assert_eq!(app.confirm, None);
        }
    }

    #[test]
    fn what_was_applied_stays_until_the_screen_is_left() {
        let mut app = previewing(mixed());
        let done = vec![
            pend("alpha", Ok(sync::Action::Add)),
            pend("beta", Err("prompt file not found: /prompts/beta.md")),
        ];
        app.show_applied(done.clone());
        // The next look finds less to do; the screen still says what was done.
        app.refresh(unsynced(vec![pend("beta", Err("prompt file not found"))]));
        assert_eq!(app.screen, Screen::Sync);
        assert_eq!(app.changes(), done);
        assert!(app.applied());
        // It was applied: asking again is for the next preview.
        assert_eq!(app.act(Action::Apply), []);
        assert_eq!(app.confirm, None);
        app.act(Action::Back);
        assert_eq!(app.screen, Screen::Jobs);
        assert!(!app.applied());
        app.act(Action::Sync);
        assert_eq!(app.changes().len(), 1);
    }

    #[test]
    fn an_apply_that_did_nothing_goes_back_and_says_so() {
        let mut app = previewing(mixed());
        app.show_applied(Vec::new());
        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(text(&app), "nothing was applied");
    }

    #[test]
    fn the_preview_closes_when_nothing_is_left_to_apply() {
        let mut app = previewing(mixed());
        app.refresh(unsynced(Vec::new()));
        assert_eq!(app.screen, Screen::Jobs);
        assert_eq!(text(&app), "nothing left to apply");
    }

    #[test]
    fn the_question_goes_when_nothing_could_be_applied_any_more() {
        let mut app = previewing(mixed());
        app.act(Action::Apply);
        app.refresh(unsynced(vec![pend("beta", Err("workdir not found"))]));
        assert_eq!(app.confirm, None);
        assert_eq!(app.screen, Screen::Sync);
        // And it keeps its count right while there is something.
        let mut app = previewing(mixed());
        app.act(Action::Apply);
        app.refresh(unsynced(vec![
            pend("alpha", Ok(sync::Action::Add)),
            pend("old", Ok(sync::Action::Remove)),
        ]));
        assert_eq!(app.confirm, Some(Confirm::Apply { changes: 2 }));
    }

    #[test]
    fn the_help_goes_back_to_the_preview() {
        let mut app = previewing(mixed());
        app.act(Action::Help);
        app.refresh(unsynced(mixed()));
        app.act(Action::Back);
        assert_eq!(app.screen, Screen::Sync);
    }

    #[test]
    fn a_long_preview_scrolls_and_stops_at_its_end() {
        let many: Vec<Pending> = (0..10)
            .map(|n| pend(&format!("job-{n}"), Ok(sync::Action::Add)))
            .collect();
        let mut app = previewing(many);
        app.page = 4;
        assert_eq!(app.sync_top(), 0);
        app.act(Action::Down);
        assert_eq!(app.sync_top(), 1);
        app.act(Action::Bottom);
        assert_eq!(app.sync_top(), 6);
        app.act(Action::Down);
        assert_eq!(app.sync_top(), 6);
        app.act(Action::Top);
        assert_eq!(app.sync_top(), 0);
    }

    #[test]
    fn a_reason_takes_its_own_rows_and_never_more_than_two() {
        let mut app = previewing(vec![pend(
            "beta",
            Err("one two three four five six seven eight nine ten eleven twelve"),
        )]);
        app.columns = 24;
        assert_eq!(
            app.reason_rows("prompt file not found"),
            ["prompt file not found"]
        );
        let rows = app.reason_rows("one two three four five six seven eight nine ten eleven");
        assert_eq!(rows.len(), 2);
        // What it ends in is what it is about: the end is what is kept.
        assert!(rows[1].starts_with('…'), "{rows:?}");
        assert!(rows[1].ends_with("ten eleven"), "{rows:?}");
        assert!(rows.iter().all(|row| crate::tui::text::width(row) <= 24));
        // One row for the job and two for why.
        assert_eq!(app.sync_rows(), 3);
    }
}
