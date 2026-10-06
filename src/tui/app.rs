//! The state of the terminal UI. Actions come in, effects go out: nothing here
//! reads a file, the clock or the environment.

use std::path::PathBuf;

use crate::store::{Outcome, Run, State};
use crate::tui::text::rows;
use crate::tui::world::{JobView, Log, Snapshot};

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
}

/// Something to do outside the UI's own state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    SetState { job: String, state: State },
    Start { job: String },
    Stop { job: String, pid: u32 },
    Edit { path: PathBuf },
    Quit,
}

/// A question the UI is waiting on before it acts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    Stop { job: String, pid: u32 },
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
    /// Where `Back` leaves the help screen.
    behind_help: Screen,
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
            behind_help: Screen::Jobs,
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
        if let Some(Confirm::Stop { job, pid }) = &self.confirm {
            let still_running = self.snapshot.jobs.iter().any(|view| {
                view.name == *job && view.running().is_some_and(|run| run.pid == Some(*pid))
            });
            if !still_running {
                self.confirm = None;
            }
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
            Action::RunNow
            | Action::Stop
            | Action::Pause
            | Action::Skip
            | Action::Resume
            | Action::EditPrompt => return self.operate(action),
        }
        Vec::new()
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
            Screen::Help => {}
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
            Screen::Log | Screen::Help => {}
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
}
