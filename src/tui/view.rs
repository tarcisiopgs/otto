//! The drawing of the terminal UI: a function of the state and of the current
//! time, and of nothing else. The look is described in `DESIGN.md`.

use jiff::Zoned;
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::agent::Agent;
use crate::config::{Schedule, Weekday};
use crate::next::{self, Next};
use crate::store::{Outcome, Run};
use crate::tui::app::{App, Confirm, ENTRY, Screen};
use crate::tui::form::{Field, Focus, Form};
use crate::tui::text::{cut, fit, tail, width as width_in_columns, wrapped};
use crate::tui::world::JobView;

/// The smallest terminal the UI is drawn on: columns, then rows.
pub const MIN: (u16, u16) = (60, 12);

/// How many of a job's latest runs its strip of marks shows.
const STRIP: usize = 8;

/// The rows of the job screen above its runs: the entry, three facts, a rule.
const SHEET: usize = 6;

/// The columns kept for how the last run ended, so that the strips of every
/// job end in the same column.
const LAST: usize = 28;

/// The columns an ordinary coming run takes: `next tomorrow 16:05` and a few
/// to spare.
const COMING: usize = 22;

/// How many runs the job screen keeps in view before its facts give way.
const RUNS: usize = 3;

/// The rows left for what scrolls on a terminal `height` rows tall: all but
/// the header, the line under it, the notice and the shortcut bar.
pub fn page(height: u16) -> usize {
    usize::from(height.saturating_sub(4))
}

/// The columns a line of the log has on a terminal `width` columns wide: all
/// but the margin rule before it and the column after it.
pub fn columns(width: u16) -> usize {
    usize::from(width.saturating_sub(4))
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
    let mut cursor = None;
    let mut body = match app.screen {
        Screen::Jobs => jobs(app, now, width, rows),
        Screen::Job => job(app, now, width, rows),
        Screen::Log => log(app, rows),
        Screen::Help => help(),
        Screen::Form => match &app.form {
            Some(form) => {
                let (lines, at) = fields(form, width);
                cursor = Some(at);
                lines
            }
            None => Vec::new(),
        },
    };
    body.resize(rows, Line::default());

    let mut lines = vec![header(app, now, width, rows), Line::default()];
    lines.extend(body);
    lines.push(notice(app, width));
    lines.push(bar(app, width));
    frame.render_widget(Paragraph::new(lines), area);
    // The terminal's own cursor shows where typing goes. It is only shown
    // while a field is being filled, and never under a question.
    if let (Some((column, row)), None) = (cursor, &app.confirm) {
        let fits = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        frame.set_cursor_position((fits(column), fits(row + 2)));
    }
}

/// The one accent, kept to the two glyphs that mean "here" and "now": the
/// selection marker and the mark of a run in progress. Words stay in the
/// terminal's own foreground, which a light theme can read.
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

fn spaces(count: usize) -> Span<'static> {
    Span::raw(" ".repeat(count))
}

/// `left` at the left edge and `right` at the right one, a column in from it.
fn between<'a>(left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: usize) -> Line<'a> {
    let gap = width
        .saturating_sub(width_of(&left) + width_of(&right) + 1)
        .max(1);
    let mut spans = left;
    spans.push(spaces(gap));
    spans.extend(right);
    Line::from(spans)
}

fn header<'a>(app: &'a App, now: &Zoned, width: usize, rows: usize) -> Line<'a> {
    let job = app.selected();
    let name = app.job.as_deref().unwrap_or_default();
    let run = app.selected_run();
    // Where the selection is in a list that does not fit: `3 of 12`.
    let folio = |chosen: Option<usize>, len: usize, visible: usize| match chosen {
        Some(index) if len > visible => format!(" · {} of {len}", index + 1),
        _ => String::new(),
    };
    let place = match app.screen {
        Screen::Jobs => {
            let all = &app.snapshot.jobs;
            let chosen = all.iter().position(|job| job.name == name);
            let visible = rows.saturating_sub(problem(app, width).len()) / ENTRY;
            format!("jobs{}", folio(chosen, all.len(), visible))
        }
        Screen::Job => {
            let runs = job.map_or(&[][..], |job| job.runs.as_slice());
            let chosen = runs
                .iter()
                .position(|one| Some(&one.id) == app.run.as_ref());
            let left = rows.saturating_sub(problem(app, width).len());
            let visible = left.saturating_sub(sheet(left));
            format!("{name}{}", folio(chosen, runs.len(), visible))
        }
        Screen::Log => match run {
            Some(run) => format!("{name} · {}", started(run, now)),
            None => name.to_owned(),
        },
        Screen::Help => "help".to_owned(),
        Screen::Form => match app.form.as_ref().and_then(|form| form.editing.as_deref()) {
            Some(job) => format!("edit {job}"),
            None => "new job".to_owned(),
        },
    };
    let right = match (app.screen, run) {
        (Screen::Log, Some(run)) => {
            let mut spans = vec![mark(run), Span::raw(" "), outcome(run)];
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
    // A long name gives way to what sits at the right edge.
    let room = width.saturating_sub(width_of(&right) + 10);
    let left = vec![
        Span::styled(" otto", strong()),
        Span::styled(" · ", dim()),
        Span::raw(cut(&place, room)),
    ];
    between(left, right, width)
}

/// The mark a run leaves in the logbook. Each outcome has a glyph of its own,
/// so the strip reads the same without colour.
fn mark(run: &Run) -> Span<'static> {
    match run.outcome {
        Outcome::Running => Span::styled("●", accent()),
        Outcome::Ok => Span::raw("✓"),
        Outcome::Failed => Span::styled("✗", bad()),
        // The record was left open: the machine went down, or the run was
        // stopped. otto cannot tell which, and neither finished.
        Outcome::Interrupted => Span::styled("!", bad()),
        // A scheduled run that did not start the agent.
        Outcome::Skipped | Outcome::Paused => Span::styled("·", dim()),
    }
}

