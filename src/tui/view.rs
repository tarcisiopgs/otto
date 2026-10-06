//! The drawing of the terminal UI: a function of the state and of the current
//! time, and of nothing else. The look is described in `DESIGN.md`.

use jiff::Zoned;
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::config::{Schedule, Weekday};
use crate::next::{self, Next};
use crate::store::{Outcome, Run};
use crate::tui::app::{App, Confirm, Screen};
use crate::tui::world::JobView;

/// The smallest terminal the UI is drawn on: columns, then rows.
pub const MIN: (u16, u16) = (60, 12);

/// How many of a job's latest runs its strip of marks shows.
const STRIP: usize = 8;

/// The rows a job takes on the list: two lines and the rule that closes it.
const ENTRY: usize = 3;

/// The rows left for what scrolls on a terminal `height` rows tall: all but
/// the header, the line under it, the notice and the shortcut bar.
pub fn page(height: u16) -> usize {
    usize::from(height.saturating_sub(4))
}

pub fn draw(frame: &mut Frame, app: &App, now: &Zoned) {
    let area = frame.area();
    if area.width < MIN.0 || area.height < MIN.1 {
        let message = format!("terminal too small: otto needs {} × {}", MIN.0, MIN.1);
        frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: true }), area);
        return;
    }
    let width = usize::from(area.width);
    let rows = page(area.height);
    let mut body = match app.screen {
        Screen::Jobs => jobs(app, now, width, rows),
        Screen::Job => job(app, now, width, rows),
        Screen::Log => log(app, rows),
        Screen::Help => help(),
    };
    body.resize(rows, Line::default());

    let mut lines = vec![header(app, now, width)];
    lines.push(match app.screen {
        Screen::Log => Line::styled("─".repeat(width), dim()),
        _ => Line::default(),
    });
    lines.extend(body);
    lines.push(notice(app));
    lines.push(bar(app, width));
    frame.render_widget(Paragraph::new(lines), area);
}

/// The one accent: what is selected, what is running, what waits on the user.
fn accent() -> Style {
    Style::new().fg(Color::Yellow)
}

/// A failure or an error, and nothing else.
fn bad() -> Style {
    Style::new().fg(Color::Red)
}

