//! The terminal UI: `otto` with no subcommand.

pub mod app;
pub mod form;
pub mod keys;
pub mod text;
pub mod view;
pub mod world;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::jobs_file;
use app::{App, Effect};
use world::World;

/// How often the UI takes a new picture of the machine.
const TICK: Duration = Duration::from_secs(1);

/// Runs the UI until the user quits. The terminal is given back on every way
/// out, a panic included.
pub fn run(world: &mut dyn World, now: &dyn Fn() -> Timestamp, zone: &TimeZone) -> Result<()> {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            // It may have got as far as raw mode before failing.
            ratatui::restore();
            return Err(error).context("cannot take over the terminal");
        }
    };
    let result = watch(&mut terminal, world, now, zone);
    ratatui::restore();
    result
}

fn watch(
    terminal: &mut DefaultTerminal,
    world: &mut dyn World,
    now: &dyn Fn() -> Timestamp,
    zone: &TimeZone,
) -> Result<()> {
    let cannot_draw = "cannot draw on the terminal";
    let mut app = App::new(world.snapshot(now()));
    let mut next_tick = Instant::now() + TICK;
    loop {
        let size = terminal.size().context(cannot_draw)?;
        app.page = view::page(size.height);
        app.columns = view::columns(size.width);
        let clock = now().to_zoned(zone.clone());
        terminal
            .draw(|frame| view::draw(frame, &app, &clock))
            .context(cannot_draw)?;

        let wait = next_tick.saturating_duration_since(Instant::now());
        if !event::poll(wait).context("cannot read the terminal")? {
            tick(&mut app, world, now());
            next_tick = Instant::now() + TICK;
            continue;
        }
        // Anything but a key, a resize for one, only needs the redraw.
        let Event::Key(key) = event::read().context("cannot read the terminal")? else {
            continue;
        };
        let Some(action) = keys::action(key, &app) else {
            continue;
        };
        let effects = app.act(action);
        let acted = !effects.is_empty();
        for effect in effects {
            if effect == Effect::Quit {
                return Ok(());
            }
            if let Some(path) = perform(effect, &mut app, world) {
                suspend(terminal).context(cannot_draw)?;
                let edited = world.edit(&path);
                resume(terminal).context("cannot take over the terminal")?;
                app.done(edited.map_err(|error| format!("{error:#}")));
            }
        }
        // What an effect changed shows at once, and so does a log just opened;
        // moving the selection does not go to the disk.
        if acted {
            tick(&mut app, world, now());
        } else if app.log.is_none() {
            load_log(&mut app, world);
        }
    }
}

/// Hands the terminal to another program as the shell left it: cursor shown
/// (drawing hides it), cooked mode, the screen the user had before otto.
fn suspend(terminal: &mut DefaultTerminal) -> io::Result<()> {
    terminal.show_cursor()?;
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)
}

/// Takes the terminal back after `suspend`. Unlike a second `ratatui::init`,
/// this does not install the panic hook again on top of itself.
fn resume(terminal: &mut DefaultTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()
}

/// Carries out an effect and tells the app how it went. Opening the editor
/// needs the terminal, which the loop owns: an effect that ends in the
/// editor gives back the file to open.
fn perform(effect: Effect, app: &mut App, world: &mut dyn World) -> Option<PathBuf> {
    let told = |error: anyhow::Error| format!("{error:#}");
    match effect {
        Effect::SetState { job, state } => app.done(world.set_state(&job, state).map_err(told)),
        Effect::Start { job } => app.done(world.start(&job).map_err(told)),
        Effect::Stop { job, pid } => app.done(world.stop(&job, pid).map_err(told)),
        Effect::Edit { path } => return Some(path),
        // The loop ends on this one before it gets here.
        Effect::Quit => {}
        Effect::OpenForm { job } => app.open_form(world.text().map_err(told), job.as_deref()),
        Effect::Check { spec } => app.show_warnings(world.warnings(&spec)),
        Effect::Save {
            job,
            read,
            text,
            prompt,
        } => match world.save(&read, &text) {
            Err(error) => app.save_failed(told(error)),
            Ok(()) => {
                app.saved(&job);
                // A job with no prompt file would fail its first run: one
                // that is missing is created and handed to the editor.
                match world.ensure_prompt(&prompt) {
                    Ok(created) => return created,
                    Err(error) => app.done(Err(told(error))),
                }
            }
        },
        // From the file as it is now, not as it was when the list was read.
        Effect::Delete { job } => {
            let removed = world.text().and_then(|read| {
                let text = jobs_file::remove(&read, &job)?;
                world.save(&read, &text)
            });
            app.done(removed.map_err(told));
        }
    }
    None
}