/// How a run ended, in words.
fn outcome(run: &Run) -> Span<'static> {
    let style = match run.outcome {
        Outcome::Failed | Outcome::Interrupted => bad(),
        _ => Style::new(),
    };
    Span::styled(run.outcome_label(), style)
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

/// The coming runs in words.
fn coming(next: Option<&Next>, now: &Zoned) -> String {
    match next {
        Some(Next::At(when)) => format!("next {}", next::label(when, now)),
        Some(Next::Skipping { skipped, then }) => format!(
            "skips {}, then {}",
            next::label(skipped, now),
            next::label(then, now)
        ),
        Some(Next::Paused) | None => "no next run".to_owned(),
    }
}

fn state(job: &JobView) -> Vec<Span<'static>> {
    match job.status() {
        "running" => vec![Span::styled("●", accent()), Span::raw(" running")],
        "paused" => vec![Span::styled("paused", strong())],
        "skip next" => vec![Span::raw("skip next")],
        other => vec![Span::styled(other, dim())],
    }
}

/// How the last finished run ended, from the most said to the least:
/// `last ok · 2m 14s`, `last ok`, `ok`. A narrow line takes a shorter one.
fn last(job: &JobView, now: &Zoned) -> Vec<Vec<Span<'static>>> {
    let finished = job.runs.iter().find(|run| run.outcome != Outcome::Running);
    let Some(run) = finished else {
        // Nothing has finished yet: the first run is going, or none ever ran.
        return vec![match job.running() {
            Some(run) => {
                let since = run
                    .started
                    .to_zoned(now.time_zone().clone())
                    .strftime("%H:%M");
                vec![Span::raw(format!("running since {since}"))]
            }
            None => vec![Span::styled("never ran", dim())],
        }];
    };
    let short = vec![Span::styled("last ", dim()), outcome(run)];
    let mut full = short.clone();
    if let Some(duration) = run.duration_label() {
        full.push(Span::styled(format!(" · {duration}"), dim()));
    }
    vec![full, short, vec![outcome(run)]]
}

/// The latest `count` runs as marks, oldest first, so the newest sits at the
/// right.
fn strip(job: &JobView, count: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for run in job.runs.iter().take(count).rev() {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(mark(run));
    }
    spans
}

/// The second line of an entry: the coming run, the strip of marks and how
/// the last run ended.
///
/// With room, the strip ends in a fixed column, so the newest run of every
/// job is read down one column. Without it the strips still stack, after a
/// coming run given the room of an ordinary one, and the ending gives up its
/// duration, then the word `last`. Only then does the line let go of the
/// shared column, and last of all of its oldest marks.
fn second_line(job: &JobView, now: &Zoned, width: usize) -> Line<'static> {
    let lead = vec![
        Span::styled(" │ ", dim()),
        Span::raw(coming(job.next.as_ref(), now)),
    ];
    let edge = width.saturating_sub(1);
    let endings = last(job, now);
    let marks = strip(job, STRIP);
    let build = |marks: Vec<Span<'static>>, gap: usize, ending: &[Span<'static>]| {
        let mut left = lead.clone();
        if !marks.is_empty() {
            left.push(spaces(gap));
            left.extend(marks);
        }
        between(left, ending.to_vec(), width)
    };

    let column = edge.saturating_sub(LAST + 2);
    let taken = width_of(&lead) + width_of(&marks);
    if let Some(full) = endings.first().filter(|full| width_of(full) <= LAST)
        && taken + 2 <= column
    {
        return build(marks, column - taken, full);
    }
    let fits = |marks: &[Span], ending: &[Span]| {
        let strip = if marks.is_empty() {
            0
        } else {
            2 + width_of(marks)
        };
        width_of(&lead) + strip + 2 + width_of(ending) <= edge
    };
    // Too narrow for the fixed column: the strips still stack when the
    // coming run is given the room of an ordinary one.
    let padded = COMING.saturating_sub(width_of(&lead).saturating_sub(3));
    for gap in [2 + padded, 2] {
        for ending in &endings {
            let room = width_of(&lead) + gap + width_of(&marks) + 2 + width_of(ending);
            if !marks.is_empty() && room <= edge {
                return build(marks, gap, ending);
            }
        }
    }
    for ending in &endings {
        if fits(&marks, ending) {
            return build(marks, 2, ending);
        }
    }
    let shortest = endings.last().map_or(&[][..], Vec::as_slice);
    for count in (0..STRIP).rev() {
        let fewer = strip(job, count);
        if fits(&fewer, shortest) {
            return build(fewer, 2, shortest);
        }
    }
    build(Vec::new(), 2, shortest)
}

/// The first line of an entry: the job, its agent, its schedule and its state.
fn first_line<'a>(job: &'a JobView, chosen: bool, marker: bool, width: usize) -> Line<'a> {
    between(
        vec![
            Span::styled(if chosen && marker { "▸ " } else { "  " }, accent()),
            Span::styled(
                fit(&job.name, 20),
                if chosen { strong() } else { Style::new() },
            ),
            Span::raw(format!("  {:<8}", job.job.agent.program())),
            Span::raw(schedule(&job.job.schedule)),
        ],
        state(job),
        width,
    )
}

/// The rows the job screen spends above its runs when `left` are free: the
/// whole sheet, or only the entry and the rule when the runs would be left
/// with too few.
fn sheet(left: usize) -> usize {
    if left >= SHEET + RUNS { SHEET } else { ENTRY }
}