/// Rules and secondary facts.
fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn strong() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// `left` at the left edge and `right` at the right one, a column in from each.
fn between<'a>(left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: usize) -> Line<'a> {
    let gap = width
        .saturating_sub(width_of(&left) + width_of(&right) + 1)
        .max(1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

/// `text` in exactly `columns` columns: padded, or cut with an ellipsis.
fn fit(text: &str, columns: usize) -> String {
    if text.chars().count() <= columns {
        return format!("{text:<columns$}");
    }
    let kept: String = text.chars().take(columns.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// The end of `text` when it is longer than `columns`: a path says the most
/// in its last parts.
fn tail(text: &str, columns: usize) -> String {
    let length = text.chars().count();
    if length <= columns {
        return text.to_owned();
    }
    let kept: String = text.chars().skip(length + 1 - columns.max(1)).collect();
    format!("…{kept}")
}

/// `text` over as many lines of `columns` as it needs.
fn wrapped(text: &str, columns: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(columns.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn header<'a>(app: &'a App, now: &Zoned, width: usize) -> Line<'a> {
    let job = app.job.as_deref().unwrap_or_default();
    let run = app.selected_run();
    let place = match app.screen {
        Screen::Jobs => "jobs".to_owned(),
        Screen::Job => job.to_owned(),
        Screen::Log => match run {
            Some(run) => format!("{job} · {}", started(run, now)),
            None => job.to_owned(),
        },
        Screen::Help => "help".to_owned(),
    };
    let left = vec![
        Span::styled(" otto", strong()),
        Span::styled(" · ", dim()),
        Span::raw(place),
    ];
    let right = match (app.screen, run) {
        (Screen::Log, Some(run)) => {
            let mut spans = vec![mark(run), Span::raw(" "), Span::raw(run.outcome_label())];
            if let Some(duration) = run.duration_label() {
                spans.push(Span::styled(format!(" · {duration}"), dim()));
            }
            spans
        }
        _ => {
            let clock = now.strftime("%a %-d %b  %H:%M").to_string().to_lowercase();
            vec![Span::styled(clock, dim())]
        }
    };
    between(left, right, width)
}

/// The mark a run leaves in the logbook. Each outcome has a glyph of its own,
/// so the strip reads the same without colour.
fn mark(run: &Run) -> Span<'static> {
    match run.outcome {
        Outcome::Running => Span::styled("●", accent()),
        Outcome::Ok => Span::raw("✓"),
        Outcome::Failed => Span::styled("✗", bad()),
        Outcome::Interrupted => Span::styled("!", bad()),
        // A scheduled run that did not start the agent.
        Outcome::Skipped | Outcome::Paused => Span::styled("·", dim()),
    }
}

/// When a run started, on the clock of the terminal.
fn started(run: &Run, now: &Zoned) -> String {
    run.started
        .to_zoned(now.time_zone().clone())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// `16:05 mon–fri`, `07:00 daily`, `09:00 mon,thu`.
fn schedule(schedule: &Schedule) -> String {
    use Weekday::{Fri, Mon, Sat, Sun, Thu, Tue, Wed};
    const WEEK: [(Weekday, &str); 7] = [
        (Mon, "mon"),
        (Tue, "tue"),
        (Wed, "wed"),
        (Thu, "thu"),
        (Fri, "fri"),
        (Sat, "sat"),
        (Sun, "sun"),
    ];
    let on: Vec<&str> = WEEK
        .iter()
        .filter(|(day, _)| schedule.days.contains(day))
        .map(|(_, name)| *name)
        .collect();
    let days = match on.as_slice() {
        [_, _, _, _, _, _, _] => "daily".to_owned(),
        ["mon", "tue", "wed", "thu", "fri"] => "mon–fri".to_owned(),
        _ => on.join(","),
    };
    format!("{} {days}", schedule.at)
}

/// The coming runs in words. `lead` goes before the time of a plain next run.
fn coming(next: Option<&Next>, now: &Zoned, lead: &str) -> String {
    match next {
        Some(Next::At(when)) => format!("{lead}{}", next::label(when, now)),
        Some(Next::Skipping { skipped, then }) => format!(
            "skips {}, then {}",
            next::label(skipped, now),
            next::label(then, now)
        ),
        Some(Next::Paused) | None => "no next run".to_owned(),
    }
}

fn state(job: &JobView) -> Span<'static> {
    match job.status() {
        "running" => Span::styled("● running", accent()),
        "skip next" => Span::styled("skip next", accent()),
        "paused" => Span::styled("paused", strong()),
        other => Span::styled(other, dim()),
    }
}

/// How the latest run ended, `last ok · 2m 14s`, or since when it is running.
fn last(job: &JobView, now: &Zoned) -> Vec<Span<'static>> {
    let Some(run) = job.runs.first() else {
        return vec![Span::styled("never ran", dim())];
    };
    if run.outcome == Outcome::Running {
        let since = run
            .started
            .to_zoned(now.time_zone().clone())
            .strftime("%H:%M");
        return vec![Span::styled(format!("running since {since}"), accent())];
    }
    let style = match run.outcome {
        Outcome::Failed | Outcome::Interrupted => bad(),
        _ => Style::new(),
    };
    let mut spans = vec![
        Span::styled("last ", dim()),
        Span::styled(run.outcome_label(), style),
    ];
    if let Some(duration) = run.duration_label() {
        spans.push(Span::styled(format!(" · {duration}"), dim()));
    }
    spans
}

/// The latest runs as marks, oldest first, so the newest sits at the right.
fn strip(job: &JobView) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for run in job.runs.iter().take(STRIP).rev() {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(mark(run));
    }
    spans
}

/// The first of `len` rows to draw so that row `selected` is among `visible`.
fn first_visible(selected: usize, visible: usize) -> usize {
    (selected + 1).saturating_sub(visible.max(1))
}

fn rule(width: usize) -> Line<'static> {
    Line::styled(format!(" ├{}", "─".repeat(width.saturating_sub(3))), dim())
}

