//! The terminal UI: `otto` with no subcommand.

pub mod app;
pub mod keys;
pub mod view;
pub mod world;

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};

use app::{App, Effect};
use world::World;

/// How often the UI takes a new picture of the machine.
const TICK: Duration = Duration::from_secs(1);

/// Runs the UI until the user quits. The terminal is given back on every way
/// out, a panic included.
pub fn run(world: &mut dyn World, now: &dyn Fn() -> Timestamp, zone: &TimeZone) -> Result<()> {
    let mut terminal = ratatui::try_init().context("cannot take over the terminal")?;
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
            match effect {
                Effect::Quit => return Ok(()),
                Effect::Edit { path } => {
                    // The editor needs the terminal as the shell left it,
                    // cursor included: drawing hides it.
                    terminal.show_cursor().context(cannot_draw)?;
                    ratatui::restore();
                    let edited = world.edit(&path);
                    *terminal = ratatui::try_init().context("cannot take over the terminal")?;
                    terminal.clear().context(cannot_draw)?;
                    app.done(edited.map_err(|error| format!("{error:#}")));
                }
                other => app.done(perform(&other, world)),
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

/// Carries out an effect that does not need the terminal. A failure comes back
/// as the text the screen shows.
fn perform(effect: &Effect, world: &mut dyn World) -> Result<(), String> {
    let done = match effect {
        Effect::SetState { job, state } => world.set_state(job, *state),
        Effect::Start { job } => world.start(job),
        Effect::Stop { job, pid } => world.stop(job, *pid),
        // These two belong to the loop, which owns the terminal.
        Effect::Edit { .. } | Effect::Quit => Ok(()),
    };
    done.map_err(|error| format!("{error:#}"))
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
            assert_eq!(perform(&effect, &mut world), Ok(()));
        }
        assert_eq!(
            *world.calls.borrow(),
            ["start report", "stop report 77", "set_state report paused"]
        );
    }

    #[test]
    fn a_failing_effect_is_text_for_the_screen() {
        let mut world = world();
        world.fail = Some("process 77 does not lead its process group".to_owned());
        assert_eq!(
            perform(
                &Effect::Stop {
                    job: "report".to_owned(),
                    pid: 77
                },
                &mut world
            ),
            Err("process 77 does not lead its process group".to_owned())
        );
    }
}