/// The first of the rows to draw so that row `selected` is among `visible`.
fn first_visible(selected: usize, visible: usize) -> usize {
    (selected + 1).saturating_sub(visible.max(1))
}

fn rule(width: usize) -> Line<'static> {
    Line::styled(format!(" ├{}", "─".repeat(width.saturating_sub(3))), dim())
}

/// Why the jobs file cannot be used, in at most two lines, and what the list
/// under it is.
fn problem(app: &App, width: usize) -> Vec<Line<'_>> {
    let Some(error) = &app.snapshot.error else {
        return Vec::new();
    };
    // The file is the one otto was opened with: its name is enough, and its
    // directory would take the line the reason needs.
    let path = app.snapshot.config.display().to_string();
    let name = app
        .snapshot
        .config
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let error = match name {
        Some(name) if !path.is_empty() => error.replace(&path, &name),
        _ => error.clone(),
    };
    // A parser reports over several lines and draws the place (`1 | jobs = 3`,
    // `  |    ^`). Folded into a sentence the drawing means nothing: it is
    // left out, and the lines that say something are joined.
    let said: Vec<&str> = error
        .lines()
        .map(str::trim)
        .filter(|line| {
            let drawn = line
                .split_once('|')
                .is_some_and(|(before, _)| before.chars().all(|c| c.is_ascii_digit() || c == ' '));
            !line.is_empty() && !drawn
        })
        .collect();
    let room = width.saturating_sub(4);
    let mut parts = wrapped(&said.join(", "), room);
    if parts.len() > 2 {
        parts.truncate(2);
        if let Some(end) = parts.last_mut() {
            *end = format!(
                "{}…",
                cut(end, room.saturating_sub(1)).trim_end_matches('…')
            );
        }
    }
    let mut lines: Vec<Line> = parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let lead = if index == 0 { " ! " } else { "   " };
            Line::styled(format!("{lead}{part}"), bad())
        })
        .collect();
    if !app.snapshot.jobs.is_empty() {
        lines.push(Line::styled(
            "   showing the last valid read; fix the file to reload",
            dim(),
        ));
    }
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
        lines.push(first_line(job, selected == Some(index), true, width));
        lines.push(second_line(job, now, width));
        lines.push(rule(width));
    }
    lines
}

fn job<'a>(app: &'a App, now: &Zoned, width: usize, rows: usize) -> Vec<Line<'a>> {
    let Some(job) = app.selected() else {
        return Vec::new();
    };
    // A fact sits behind the margin rule and stops a column short of the edge.
    let room = width.saturating_sub(13);
    let fact = |name: &'static str, value: String| {
        Line::from(vec![
            Span::styled(format!(" │ {name:<9}"), dim()),
            Span::raw(value),
        ])
    };
    let args = if job.job.args.is_empty() {
        "none".to_owned()
    } else {
        job.job.args.join(" ")
    };
    let mut lines = problem(app, width);
    // The same entry the list shows, opened: its facts and its runs follow.
    lines.push(first_line(job, true, false, width));
    lines.push(second_line(job, now, width));
    // On a short terminal the facts give way to the runs.
    if sheet(rows.saturating_sub(lines.len() - 2)) == SHEET {
        lines.push(fact(
            "workdir",
            tail(&job.job.workdir.display().to_string(), room),
        ));
        lines.push(fact(
            "prompt",
            tail(&job.job.prompt.display().to_string(), room),
        ));
        lines.push(fact("args", cut(&args, room)));
    }
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
        lines.push(Line::from(vec![
            Span::styled(if chosen { "▸ " } else { "  " }, accent()),
            mark(run),
            Span::raw(format!("  {}  ", started(run, now))),
            Span::styled(format!("{:<11}", run.trigger.label()), dim()),
            {
                let mut how = outcome(run);
                how.content = format!("{:<14}", how.content).into();
                how
            },
            Span::styled(run.duration_label().unwrap_or_default(), dim()),
        ]));
    }
    lines
}

/// The output of a run, behind the margin rule like the rest of its entry.
fn log(app: &App, rows: usize) -> Vec<Line<'_>> {
    let behind = |text: &str, style: Style| {
        Line::from(vec![
            Span::styled(" │ ", dim()),
            Span::styled(text.to_owned(), style),
        ])
    };
    if app.log.is_none() {
        return vec![behind("reading the output…", dim())];
    }
    app.log_rows()
        .into_iter()
        .skip(app.first_line())
        .take(rows)
        .map(|row| behind(row, Style::new()))
        .collect()
}

/// Where the value of a field starts: after the marker, the margin rule and
/// the label.
const VALUE: usize = 12;

/// A field that takes text, as much of it as fits, and where its cursor is.
/// A text longer than the room shows the part the cursor is in.
fn typed(field: &Field, room: usize) -> (String, usize) {
    // A line break is one argument ending and the next beginning.
    let shown = |text: &str| text.replace('\n', " ⏎ ");
    let before = shown(&field.text()[..field.cursor()]);
    let after = shown(&field.text()[field.cursor()..]);
    // Room for the cursor itself, which sits after the text.
    let room = room.saturating_sub(1).max(1);
    let before = tail(&before, room);
    let column = width_in_columns(&before);
    let after = cut(&after, room - column.min(room));
    (format!("{before}{after}"), column)
}

/// A choice among a few: the chosen ones carry a mark, the others a dot.
fn choice(name: &str, chosen: bool) -> Vec<Span<'static>> {
    if chosen {
        vec![Span::raw("✓"), Span::styled(name.to_owned(), strong())]
    } else {
        vec![Span::styled(format!("·{name}"), dim())]
    }
}