fn problem(app: &App, width: usize) -> Vec<Line<'_>> {
    let Some(error) = &app.snapshot.error else {
        return Vec::new();
    };
    // A parser reports over several lines, with a drawing of the place.
    let sentence = error.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut lines: Vec<Line> = wrapped(&sentence, width.saturating_sub(2))
        .into_iter()
        .take(3)
        .map(|part| Line::styled(format!(" {part}"), bad()))
        .collect();
    lines.push(Line::default());
    lines
}

fn jobs<'a>(app: &'a App, now: &Zoned, width: usize, rows: usize) -> Vec<Line<'a>> {
    let mut lines = problem(app, width);
    let all = &app.snapshot.jobs;
    if all.is_empty() {
        if app.snapshot.error.is_none() {
            lines.push(Line::raw(" no jobs yet"));
            lines.push(Line::styled(
                format!(" otto reads them from {}", app.snapshot.config.display()),
                dim(),
            ));
        }
        return lines;
    }
    let selected = all
        .iter()
        .position(|job| Some(&job.name) == app.job.as_ref());
    let visible = rows.saturating_sub(lines.len()) / ENTRY;
    let first = first_visible(selected.unwrap_or(0), visible);
    for (index, job) in all.iter().enumerate().skip(first).take(visible.max(1)) {
        let chosen = selected == Some(index);
        lines.push(between(
            vec![
                Span::styled(if chosen { "▸ " } else { "  " }, accent()),
                Span::styled(
                    fit(&job.name, 20),
                    if chosen { strong() } else { Style::new() },
                ),
                Span::raw(format!("  {:<8}", job.job.agent.program())),
                Span::styled(schedule(&job.job.schedule), dim()),
            ],
            vec![state(job)],
            width,
        ));

        let mut left = vec![
            Span::styled(" │ ", dim()),
            Span::raw(format!("{:<22}", coming(job.next.as_ref(), now, "next "))),
        ];
        let right = last(job, now);
        let marks = strip(job);
        // On a narrow terminal the strip gives way to how the last run ended.
        if width_of(&left) + 2 + width_of(&marks) + 2 + width_of(&right) < width {
            left.push(Span::raw("  "));
            left.extend(marks);
        }
        lines.push(between(left, right, width));
        lines.push(rule(width));
    }
    lines
}

fn job<'a>(app: &'a App, now: &Zoned, width: usize, rows: usize) -> Vec<Line<'a>> {
    let Some(job) = app.selected() else {
        return Vec::new();
    };
    let fact = |name: &'static str, value: String| {
        vec![Span::styled(format!(" {name:<9}"), dim()), Span::raw(value)]
    };
    let args = if job.job.args.is_empty() {
        "none".to_owned()
    } else {
        job.job.args.join(" ")
    };
    let mut lines = problem(app, width);
    lines.push(between(
        fact("agent", job.job.agent.program().to_owned()),
        vec![state(job)],
        width,
    ));
    lines.push(Line::from(fact("at", schedule(&job.job.schedule))));
    lines.push(Line::from(fact("next", coming(job.next.as_ref(), now, ""))));
    // A fact starts in column ten and stops a column short of the edge.
    let room = width.saturating_sub(11);
    lines.push(Line::from(fact(
        "workdir",
        tail(&job.job.workdir.display().to_string(), room),
    )));
    lines.push(Line::from(fact(
        "prompt",
        tail(&job.job.prompt.display().to_string(), room),
    )));
    lines.push(Line::from(fact("args", args)));
    lines.push(rule(width));

    if job.runs.is_empty() {
        lines.push(Line::styled(" no runs yet", dim()));
        return lines;
    }
    let selected = job
        .runs
        .iter()
        .position(|run| Some(&run.id) == app.run.as_ref());
    let visible = rows.saturating_sub(lines.len());
    let first = first_visible(selected.unwrap_or(0), visible);
    for (index, run) in job.runs.iter().enumerate().skip(first).take(visible.max(1)) {
        let chosen = selected == Some(index);
        let outcome = match run.outcome {
            Outcome::Failed | Outcome::Interrupted => bad(),
            Outcome::Running => accent(),
            _ => Style::new(),
        };
        lines.push(Line::from(vec![
            Span::styled(if chosen { "▸ " } else { "  " }, accent()),
            mark(run),
            Span::raw(format!("  {}  ", started(run, now))),
            Span::styled(format!("{:<11}", run.trigger.label()), dim()),
            Span::styled(format!("{:<14}", run.outcome_label()), outcome),
            Span::styled(run.duration_label().unwrap_or_default(), dim()),
        ]));
    }
    lines
}