/// One tick: a new picture of the machine and, if the screen needs it, the log.
fn tick(app: &mut App, world: &mut dyn World, now: Timestamp) {
    app.refresh(world.snapshot(now));
    load_log(app, world);
}

fn load_log(app: &mut App, world: &dyn World) {
    if let Some((job, run)) = app.wants_log() {
        app.show_log(world.log(&job, &run).map_err(|error| format!("{error:#}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::config::{Job, Schedule, Weekday};
    use crate::store::{Outcome, Run, State, Trigger};
    use crate::tui::app::Action;
    use crate::tui::world::{Fake, JobView, Log, Snapshot};

    fn now() -> Timestamp {
        "2026-10-06T12:00:00Z".parse().unwrap()
    }

    /// A world with one job, `report`, that has one finished run, `r1`.
    fn world() -> Fake {
        let job = JobView {
            name: "report".to_owned(),
            job: Job {
                agent: Agent::Claude,
                prompt: "/prompts/report.md".into(),
                workdir: "/work".into(),
                schedule: Schedule {
                    at: "16:05".to_owned(),
                    days: Weekday::every_day(),
                },
                args: Vec::new(),
            },
            state: State::default(),
            runs: vec![Run {
                id: "r1".to_owned(),
                started: now(),
                finished: Some(now()),
                trigger: Trigger::Manual,
                outcome: Outcome::Ok,
                exit_code: Some(0),
                pid: None,
            }],
            next: None,
        };
        Fake {
            snapshot: Snapshot {
                jobs: vec![job],
                ..Snapshot::default()
            },
            log: Log {
                text: "done\n".to_owned(),
                truncated: false,
            },
            ..Fake::default()
        }
    }

    #[test]
    fn a_tick_refreshes_and_loads_the_log_the_screen_wants() {
        let mut world = world();
        let mut app = App::new(Snapshot::default());
        tick(&mut app, &mut world, now());
        assert_eq!(app.job.as_deref(), Some("report"));
        assert_eq!(*world.calls.borrow(), ["snapshot"]);

        app.act(Action::Open);
        app.act(Action::Open);
        tick(&mut app, &mut world, now());
        assert_eq!(
            app.log.as_ref().map(|log| log.text.as_str()),
            Some("done\n")
        );
        // A finished run is read once.
        tick(&mut app, &mut world, now());
        assert_eq!(
            *world.calls.borrow(),
            ["snapshot", "snapshot", "log report r1", "snapshot"]
        );
    }

    #[test]
    fn a_log_that_cannot_be_read_closes_its_screen() {
        let mut world = world();
        let mut app = App::new(world.snapshot.clone());
        app.act(Action::Open);
        app.act(Action::Open);
        world.fail = Some("no output for run r1 of report".to_owned());
        tick(&mut app, &mut world, now());
        assert_eq!(app.screen, app::Screen::Job);
        assert_eq!(
            app.notice.map(|notice| notice.text),
            Some("no output for run r1 of report".to_owned())
        );
    }

    #[test]
    fn an_effect_reaches_the_world() {
        let mut world = world();
        let mut app = App::new(world.snapshot.clone());
        let state = State {
            paused: true,
            skip_next: false,
        };
        for effect in [
            Effect::Start {
                job: "report".to_owned(),
            },
            Effect::Stop {
                job: "report".to_owned(),
                pid: 77,
            },
            Effect::SetState {
                job: "report".to_owned(),
                state,
            },
        ] {
            assert_eq!(perform(effect, &mut app, &mut world), None);
        }
        assert_eq!(app.notice, None);
        assert_eq!(
            *world.calls.borrow(),
            ["start report", "stop report 77", "set_state report paused"]
        );
    }

    #[test]
    fn a_failing_effect_is_text_for_the_screen() {
        let mut world = world();
        let mut app = App::new(world.snapshot.clone());
        world.fail = Some("process 77 does not lead its process group".to_owned());
        let stop = Effect::Stop {
            job: "report".to_owned(),
            pid: 77,
        };
        assert_eq!(perform(stop, &mut app, &mut world), None);
        assert_eq!(
            app.notice.map(|notice| notice.text),
            Some("process 77 does not lead its process group".to_owned())
        );
    }

    #[test]
    fn editing_the_prompt_is_a_file_for_the_editor() {
        let mut world = world();
        let mut app = App::new(world.snapshot.clone());
        let edit = Effect::Edit {
            path: "/prompts/report.md".into(),
        };
        assert_eq!(
            perform(edit, &mut app, &mut world),
            Some(PathBuf::from("/prompts/report.md"))
        );
        assert!(world.calls.borrow().is_empty());
    }

    const FILE: &str = "[jobs.report]\nagent = \"claude\"\nprompt = \"report.md\"\nworkdir = \".\"\nschedule = { at = \"16:05\" }\n";

    #[test]
    fn opening_the_form_reads_the_jobs_file() {
        let mut world = world();
        world.text = FILE.to_owned();
        let mut app = App::new(world.snapshot.clone());
        let open = Effect::OpenForm {
            job: Some("report".to_owned()),
        };
        assert_eq!(perform(open, &mut app, &mut world), None);
        assert_eq!(app.screen, app::Screen::Form);
        assert_eq!(app.form.as_ref().map(|form| form.read.as_str()), Some(FILE));
        assert_eq!(*world.calls.borrow(), ["text"]);
    }

    #[test]
    fn checking_a_job_brings_its_warnings_to_the_screen() {
        let mut world = world();
        world.text = FILE.to_owned();
        world.warnings = vec!["codex not found in PATH".to_owned()];
        let mut app = App::new(world.snapshot.clone());
        perform(Effect::OpenForm { job: None }, &mut app, &mut world);
        let spec = app.form.as_ref().unwrap().spec();
        perform(Effect::Check { spec }, &mut app, &mut world);
        assert_eq!(app.warnings, ["codex not found in PATH"]);
    }

    fn save() -> Effect {
        Effect::Save {
            job: "nightly".to_owned(),
            read: FILE.to_owned(),
            text: "new text".to_owned(),
            prompt: "prompts/nightly.md".to_owned(),
        }
    }

    /// The form for a new job, open on the screen.
    fn with_the_form(world: &mut Fake) -> App {
        world.text = FILE.to_owned();
        let mut app = App::new(world.snapshot.clone());
        perform(Effect::OpenForm { job: None }, &mut app, world);
        world.calls.borrow_mut().clear();
        app
    }

    #[test]
    fn saving_writes_the_file_then_sees_to_the_prompt() {
        let mut world = world();
        let mut app = with_the_form(&mut world);
        // The prompt file was already there: nothing to open.
        assert_eq!(perform(save(), &mut app, &mut world), None);
        assert_eq!(
            *world.calls.borrow(),
            ["save new text", "ensure_prompt prompts/nightly.md"]
        );
        assert_eq!(app.screen, app::Screen::Jobs);
        assert_eq!(app.job.as_deref(), Some("nightly"));
    }

    #[test]
    fn a_prompt_file_just_created_opens_in_the_editor() {
        let mut world = world();
        world.created = Some("/etc/otto/prompts/nightly.md".into());
        let mut app = with_the_form(&mut world);
        assert_eq!(
            perform(save(), &mut app, &mut world),
            Some(PathBuf::from("/etc/otto/prompts/nightly.md"))
        );
    }

    #[test]
    fn a_save_that_fails_leaves_the_form_and_the_prompt_alone() {
        let mut world = world();
        let mut app = with_the_form(&mut world);
        world.fail = Some("the disk is full".to_owned());
        assert_eq!(perform(save(), &mut app, &mut world), None);
        assert_eq!(*world.calls.borrow(), ["save new text"]);
        assert_eq!(app.screen, app::Screen::Form);
        assert_eq!(
            app.notice.map(|notice| notice.text),
            Some("the disk is full".to_owned())
        );
    }

    #[test]
    fn deleting_takes_the_job_out_of_the_file_as_it_is_now() {
        let mut world = world();
        world.text = FILE.to_owned();
        let mut app = App::new(world.snapshot.clone());
        let delete = Effect::Delete {
            job: "report".to_owned(),
        };
        assert_eq!(perform(delete, &mut app, &mut world), None);
        assert_eq!(*world.calls.borrow(), ["text", "save "]);
        assert_eq!(app.notice, None);

        // A job the file does not have is told, and nothing is written.
        world.calls.borrow_mut().clear();
        let gone = Effect::Delete {
            job: "gone".to_owned(),
        };
        perform(gone, &mut app, &mut world);
        assert_eq!(*world.calls.borrow(), ["text"]);
        assert!(app.notice.unwrap().text.contains("no job named"));
    }
}