/// The form: one opened entry, each field a fact behind the margin rule, and
/// where on it the cursor goes, as a column and a row of the body.
fn fields(form: &Form, width: usize) -> (Vec<Line<'_>>, (usize, usize)) {
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let room = width.saturating_sub(VALUE + 1);
    let mut cursor = (VALUE, 0);
    let mut lines = Vec::new();
    let order = [
        (Focus::Name, "name"),
        (Focus::Agent, "agent"),
        (Focus::Workdir, "workdir"),
        (Focus::At, "at"),
        (Focus::Days, "days"),
        (Focus::Args, "args"),
        (Focus::Prompt, "prompt"),
    ];
    for (row, (focus, label)) in order.into_iter().enumerate() {
        let focused = form.focus == focus;
        let mut spans = vec![
            Span::styled(if focused { "▸" } else { " " }, accent()),
            Span::styled("│ ", dim()),
            Span::styled(
                format!("{label:<9}"),
                if focused { strong() } else { dim() },
            ),
        ];
        let mut column = 0;
        let text = match focus {
            Focus::Name => Some(&form.name),
            Focus::Workdir => Some(&form.workdir),
            Focus::At => Some(&form.at),
            Focus::Args => Some(&form.args),
            Focus::Prompt => Some(&form.prompt),
            Focus::Agent | Focus::Days => None,
        };
        match (focus, text) {
            (_, Some(field)) => {
                let (shown, at) = typed(field, room);
                column = at;
                // A job that exists keeps its name: it is shown, not offered.
                let style = match (focus, &form.editing) {
                    (Focus::Name, Some(_)) => dim(),
                    _ => Style::new(),
                };
                spans.push(Span::styled(shown, style));
            }
            (Focus::Agent, _) => {
                for (index, agent) in Agent::ALL.into_iter().enumerate() {
                    if index > 0 {
                        spans.push(Span::raw("  "));
                    }
                    if agent == form.agent {
                        column = width_of(&spans) - VALUE;
                    }
                    spans.extend(choice(agent.program(), agent == form.agent));
                }
            }
            (_, None) => {
                for (index, day) in DAYS.into_iter().enumerate() {
                    if index > 0 {
                        spans.push(Span::raw(" "));
                    }
                    if index == form.day {
                        column = width_of(&spans) - VALUE;
                    }
                    spans.extend(choice(day, form.days[index]));
                }
            }
        }
        if focused {
            cursor = (VALUE + column, row);
        }
        lines.push(Line::from(spans));
    }
    lines.push(rule(width));
    (lines, cursor)
}

/// What the job keys do. One screen of the smallest terminal holds these and
/// the column beside them.
const ACTIONS: [(&str, &str); 8] = [
    ("enter", "open the job, then a run"),
    ("esc", "go back"),
    ("r", "run the job now"),
    ("x", "stop the run in progress"),
    ("p s u", "pause, skip next, resume"),
    ("e", "edit the prompt"),
    ("n", "new job"),
    ("E d", "edit, delete the job"),
];

const MOVES: [(&str, &str); 5] = [
    ("↑ ↓ k j", "move"),
    ("pgup pgdn", "move a page"),
    ("g G", "first, last"),
    ("?", "this help"),
    ("q", "quit"),
];

/// The keys in two columns, with what each mark of a run means under the
/// second.
fn help() -> Vec<Line<'static>> {
    let ran = |outcome| Run {
        id: String::new(),
        started: jiff::Timestamp::UNIX_EPOCH,
        finished: None,
        trigger: crate::store::Trigger::Manual,
        outcome,
        exit_code: None,
        pid: None,
    };
    let legend = |outcome, meaning: &'static str| {
        vec![mark(&ran(outcome)), Span::raw(format!(" {meaning}"))]
    };
    let mut right: Vec<Vec<Span>> = MOVES
        .iter()
        .map(|(key, what)| {
            vec![
                Span::styled(format!("{key:<10}"), strong()),
                Span::raw(*what),
            ]
        })
        .collect();
    let mut both = legend(Outcome::Ok, "ok   ");
    both.extend(legend(Outcome::Failed, "failed"));
    right.push(both);
    let mut both = legend(Outcome::Interrupted, "interrupted   ");
    both.extend(legend(Outcome::Running, "running"));
    right.push(both);
    right.push(legend(Outcome::Skipped, "did not start the agent"));

    ACTIONS
        .iter()
        .zip(right)
        .map(|((key, what), right)| {
            let mut spans = vec![
                Span::styled(format!(" {key:<6}"), strong()),
                Span::raw(format!("{what:<26}")),
            ];
            spans.extend(right);
            Line::from(spans)
        })
        .collect()
}

fn notice(app: &App, width: usize) -> Line<'_> {
    let room = width.saturating_sub(2);
    if let Some(notice) = &app.notice {
        let style = if notice.error { bad() } else { Style::new() };
        return Line::styled(format!(" {}", cut(&notice.text, room)), style);
    }
    // With nothing to tell, the form says what the field in focus calls for,
    // or what saving the job as it stands would run into.
    if let (Screen::Form, Some(form)) = (&app.screen, &app.form) {
        let text = if form.focus == Focus::Args {
            "a scheduled run has nobody to answer a permission prompt".to_owned()
        } else if app.warnings.is_empty() {
            return Line::default();
        } else {
            format!("note: {}", app.warnings.join("; "))
        };
        return Line::raw(format!(" {}", cut(&text, room)));
    }
    // And the log screen says where it is in the output.
    let (Screen::Log, Some(log)) = (&app.screen, &app.log) else {
        return Line::default();
    };
    let total = app.log_rows().len();
    let first = app.first_line();
    let mut text = if total == 0 {
        "no output yet".to_owned()
    } else {
        format!("{}–{} of {total}", first + 1, (first + app.page).min(total))
    };
    let running = app
        .selected_run()
        .is_some_and(|run| run.outcome == Outcome::Running);
    if running && app.follow {
        text.push_str(" · following");
    }
    if log.truncated {
        text.push_str(" · showing the last 1 MiB of the output");
    }
    Line::styled(format!(" {}", cut(&text, room)), dim())
}