fn log(app: &App, rows: usize) -> Vec<Line<'_>> {
    let Some(log) = &app.log else {
        return vec![Line::styled(" reading the output…", dim())];
    };
    log.text
        .lines()
        .skip(app.first_line())
        .take(rows)
        .map(|line| Line::raw(format!(" {line}")))
        .collect()
}

/// Every key and what it does, in the order of the shortcut bars.
const HELP: [(&str, &str); 14] = [
    ("enter", "open the job, then the output of a run"),
    ("esc", "go back"),
    ("r", "run the job now"),
    ("x", "stop the run in progress"),
    ("p", "pause the schedule"),
    ("s", "skip the next scheduled run"),
    ("u", "resume the schedule"),
    ("e", "edit the prompt in $VISUAL or $EDITOR"),
    ("↑ ↓  k j", "move"),
    ("pgup pgdn", "move a page"),
    ("g G", "go to the first, the last"),
    ("?", "this help"),
    ("q", "quit"),
    ("ctrl-c", "quit"),
];

fn help() -> Vec<Line<'static>> {
    HELP.iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!(" {key:<11}"), strong()),
                Span::raw(*what),
            ])
        })
        .collect()
}

fn notice(app: &App) -> Line<'_> {
    if let Some(notice) = &app.notice {
        let style = if notice.error { bad() } else { accent() };
        return Line::styled(format!(" {}", notice.text), style);
    }
    // With nothing to tell, the log screen says how much of the output it shows.
    match (&app.screen, &app.log) {
        (Screen::Log, Some(log)) if log.truncated => {
            Line::styled(" showing the last 1 MiB of the output", dim())
        }
        _ => Line::default(),
    }
}

/// The keys of the screen, or the question the app is waiting on.
fn bar(app: &App, width: usize) -> Line<'_> {
    if let Some(confirm) = &app.confirm {
        let question = match confirm {
            Confirm::Stop { job, .. } => format!(" stop {job}?"),
        };
        return Line::from(vec![
            Span::styled(question, accent().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("y", strong()),
            Span::styled(" yes  ", dim()),
            Span::styled("n", strong()),
            Span::styled(" no", dim()),
        ]);
    }
    let keys: &[(&str, &str)] = match app.screen {
        Screen::Jobs => &[
            ("enter", "open"),
            ("r", "run"),
            ("x", "stop"),
            ("p", "pause"),
            ("s", "skip"),
            ("u", "resume"),
            ("e", "prompt"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Screen::Job => &[
            ("enter", "log"),
            ("r", "run"),
            ("x", "stop"),
            ("p", "pause"),
            ("s", "skip"),
            ("u", "resume"),
            ("e", "prompt"),
            ("esc", "back"),
        ],
        Screen::Log => &[
            ("↑↓", "scroll"),
            ("g", "top"),
            ("G", "end"),
            ("esc", "back"),
            ("?", "help"),
        ],
        Screen::Help => &[("esc", "back"), ("q", "quit")],
    };
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1;
    for (key, what) in keys {
        // One that does not fit is left out whole; the help screen has it.
        let needed = key.chars().count() + 1 + what.chars().count();
        if used + needed > width {
            break;
        }
        used += needed + 2;
        spans.push(Span::styled(*key, strong()));
        spans.push(Span::styled(format!(" {what}  "), dim()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use jiff::Timestamp;
    use jiff::civil::DateTime;
    use jiff::tz::TimeZone;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::agent::Agent;
    use crate::config::{Job, Schedule, Weekday};
    use crate::next::Next;
    use crate::store::{Outcome, Run, State, Trigger};
    use crate::tui::app::{Action, Notice};
    use crate::tui::world::{JobView, Log, Snapshot};

    fn local(text: &str) -> Zoned {
        text.parse::<DateTime>()
            .unwrap()
            .to_zoned(TimeZone::get("America/Sao_Paulo").unwrap())
            .unwrap()
    }

    /// Tuesday morning.
    fn now() -> Zoned {
        local("2026-10-06T09:00")
    }

    /// What the app looks like on a terminal of this size, one line per row.
    fn screen(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app, &now())).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn job(name: &str) -> JobView {
        JobView {
            name: name.to_owned(),
            job: Job {
                agent: Agent::Claude,
                prompt: PathBuf::from(format!("/prompts/{name}.md")),
                workdir: PathBuf::from("/work/app"),
                schedule: Schedule {
                    at: "16:05".to_owned(),
                    days: vec![
                        Weekday::Mon,
                        Weekday::Tue,
                        Weekday::Wed,
                        Weekday::Thu,
                        Weekday::Fri,
                    ],
                },
                args: vec!["--model".to_owned(), "sonnet".to_owned()],
            },
            state: State::default(),
            runs: Vec::new(),
            next: Some(Next::At(local("2026-10-06T16:05"))),
        }
    }

    fn run(id: &str, outcome: Outcome, trigger: Trigger) -> Run {
        let started: Timestamp = "2026-10-05T19:16:26Z".parse().unwrap();
        let finished: Timestamp = "2026-10-05T19:18:40Z".parse().unwrap();
        Run {
            id: id.to_owned(),
            started,
            finished: (outcome != Outcome::Running).then_some(finished),
            trigger,
            outcome,
            exit_code: (outcome == Outcome::Failed).then_some(3),
            pid: (outcome == Outcome::Running).then_some(77),
        }
    }

    fn app(jobs: Vec<JobView>) -> App {
        App::new(Snapshot {
            config: PathBuf::from("/home/me/.config/otto/jobs.toml"),
            jobs,
            error: None,
        })
    }

    /// `report`, paused, whose last run failed.
    fn report() -> JobView {
        JobView {
            state: State {
                paused: true,
                skip_next: false,
            },
            runs: vec![run("r1", Outcome::Failed, Trigger::Scheduled)],
            next: Some(Next::Paused),
            ..job("report")
        }
    }

    #[test]
    fn the_job_list_shows_what_the_spec_asks() {
        let text = screen(&app(vec![report(), job("triage")]), 80, 24);
        for expected in [
            "report",
            "claude",
            "16:05 mon–fri",
            "paused",
            "failed (3)",
            "no next run",
            "triage",
            "active",
            "next today 16:05",
            "never ran",
            "09:00",
        ] {
            assert!(text.contains(expected), "{expected:?} is missing:\n{text}");
        }
    }

    #[test]
    fn the_selected_job_carries_the_marker() {
        let mut app = app(vec![report(), job("triage")]);
        assert!(screen(&app, 80, 24).contains("▸ report"));
        app.act(Action::Down);
        let text = screen(&app, 80, 24);
        assert!(text.contains("▸ triage"));
        assert!(!text.contains("▸ report"));
    }

    #[test]
    fn a_skip_shows_both_runs() {
        let skipping = JobView {
            state: State {
                paused: false,
                skip_next: true,
            },
            next: Some(Next::Skipping {
                skipped: local("2026-10-06T16:05"),
                then: local("2026-10-07T16:05"),
            }),
            ..job("report")
        };
        let text = screen(&app(vec![skipping]), 80, 24);
        assert!(text.contains("skip next"));
        assert!(text.contains("skips today 16:05, then tomorrow 16:05"));
    }

    #[test]
    fn the_run_strip_reads_oldest_to_newest() {
        let with_runs = JobView {
            runs: vec![
                run("r4", Outcome::Running, Trigger::Manual),
                run("r3", Outcome::Ok, Trigger::Scheduled),
                run("r2", Outcome::Failed, Trigger::Scheduled),
                run("r1", Outcome::Skipped, Trigger::Scheduled),
                run("r0", Outcome::Interrupted, Trigger::Scheduled),
            ],
            ..job("report")
        };
        let text = screen(&app(vec![with_runs]), 80, 24);
        assert!(text.contains("! · ✗ ✓ ●"), "{text}");
        assert!(text.contains("● running"));
        assert!(text.contains("running since 16:16"), "{text}");
        assert!(!text.contains("last running"));
    }

    #[test]
    fn the_strip_keeps_the_last_eight_runs() {
        let mut runs = vec![run("new", Outcome::Failed, Trigger::Manual)];
        runs.extend((0..12).map(|n| run(&format!("r{n}"), Outcome::Ok, Trigger::Manual)));
        let text = screen(
            &app(vec![JobView {
                runs,
                ..job("report")
            }]),
            80,
            24,
        );
        assert!(text.contains(" ✓ ✓ ✓ ✓ ✓ ✓ ✓ ✗ "), "{text}");
        assert!(!text.contains("✓ ✓ ✓ ✓ ✓ ✓ ✓ ✓"));
    }

    #[test]
    fn an_empty_list_says_where_the_jobs_file_goes() {
        let text = screen(&app(Vec::new()), 80, 24);
        assert!(text.contains("no jobs yet"));
        assert!(text.contains("/home/me/.config/otto/jobs.toml"));
    }

    #[test]
    fn a_jobs_file_error_is_on_screen() {
        let mut app = app(vec![job("report")]);
        app.snapshot.error = Some("invalid config jobs.toml: job report".to_owned());
        let text = screen(&app, 80, 24);
        assert!(text.contains("invalid config jobs.toml: job report"));
        assert!(text.contains("report"));
    }

    #[test]
    fn a_long_list_keeps_the_selected_job_in_view() {
        let jobs = (0..20).map(|n| job(&format!("job-{n:02}"))).collect();
        let mut app = app(jobs);
        assert!(screen(&app, 80, 14).contains("job-00"));
        app.act(Action::Bottom);
        let text = screen(&app, 80, 14);
        assert!(text.contains("▸ job-19"), "{text}");
        assert!(!text.contains("job-00"));
    }

    fn on_the_job() -> App {
        let with_runs = JobView {
            runs: vec![
                run("r2", Outcome::Ok, Trigger::Manual),
                run("r1", Outcome::Failed, Trigger::Scheduled),
            ],
            ..job("report")
        };
        let mut app = app(vec![with_runs]);
        app.act(Action::Open);
        app
    }

    #[test]
    fn the_job_screen_lists_the_runs() {
        let text = screen(&on_the_job(), 80, 24);
        for expected in [
            "otto · report",
            "claude",
            "16:05 mon–fri",
            "today 16:05",
            "/work/app",
            "/prompts/report.md",
            "--model sonnet",
            "2026-10-05 16:16",
            "manual",
            "scheduled",
            "failed (3)",
            "2m 14s",
            "▸ ✓",
        ] {
            assert!(text.contains(expected), "{expected:?} is missing:\n{text}");
        }
    }

    #[test]
    fn a_job_that_never_ran_says_so() {
        let mut app = app(vec![job("report")]);
        app.act(Action::Open);
        assert!(screen(&app, 80, 24).contains("no runs yet"));
    }

    fn on_the_log(log: Log) -> App {
        let mut app = on_the_job();
        app.act(Action::Open);
        app.page = page(24);
        app.show_log(Ok(log));
        app
    }

    #[test]
    fn the_log_screen_shows_the_output_and_says_when_it_was_cut() {
        let whole = on_the_log(Log {
            text: "first line\nsecond line\n".to_owned(),
            truncated: false,
        });
        let text = screen(&whole, 80, 24);
        assert!(text.contains("first line"));
        assert!(text.contains("second line"));
        assert!(text.contains("2026-10-05 16:16"));
        assert!(!text.contains("showing the last 1 MiB"));

        let cut = on_the_log(Log {
            text: "tail\n".to_owned(),
            truncated: true,
        });
        assert!(screen(&cut, 80, 24).contains("showing the last 1 MiB"));
    }

    #[test]
    fn a_long_log_shows_its_end_until_the_user_scrolls() {
        let text: String = (0..100).map(|n| format!("line {n:03}\n")).collect();
        let mut app = on_the_log(Log {
            text,
            truncated: false,
        });
        let end = screen(&app, 80, 24);
        assert!(end.contains("line 099"));
        assert!(!end.contains("line 000"));

        app.act(Action::Top);
        let start = screen(&app, 80, 24);
        assert!(start.contains("line 000"));
        assert!(!start.contains("line 099"));
    }

    #[test]
    fn a_log_not_read_yet_says_it_is_loading() {
        let mut app = on_the_job();
        app.act(Action::Open);
        assert!(screen(&app, 80, 24).contains("reading"));
    }

    #[test]
    fn a_confirmation_names_what_it_stops() {
        let running = JobView {
            runs: vec![run("r1", Outcome::Running, Trigger::Manual)],
            ..job("report")
        };
        let mut app = app(vec![running]);
        app.act(Action::Stop);
        let text = screen(&app, 80, 24);
        assert!(text.contains("stop report?"));
        assert!(text.contains("y yes"));
        assert!(text.contains("n no"));
        assert!(!text.contains("q quit"));
    }

    #[test]
    fn every_action_of_the_screen_is_in_the_shortcut_bar() {
        let text = screen(&app(vec![job("report")]), 80, 24);
        let bar = text.lines().last().unwrap();
        for word in [
            "open", "run", "stop", "pause", "skip", "resume", "prompt", "help", "quit",
        ] {
            assert!(bar.contains(word), "{word:?} is missing from {bar:?}");
        }
    }

    #[test]
    fn the_help_screen_lists_every_action() {
        let mut app = app(vec![job("report")]);
        app.act(Action::Help);
        let text = screen(&app, 80, 24);
        for expected in [
            "run the job now",
            "stop the run in progress",
            "pause the schedule",
            "skip the next scheduled run",
            "resume the schedule",
            "edit the prompt",
            "quit",
        ] {
            assert!(text.contains(expected), "{expected:?} is missing:\n{text}");
        }
    }

    #[test]
    fn a_small_terminal_gets_a_message() {
        let text = screen(&app(vec![job("report")]), 40, 8);
        assert!(text.contains("terminal too small"));
        assert!(!text.contains("report"));
    }

    #[test]
    fn a_notice_is_on_screen() {
        let mut app = app(vec![job("report")]);
        app.notice = Some(Notice {
            text: "report is already running".to_owned(),
            error: false,
        });
        assert!(screen(&app, 80, 24).contains("report is already running"));
    }

    #[test]
    fn a_shortcut_that_does_not_fit_is_left_out_whole() {
        let text = screen(&app(vec![job("report")]), 76, 24);
        let bar = text.lines().last().unwrap().trim_end();
        assert!(bar.ends_with("? help"), "{bar:?}");
    }

    #[test]
    fn a_long_path_keeps_its_end() {
        let long = JobView {
            job: Job {
                prompt: PathBuf::from(format!("/{}/prompts/report.md", "deep/".repeat(30))),
                ..job("report").job
            },
            ..job("report")
        };
        let mut app = app(vec![long]);
        app.act(Action::Open);
        let text = screen(&app, 80, 24);
        assert!(text.contains("…"), "{text}");
        assert!(text.contains("/prompts/report.md"), "{text}");
    }

    #[test]
    fn an_error_of_many_lines_reads_as_one_sentence() {
        let mut app = app(vec![job("report")]);
        app.snapshot.error = Some("invalid config jobs.toml: TOML parse error\n  |\n1 | jobs = 3\n  |        ^\ninvalid type".to_owned());
        let text = screen(&app, 80, 24);
        assert!(
            text.contains("TOML parse error | 1 | jobs = 3 | ^ invalid type"),
            "{text}"
        );
    }

    #[test]
    fn the_smallest_terminal_still_shows_a_job() {
        let text = screen(&app(vec![report()]), MIN.0, MIN.1);
        assert!(text.contains("▸ report"));
        assert!(text.contains("failed (3)"));
    }
}