/// The keys of the screen, or the question the app is waiting on.
fn bar(app: &App, width: usize) -> Line<'_> {
    if let Some(confirm) = &app.confirm {
        // Each answer is named for what it does. Room is kept for the two
        // answers after the name.
        let name = |job: &str| cut(job, width.saturating_sub(34));
        let (question, yes, no) = match confirm {
            Confirm::Stop { job, .. } => (format!(" stop {}?", name(job)), "stop", "keep running"),
            Confirm::Delete { job } => (format!(" delete {}?", name(job)), "delete", "keep"),
            Confirm::Discard => (
                " discard the changes?".to_owned(),
                "discard",
                "keep editing",
            ),
        };
        return Line::from(vec![
            Span::styled(question, strong()),
            Span::raw("  "),
            Span::styled("y", strong()),
            Span::styled(format!(" {yes}  "), dim()),
            Span::styled("n", strong()),
            Span::styled(format!(" {no}"), dim()),
        ]);
    }
    type Key = (&'static str, &'static str);
    // A job that is running can be stopped, one that is not can be run: the
    // bar offers the one that would do something.
    let running = app.selected().is_some_and(|job| job.running().is_some());
    let job: [Key; 5] = [
        if running { ("x", "stop") } else { ("r", "run") },
        ("p", "pause"),
        ("s", "skip"),
        ("u", "resume"),
        ("e", "prompt"),
    ];
    // `always` shows whatever the width: the way back, to the help and out.
    // The actions give way to it on a narrow terminal, last first.
    let manage: [Key; 2] = [("E", "edit"), ("d", "delete")];
    let (actions, always): (Vec<Key>, [Key; 2]) = match app.screen {
        // With no job yet, creating one is all there is to do.
        Screen::Jobs if app.snapshot.jobs.is_empty() => {
            (vec![("n", "new")], [("?", "help"), ("q", "quit")])
        }
        Screen::Jobs => (
            [("enter", "open")]
                .into_iter()
                .chain(job)
                .chain([("n", "new")])
                .chain(manage)
                .collect(),
            [("?", "help"), ("q", "quit")],
        ),
        Screen::Job => (
            [("enter", "log")]
                .into_iter()
                .chain(job)
                .chain(manage)
                .collect(),
            [("esc", "back"), ("?", "help")],
        ),
        // Every letter is text here: what is left are these.
        Screen::Form => (
            vec![("tab", "next"), ("space", "mark")],
            [("ctrl-s", "save"), ("esc", "cancel")],
        ),
        Screen::Log => (
            vec![("↑↓", "scroll"), ("g", "top"), ("G", "end")],
            [("esc", "back"), ("?", "help")],
        ),
        Screen::Help => (Vec::new(), [("esc", "back"), ("q", "quit")]),
    };
    let size = |(key, what): &Key| width_in_columns(key) + 1 + width_in_columns(what) + 2;
    let mut room = width
        .saturating_sub(1)
        .saturating_sub(always.iter().map(size).sum::<usize>())
        // The last one needs no gap after it.
        + 2;
    let mut spans = vec![Span::raw(" ")];
    let mut show = |(key, what): Key| {
        spans.push(Span::styled(key, strong()));
        spans.push(Span::styled(format!(" {what}  "), dim()));
    };
    for item in actions {
        if size(&item) > room {
            break;
        }
        room -= size(&item);
        show(item);
    }
    always.into_iter().for_each(&mut show);
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
    use crate::tui::form::Edit;
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
        // What the last finished run did stays on screen while a new one goes.
        assert!(text.contains("last ok · 2m 14s"), "{text}");
        assert!(!text.contains("running since"));
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
        assert!(text.contains("y stop"));
        assert!(text.contains("n keep running"));
        assert!(!text.contains("q quit"));
    }

    #[test]
    fn every_action_of_the_screen_is_in_the_shortcut_bar() {
        let text = screen(&app(vec![job("report")]), 80, 24);
        let bar = text.lines().last().unwrap();
        for word in [
            "open", "run", "pause", "skip", "resume", "prompt", "help", "quit",
        ] {
            assert!(bar.contains(word), "{word:?} is missing from {bar:?}");
        }
    }

    #[test]
    fn the_bar_offers_run_or_stop_never_both() {
        let idle = screen(&app(vec![job("report")]), 80, 24);
        let bar = idle.lines().last().unwrap();
        assert!(bar.contains("r run"), "{bar:?}");
        assert!(!bar.contains("x stop"), "{bar:?}");

        let running = JobView {
            runs: vec![run("r1", Outcome::Running, Trigger::Manual)],
            ..job("report")
        };
        let mut app = app(vec![running]);
        let busy = screen(&app, 80, 24);
        let bar = busy.lines().last().unwrap();
        assert!(bar.contains("x stop"), "{bar:?}");
        assert!(!bar.contains("r run"), "{bar:?}");
        // The job screen follows the same rule.
        app.act(Action::Open);
        let opened = screen(&app, 80, 24);
        let bar = opened.lines().last().unwrap();
        assert!(bar.contains("x stop") && !bar.contains("r run"), "{bar:?}");
    }

    #[test]
    fn a_wide_name_does_not_push_the_line_past_the_edge() {
        let wide = JobView {
            job: Job {
                workdir: PathBuf::from(format!("/work/{}", "日本語".repeat(30))),
                args: vec!["引数".repeat(40)],
                ..job("report").job
            },
            ..job("日本語のジョブ名前はとても長いです")
        };
        let mut app = app(vec![wide]);
        for open in [false, true] {
            if open {
                app.act(Action::Open);
            }
            let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
            terminal.draw(|frame| draw(frame, &app, &now())).unwrap();
            let buffer = terminal.backend().buffer();
            // The state sits at the right edge of the first line of the entry,
            // where a line pushed too far would have lost it.
            let entry: String = (0..60).map(|x| buffer[(x, 2)].symbol()).collect();
            assert!(entry.trim_end().ends_with("active"), "{entry:?}");
            // And the last column of every row is the margin it should be.
            for y in 2..9 {
                assert_eq!(buffer[(59, y)].symbol(), " ", "row {y}");
            }
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
            "pause, skip next, resume",
            "edit the prompt",
            "new job",
            "edit, delete the job",
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
        let text = screen(&app(vec![job("report")]), 68, 24);
        let bar = text.lines().last().unwrap().trim_end();
        // The way to the help and the way out stay; an action gives way.
        assert!(bar.ends_with("? help  q quit"), "{bar:?}");
        assert!(bar.contains("u resume"));
        assert!(!bar.contains("prompt"));

        let narrow = screen(&app(vec![job("report")]), MIN.0, 24);
        let bar = narrow.lines().last().unwrap().trim_end();
        assert!(bar.ends_with("? help  q quit"), "{bar:?}");
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
        // The parser's drawing of the place is left out: it does not survive
        // being folded into a sentence.
        assert!(
            text.contains("jobs.toml: TOML parse error, invalid type"),
            "{text}"
        );
        assert!(!text.contains("jobs = 3"));
    }

    #[test]
    fn a_job_that_only_ever_started_says_since_when_it_runs() {
        let first_run = JobView {
            runs: vec![run("r1", Outcome::Running, Trigger::Manual)],
            ..job("report")
        };
        assert!(screen(&app(vec![first_run]), 80, 24).contains("running since 16:16"));
    }

    /// `report` with twelve runs, the newest of which failed.
    fn busy() -> JobView {
        let mut runs = vec![run("new", Outcome::Failed, Trigger::Manual)];
        runs.extend((0..11).map(|n| run(&format!("r{n}"), Outcome::Ok, Trigger::Manual)));
        JobView {
            runs,
            ..job("report")
        }
    }

    #[test]
    fn the_strip_ends_in_the_same_column_for_every_job() {
        let short = JobView {
            runs: vec![run("r1", Outcome::Ok, Trigger::Manual)],
            ..job("triage")
        };
        let text = screen(&app(vec![busy(), short]), 80, 24);
        let column = |needle: &str| {
            let line = text.lines().find(|line| line.contains(needle)).unwrap();
            line.chars().position(|c| c == 'l').unwrap()
        };
        let newest: Vec<usize> = text
            .lines()
            .filter(|line| line.contains(" │ next"))
            .map(|line| {
                let marks: Vec<usize> = line
                    .chars()
                    .enumerate()
                    .filter(|(_, c)| matches!(c, '✓' | '✗'))
                    .map(|(index, _)| index)
                    .collect();
                *marks.last().unwrap()
            })
            .collect();
        assert_eq!(newest.len(), 2);
        assert_eq!(newest[0], newest[1], "{text}");
        let _ = column;
    }

    #[test]
    fn the_narrowest_terminal_keeps_marks_and_the_outcome() {
        let text = screen(&app(vec![busy()]), MIN.0, MIN.1);
        let line = text.lines().find(|line| line.contains(" │ next")).unwrap();
        assert!(line.contains("failed (3)"), "{text}");
        assert!(line.contains('✗'), "{text}");
        assert!(line.contains('✓'), "{text}");
    }

    #[test]
    fn a_skip_keeps_the_strip_on_a_wide_terminal() {
        let skipping = JobView {
            next: Some(Next::Skipping {
                skipped: local("2026-10-06T16:05"),
                then: local("2026-10-07T16:05"),
            }),
            ..busy()
        };
        let text = screen(&app(vec![skipping]), 100, 24);
        let line = text
            .lines()
            .find(|line| line.contains("skips today"))
            .unwrap();
        assert!(line.contains("✓ ✗"), "{text}");
        assert!(line.contains("failed (3)"));
    }

    #[test]
    fn the_strips_stack_on_the_narrowest_terminal_too() {
        let today = busy();
        let tomorrow = JobView {
            next: Some(Next::At(local("2026-10-07T16:05"))),
            ..JobView {
                name: "triage".to_owned(),
                ..busy()
            }
        };
        let text = screen(&app(vec![today, tomorrow]), MIN.0, 24);
        let newest: Vec<usize> = text
            .lines()
            .filter(|line| line.contains(" │ next"))
            .map(|line| line.chars().position(|c| c == '✗').unwrap())
            .collect();
        assert_eq!(newest.len(), 2, "{text}");
        assert_eq!(newest[0], newest[1], "{text}");
    }

    #[test]
    fn the_folio_counts_what_the_error_block_hides() {
        let mut app = app(vec![job("alpha"), job("beta"), job("gamma")]);
        // Ten rows of body hold the three entries, but not under an error.
        assert!(!screen(&app, 80, 14).contains(" of 3"));
        app.snapshot.error = Some("invalid config jobs.toml: job x".to_owned());
        assert!(screen(&app, 80, 14).contains("jobs · 1 of 3"));
    }

    #[test]
    fn the_facts_of_a_job_give_way_to_its_runs() {
        let mut app = on_the_job();
        app.snapshot.error = Some("invalid config jobs.toml: job x".to_owned());
        let small = screen(&app, MIN.0, MIN.1);
        assert!(small.contains("▸ ✓"), "{small}");
        assert!(!small.contains("workdir"));
        // With room, they are all there.
        assert!(screen(&app, 80, 24).contains("workdir"));
    }

    #[test]
    fn a_weekday_reads_in_lowercase_like_everything_else() {
        let later = JobView {
            next: Some(Next::At(local("2026-10-12T16:05"))),
            ..job("report")
        };
        assert!(screen(&app(vec![later]), 80, 24).contains("next mon 16:05"));
    }

    #[test]
    fn the_job_screen_opens_with_the_entry_of_the_job() {
        let text = screen(&on_the_job(), 80, 24);
        let lines: Vec<&str> = text.lines().collect();
        let entry = lines
            .iter()
            .position(|line| line.contains("  report") && line.contains("claude"))
            .unwrap();
        assert!(
            lines[entry + 1].starts_with(" │ next today 16:05"),
            "{text}"
        );
        assert!(lines[entry + 1].contains("last ok"));
        assert!(lines[entry + 2].starts_with(" │ workdir"), "{text}");
        assert!(lines[entry + 3].starts_with(" │ prompt"));
        assert!(lines[entry + 4].starts_with(" │ args"));
        assert!(lines[entry + 5].starts_with(" ├──"));
    }

    #[test]
    fn the_output_of_a_run_sits_behind_the_margin_rule() {
        let app = on_the_log(Log {
            text: "first line\n".to_owned(),
            truncated: false,
        });
        let text = screen(&app, 80, 24);
        assert!(text.contains(" │ first line"), "{text}");
        assert!(!text.contains("────"));
    }

    #[test]
    fn a_line_of_output_wider_than_the_terminal_goes_on_below() {
        let long = format!("{}THE-END\n", "x".repeat(152));
        let mut app = on_the_log(Log {
            text: long,
            truncated: false,
        });
        app.columns = columns(80);
        let text = screen(&app, 80, 24);
        assert!(text.contains("THE-END"), "{text}");
    }

    #[test]
    fn the_log_screen_says_where_it_is_in_the_output() {
        let text: String = (0..100).map(|n| format!("line {n:03}\n")).collect();
        let mut app = on_the_log(Log {
            text,
            truncated: true,
        });
        let end = screen(&app, 80, 24);
        assert!(end.contains("81–100 of 100"), "{end}");
        assert!(end.contains("showing the last 1 MiB"));
        app.act(Action::Top);
        assert!(screen(&app, 80, 24).contains("1–20 of 100"));
    }

    #[test]
    fn the_help_fits_the_smallest_terminal_and_explains_the_marks() {
        let mut app = app(vec![job("report")]);
        app.act(Action::Help);
        let text = screen(&app, MIN.0, MIN.1);
        for expected in [
            "run the job now",
            "edit the prompt",
            "move",
            "this help",
            "quit",
            "✓ ok",
            "✗ failed",
            "! interrupted",
            "● running",
            "· did not start the agent",
        ] {
            assert!(text.contains(expected), "{expected:?} is missing:\n{text}");
        }
    }

    #[test]
    fn an_error_breaks_between_words_and_says_the_list_is_old() {
        let mut app = app(vec![job("report")]);
        let words: Vec<String> = (0..60).map(|n| format!("word{n:02}")).collect();
        app.snapshot.error = Some(words.join(" "));
        let text = screen(&app, 80, 24);
        let first = text.lines().find(|line| line.contains("word00")).unwrap();
        assert!(first.starts_with(" ! word00"), "{text}");
        // No word is cut in two at the edge.
        assert!(
            first.trim_end().ends_with(|c: char| c.is_ascii_digit()),
            "{first:?}"
        );
        assert!(text.contains('…'), "{text}");
        assert!(!text.contains("word59"));
        assert!(text.contains("last valid read"), "{text}");
    }

    #[test]
    fn an_error_names_the_jobs_file_without_its_directory() {
        let mut app = app(vec![job("report")]);
        app.snapshot.error = Some(
            "invalid config /home/me/.config/otto/jobs.toml: job report: schedule.at must be HH:MM"
                .to_owned(),
        );
        let text = screen(&app, MIN.0, 24);
        assert!(
            text.contains(" ! invalid config jobs.toml: job report:"),
            "{text}"
        );
        assert!(text.contains("be HH:MM"));
    }

    #[test]
    fn the_two_columns_of_the_help_do_not_touch() {
        let mut app = app(vec![job("report")]);
        app.act(Action::Help);
        let text = screen(&app, MIN.0, MIN.1);
        assert!(text.contains("stop the run in progress  ?"), "{text}");
        assert!(text.contains("pause, skip next, resume  q"), "{text}");
    }

    #[test]
    fn a_long_list_says_where_the_selection_is() {
        let jobs = (0..20).map(|n| job(&format!("job-{n:02}"))).collect();
        let mut app = app(jobs);
        assert!(screen(&app, 80, 14).contains("jobs · 1 of 20"));
        app.act(Action::Bottom);
        assert!(screen(&app, 80, 14).contains("jobs · 20 of 20"));
        // A list that fits says nothing of the kind.
        assert!(!screen(&self::app(vec![job("report")]), 80, 24).contains(" of 1"));
    }

    #[test]
    fn a_notice_too_long_for_the_line_ends_in_an_ellipsis() {
        let mut app = app(vec![job("report")]);
        app.notice = Some(Notice {
            text: "x".repeat(200),
            error: true,
        });
        let text = screen(&app, 80, 24);
        let line = text.lines().find(|line| line.contains("xxx")).unwrap();
        assert!(line.trim_end().ends_with('…'), "{line:?}");
    }

    const FILE: &str = include_str!("../../examples/jobs.toml");

    /// The form for a new job, with these typed into the name.
    fn creating(name: &str) -> App {
        let mut app = app(vec![job("report")]);
        app.open_form(Ok(FILE.to_owned()), None);
        for c in name.chars() {
            app.act(Action::Input(Edit::Insert(c)));
        }
        app
    }

    /// The screen, and where the terminal's cursor was left.
    fn screen_and_cursor(app: &App, width: u16, height: u16) -> (String, (u16, u16)) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app, &now())).unwrap();
        let position = terminal.get_cursor_position().unwrap();
        (screen(app, width, height), (position.x, position.y))
    }

    #[test]
    fn the_form_is_one_opened_entry_with_a_fact_to_a_field() {
        let mut app = app(vec![job("report")]);
        app.open_form(Ok(FILE.to_owned()), Some("linear-updates"));
        let text = screen(&app, MIN.0, MIN.1);
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert_eq!(
            lines[0].split("  ").next(),
            Some(" otto · edit linear-updates")
        );
        assert_eq!(
            &lines[2..10],
            [
                " │ name     linear-updates",
                "▸│ agent    ✓claude  ·codex",
                " │ workdir  ~/Workspace/app",
                " │ at       16:05",
                " │ days     ✓mon ✓tue ✓wed ✓thu ✓fri ·sat ·sun",
                " │ args     --permission-mode ⏎ auto",
                " │ prompt   prompts/linear-updates.md",
                " ├─────────────────────────────────────────────────────────",
            ]
        );
        assert_eq!(lines[11], " tab next  space mark  ctrl-s save  esc cancel");
    }

    #[test]
    fn the_cursor_is_where_typing_goes() {
        let app = creating("ni");
        let (text, cursor) = screen_and_cursor(&app, 80, 24);
        assert!(text.contains("▸│ name     ni"), "{text}");
        assert!(text.contains("otto · new job"));
        assert_eq!(cursor, (14, 2));

        // On a choice it sits on the one it would mark.
        let mut app = creating("");
        for _ in 0..4 {
            app.act(Action::NextField);
        }
        app.act(Action::Input(Edit::Right));
        app.act(Action::Input(Edit::Right));
        app.act(Action::Toggle);
        let (text, cursor) = screen_and_cursor(&app, 80, 24);
        assert!(text.contains("▸│ days     ·mon ·tue ✓wed ·thu"), "{text}");
        assert_eq!(cursor, (22, 6));
    }

    #[test]
    fn a_text_longer_than_its_room_shows_where_the_cursor_is() {
        let mut app = creating("x");
        app.act(Action::NextField);
        app.act(Action::NextField);
        let long = format!("~/{}/the-end", "deep/".repeat(30));
        for c in long.chars() {
            app.act(Action::Input(Edit::Insert(c)));
        }
        let (text, cursor) = screen_and_cursor(&app, MIN.0, MIN.1);
        let row = text.lines().find(|line| line.contains("workdir")).unwrap();
        assert!(row.trim_end().ends_with("the-end"), "{row:?}");
        assert!(row.contains('…'));
        // After the last character, a column short of the edge.
        assert_eq!(cursor, (MIN.0 - 2, 4));

        // From the start of the field, the start is what shows.
        app.act(Action::Input(Edit::Home));
        let (text, cursor) = screen_and_cursor(&app, MIN.0, MIN.1);
        let row = text.lines().find(|line| line.contains("workdir")).unwrap();
        assert!(row.contains("workdir  ~/deep/"), "{row:?}");
        assert_eq!(cursor, (12, 4));
    }

    #[test]
    fn the_form_says_what_saving_would_run_into() {
        let mut app = creating("nightly");
        app.show_warnings(vec![
            "working directory not found: /nowhere".to_owned(),
            "codex not found in PATH".to_owned(),
        ]);
        let text = screen(&app, 80, 24);
        assert!(
            text.contains(" note: working directory not found: /nowhere; codex not found in PATH"),
            "{text}"
        );
        // On the arguments, what they are for comes first.
        for _ in 0..5 {
            app.act(Action::NextField);
        }
        let text = screen(&app, 80, 24);
        assert!(text.contains("▸│ args"));
        assert!(text.contains("a scheduled run has nobody to answer a permission prompt"));
        // And why it could not be saved comes before both.
        app.act(Action::Save);
        let text = screen(&app, 80, 24);
        assert!(!text.contains("nobody to answer"));
        assert!(
            text.contains(" job nightly: schedule.at must be HH:MM"),
            "{text}"
        );
    }

    #[test]
    fn the_questions_of_the_form_name_what_each_answer_does() {
        let mut app = creating("nightly");
        app.act(Action::Back);
        let (text, cursor) = screen_and_cursor(&app, 80, 24);
        assert!(text.contains(" discard the changes?  y discard  n keep editing"));
        // No cursor in a field while a question waits.
        assert_ne!(cursor, (19, 2));

        let mut list = self::app(vec![job("report")]);
        list.act(Action::Delete);
        assert!(screen(&list, 80, 24).contains(" delete report?  y delete  n keep"));
    }

    #[test]
    fn the_job_list_offers_to_create_edit_and_delete() {
        let empty = screen(&app(Vec::new()), 80, 24);
        assert_eq!(
            empty.lines().last().unwrap().trim_end(),
            " n new  ? help  q quit"
        );

        let wide = screen(&app(vec![job("report")]), 100, 24);
        let bar = wide.lines().last().unwrap();
        for key in ["n new", "E edit", "d delete"] {
            assert!(bar.contains(key), "{key:?} is missing from {bar:?}");
        }
    }

    #[test]
    fn the_smallest_terminal_still_shows_a_job() {
        let text = screen(&app(vec![report()]), MIN.0, MIN.1);
        assert!(text.contains("▸ report"));
        assert!(text.contains("failed (3)"));
    }
}
