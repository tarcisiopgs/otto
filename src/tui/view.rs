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
use crate::notify::Level;
use crate::store::{Outcome, Run};
use crate::sync;
use crate::tui::app::{App, Confirm, ENTRY, Screen};
use crate::tui::form::{Field, Focus, Form};
use crate::tui::text::{cut, fit, tail, width as width_in_columns, wrapped};
use crate::tui::world::{JobView, Pending};

/// The smallest terminal the UI is drawn on: columns, then rows.
pub const MIN: (u16, u16) = (60, 12);

/// How many of a job's latest runs its strip of marks shows.
const STRIP: usize = 8;

/// The rows of the job screen above its runs: the entry, four facts, a rule.
const SHEET: usize = 7;

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
        Screen::Sync => changes(app, width, rows),
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
/// What is at the right is what the line is read for: the end of `left` gives
/// way to it.
fn between<'a>(mut left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: usize) -> Line<'a> {
    let room = width.saturating_sub(width_of(&right) + 3);
    if width_of(&left) > room
        && let Some(last) = left.pop()
    {
        let kept = room.saturating_sub(width_of(&left));
        left.push(Span::styled(cut(&last.content, kept), last.style));
    }
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
        Screen::Sync => "sync".to_owned(),
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
/// the last run ended. A job the scheduler does not have as the file has it
/// says so where the coming run would be: that run is the file's word, not
/// yet the scheduler's.
///
/// With room, the strip ends in a fixed column, so the newest run of every
/// job is read down one column. Without it the strips still stack, after a
/// coming run given the room of an ordinary one, and the ending gives up its
/// duration, then the word `last`. Only then does the line let go of the
/// shared column, and last of all of its oldest marks.
fn second_line(
    job: &JobView,
    pending: Option<&Pending>,
    now: &Zoned,
    width: usize,
) -> Line<'static> {
    let lead = vec![
        Span::styled(" │ ", dim()),
        match pending.map(|pending| &pending.change) {
            Some(Ok(_)) => Span::styled("not applied", strong()),
            Some(Err(_)) => Span::styled("sync error", bad()),
            None => Span::raw(coming(job.next.as_ref(), now)),
        },
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
/// whole sheet; a row short of it, the sheet without how the job tells of
/// its runs; and only the entry and the rule when the runs would be left with
/// too few.
fn sheet(left: usize) -> usize {
    if left >= SHEET + RUNS {
        SHEET
    } else if left >= SHEET - 1 + RUNS {
        SHEET - 1
    } else {
        ENTRY
    }
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
        let pending = app.snapshot.pending_for(&job.name);
        lines.push(second_line(job, pending, now, width));
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
    let pending = app.snapshot.pending_for(&job.name);
    lines.push(second_line(job, pending, now, width));
    // On a short terminal the facts give way to the runs.
    let spent = sheet(rows.saturating_sub(lines.len() - 2));
    if spent > ENTRY {
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
    if spent == SHEET {
        lines.push(fact("notify", job.job.notify.label().to_owned()));
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

/// What a sync would do with a job, in the stem of what `otto sync` reports
/// once it did it: `add` before, `added` after.
fn stem(action: sync::Action) -> &'static str {
    match action {
        sync::Action::Add => "add",
        sync::Action::Update => "update",
        sync::Action::Remove => "remove",
        sync::Action::Unchanged => "unchanged",
        sync::Action::Busy => "busy",
    }
}

/// The sync: each job it touches as the first line of its entry, with what
/// is done to it where the state sits, and under it why it cannot be done.
fn changes(app: &App, width: usize, rows: usize) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    for change in app.changes() {
        let mut left = vec![Span::raw("  "), Span::raw(fit(&change.job, 20))];
        // A unit whose job is gone has only its name left.
        if let Some(job) = app.snapshot.jobs.iter().find(|job| job.name == change.job) {
            left.push(Span::raw(format!("  {:<8}", job.job.agent.program())));
            left.push(Span::raw(schedule(&job.job.schedule)));
        }
        let word = match &change.change {
            Ok(action) if app.applied() => Span::raw(action.label()),
            Ok(action) => Span::raw(stem(*action)),
            Err(_) => Span::styled("error", bad()),
        };
        lines.push(between(left, vec![word], width));
        if let Err(reason) = &change.change {
            for row in app.reason_rows(reason) {
                lines.push(Line::from(vec![Span::styled(" │ ", dim()), Span::raw(row)]));
            }
        }
    }
    lines.push(rule(width));
    lines.into_iter().skip(app.sync_top()).take(rows).collect()
}

/// What the sync screen says under its list: what was done, once it was,
/// where the screen is in a list that does not fit, and what `busy` leaves
/// unsaid. What does not fit in `room` is left out whole, last first.
fn about_the_sync(app: &App, room: usize) -> Line<'static> {
    use sync::Action::{Add, Busy, Remove, Update};
    let all = app.changes();
    let mut parts = Vec::new();
    if app.applied() {
        let done = all
            .iter()
            .filter(|change| matches!(change.change, Ok(Add | Update | Remove)))
            .count();
        // What happened, so in the default foreground.
        parts.push(Span::raw(match all.len() - done {
            0 => format!("{done} applied"),
            not => format!("{done} applied, {not} not"),
        }));
    }
    let total = app.sync_rows();
    if total > app.page {
        let first = app.sync_top();
        let last = (first + app.page).min(total);
        parts.push(Span::styled(
            format!("{}–{last} of {total}", first + 1),
            dim(),
        ));
    }
    if all.iter().any(|change| change.change == Ok(Busy)) {
        parts.push(Span::styled("busy: sync again after its run ends", dim()));
    }
    let mut spans = vec![Span::raw(" ")];
    let mut used = 0;
    for part in parts {
        let gap = if used == 0 { 0 } else { 3 };
        if used + gap + part.width() > room {
            break;
        }
        if gap > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        used += gap + part.width();
        spans.push(part);
    }
    Line::from(spans)
}

/// Where the value of a field starts: after the marker, the margin rule and
/// the label.
const VALUE: usize = 12;

/// A field that takes text, as much of it as fits, and where its cursor is.
/// A text longer than the room shows the part the cursor is in, with a mark
/// at each end that was cut.
fn typed(field: &Field, room: usize) -> (Vec<Span<'static>>, usize) {
    // A line break is one argument ending and the next beginning.
    let shown = |text: &str| text.replace('\n', " ⏎ ");
    let before = shown(&field.text()[..field.cursor()]);
    let after = shown(&field.text()[field.cursor()..]);
    // Room for the cursor itself, which sits after the text.
    let room = room.saturating_sub(1).max(1);
    // Some of the room is kept for what follows the cursor, so that being
    // in the middle of a text does not hide the rest of it.
    let kept = width_in_columns(&after).min(room / 3);
    let before = tail(&before, room - kept);
    let column = width_in_columns(&before);
    let after = cut(&after, room - column);
    // The mark between two arguments is neither of them.
    let mut spans = Vec::new();
    for (index, piece) in format!("{before}{after}").split('⏎').enumerate() {
        if index > 0 {
            spans.push(Span::styled("⏎", dim()));
        }
        spans.push(Span::raw(piece.to_owned()));
    }
    (spans, column)
}

/// A choice among a few: the chosen ones carry a mark, the others a dot.
fn choice(name: &str, chosen: bool) -> Vec<Span<'static>> {
    if chosen {
        vec![Span::raw("✓"), Span::styled(name.to_owned(), strong())]
    } else {
        vec![Span::styled(format!("·{name}"), dim())]
    }
}

/// The form: one opened entry, each field a fact behind the margin rule in
/// the order an entry reads, and where on it the cursor goes, as a column and
/// a row of the body.
fn fields(form: &Form, width: usize) -> (Vec<Line<'_>>, (usize, usize)) {
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let room = width.saturating_sub(VALUE + 1);
    let mut cursor = (VALUE, 0);
    let mut lines = Vec::new();
    let order = [
        (Focus::Name, "name"),
        (Focus::Agent, "agent"),
        (Focus::At, "at"),
        (Focus::Days, "days"),
        (Focus::Workdir, "workdir"),
        (Focus::Prompt, "prompt"),
        (Focus::Args, "args"),
        (Focus::Notify, "notify"),
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
            Focus::Agent | Focus::Days | Focus::Notify => None,
        };
        match (focus, text) {
            // Arguments the field cannot hold are not shown as if it could.
            (Focus::Args, _) if form.args_by_hand => {
                spans.push(Span::styled("written by hand in the jobs file", dim()));
            }
            (_, Some(field)) => {
                let (shown, at) = typed(field, room);
                column = at;
                // A job that exists keeps its name: it is shown, not offered.
                let fixed = focus == Focus::Name && form.editing.is_some();
                spans.extend(shown.into_iter().map(|span| match fixed {
                    true => span.style(dim()),
                    false => span,
                }));
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
            (Focus::Notify, _) => {
                for (index, level) in Level::ALL.into_iter().enumerate() {
                    if index > 0 {
                        spans.push(Span::raw("  "));
                    }
                    if level == form.notify {
                        column = width_of(&spans) - VALUE;
                    }
                    spans.extend(choice(level.label(), level == form.notify));
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
                // None marked is not no day: it is said in the list's word.
                if form.spec().days.is_empty() {
                    spans.push(Span::styled("  daily", dim()));
                }
            }
        }
        if focused {
            cursor = (VALUE + column, row);
        }
        lines.push(Line::from(spans));
    }
    // On the smallest terminal the fields take every row, and this rule is
    // cut off by the height of the body.
    lines.push(rule(width));
    (lines, cursor)
}

/// What the field in focus calls for, in a line.
fn hint(form: &Form) -> Option<&'static str> {
    match form.focus {
        Focus::Name if form.editing.is_some() => None,
        Focus::Name => Some("lowercase letters, digits and dashes"),
        Focus::Agent => None,
        Focus::At => Some("24-hour time, as in 16:05"),
        Focus::Days => Some("no day marked runs every day"),
        Focus::Workdir => Some("the directory the agent runs in"),
        Focus::Prompt => Some("created and opened in your editor if it is not there"),
        Focus::Args if form.args_by_hand => {
            Some("these arguments can only be changed in the jobs file")
        }
        Focus::Args => Some("a scheduled run has nobody to answer a permission prompt"),
        Focus::Notify => Some(match form.notify {
            Level::Off => "never tells",
            Level::Failures => "tells when a run fails",
            Level::Finish => "tells when a run ends, ok or failed",
            Level::All => "tells when a run starts, ends or does not happen",
        }),
    }
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
    ("n E d", "new, edit, delete a job"),
    ("S", "what a sync would do"),
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
    // On the form: why it could not be saved, until the job changes; what
    // the arguments are for, which is never out of sight on that field; what
    // saving would run into; and what the field in focus calls for.
    if let (Screen::Form, Some(form)) = (&app.screen, &app.form) {
        if let Some(reason) = &app.reason {
            return Line::styled(format!(" {}", cut(reason, room)), bad());
        }
        let note = || format!("note: {}", app.warnings.join("; "));
        let text = match (form.focus, hint(form)) {
            // Never out of sight on these two: what the arguments are for,
            // and what the level under the cursor does.
            (Focus::Args | Focus::Notify, Some(hint)) => hint.to_owned(),
            _ if !app.warnings.is_empty() => note(),
            (_, Some(hint)) => hint.to_owned(),
            (_, None) => return Line::default(),
        };
        return Line::raw(format!(" {}", cut(&text, room)));
    }
    if app.screen == Screen::Sync {
        return about_the_sync(app, room);
    }
    // The list says that the scheduler is behind the jobs file, and the way
    // to the screen that shows by how much. What the sync cannot do is not a
    // change waiting to be applied, and is counted apart.
    let pending = &app.snapshot.pending;
    if app.screen == Screen::Jobs && !pending.is_empty() {
        let errors = pending
            .iter()
            .filter(|pending| pending.change.is_err())
            .count();
        let count = |many: usize, what: &str| {
            let plural = if many == 1 { "" } else { "s" };
            format!("{many} {what}{plural}")
        };
        let said = match (pending.len() - errors, errors) {
            (changes, 0) => format!("{} not applied", count(changes, "change")),
            (0, errors) => format!("{} in the sync", count(errors, "error")),
            (changes, errors) => format!(
                "{} not applied, {}",
                count(changes, "change"),
                count(errors, "error")
            ),
        };
        return Line::from(vec![
            Span::raw(format!(" {said}: ")),
            Span::styled("S", strong()),
            Span::raw(" to review"),
        ]);
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
            Confirm::Apply { changes } => {
                let noun = if *changes == 1 { "change" } else { "changes" };
                (format!(" apply {changes} {noun}?"), "apply", "not now")
            }
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
    let chosen = app.selected();
    let running = chosen.is_some_and(|job| job.running().is_some());
    let state = chosen.map(|job| job.state).unwrap_or_default();
    let mut job: Vec<Key> = vec![if running { ("x", "stop") } else { ("r", "run") }];
    if !state.paused {
        job.push(("p", "pause"));
    }
    if !state.paused && !state.skip_next {
        job.push(("s", "skip"));
    }
    if state.paused || state.skip_next {
        job.push(("u", "resume"));
    }
    job.push(("e", "prompt"));
    // `always` shows whatever the width: the way back, to the help and out.
    // The actions give way to it on a narrow terminal, last first.
    let manage: [Key; 2] = [("E", "edit"), ("d", "delete")];
    // First, so that a narrow bar keeps it: it is what is waiting.
    let behind: Option<Key> = (!app.snapshot.pending.is_empty()).then_some(("S", "sync"));
    let (actions, always): (Vec<Key>, [Key; 2]) = match app.screen {
        // With no job yet, creating one is all there is to do, unless a
        // unit was left behind by a job that is gone.
        Screen::Jobs if app.snapshot.jobs.is_empty() => (
            behind.into_iter().chain([("n", "new")]).collect(),
            [("?", "help"), ("q", "quit")],
        ),
        Screen::Jobs => (
            behind
                .into_iter()
                .chain([("enter", "open")])
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
        // Every letter is text here: what is left are these, and what the
        // field in focus takes besides text.
        Screen::Form => {
            let mut keys: Vec<Key> = vec![("tab", "next")];
            match app.form.as_ref().map(|form| form.focus) {
                Some(Focus::Agent | Focus::Days | Focus::Notify) => {
                    keys.extend([("←→", "move"), ("space", "mark")])
                }
                // A line of its own for the next argument, where the field
                // takes typing at all.
                Some(Focus::Args) if !app.form.as_ref().is_some_and(|form| form.args_by_hand) => {
                    keys.push(("enter", "line"));
                }
                _ => {}
            }
            (keys, [("ctrl-s", "save"), ("esc", "cancel")])
        }
        Screen::Log => (
            vec![("↑↓", "scroll"), ("g", "top"), ("G", "end")],
            [("esc", "back"), ("?", "help")],
        ),
        Screen::Sync => {
            let mut keys: Vec<Key> = Vec::new();
            if !app.applied() && app.applicable() > 0 {
                keys.push(("a", "apply"));
            }
            if app.sync_rows() > app.page {
                keys.push(("↑↓", "scroll"));
            }
            (keys, [("esc", "back"), ("?", "help")])
        }
        Screen::Help => (Vec::new(), [("esc", "back"), ("q", "quit")]),
    };
    let size = |(key, what): &Key| width_in_columns(key) + 1 + width_in_columns(what) + 2;
    // A column before the first, one after the last, and the last needs no
    // gap after it: what is left is the width less what always shows.
    let mut room = width.saturating_sub(always.iter().map(size).sum::<usize>());
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
    use crate::notify::Level;
    use crate::store::{Outcome, Run, State, Trigger};
    use crate::sync;
    use crate::tui::app::{Action, Notice};
    use crate::tui::form::{Edit, Focus};
    use crate::tui::world::{JobView, Log, Pending, Snapshot};

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
                notify: crate::notify::Level::default(),
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
            ..Snapshot::default()
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
        for word in ["open", "run", "pause", "skip", "prompt", "help", "quit"] {
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
    fn the_bar_offers_only_what_would_change_the_schedule() {
        let bar = |state: State| {
            let job = JobView {
                state,
                ..job("report")
            };
            let text = screen(&app(vec![job]), 100, 24);
            text.lines().last().unwrap().to_owned()
        };
        let active = bar(State::default());
        assert!(active.contains("p pause") && active.contains("s skip"));
        assert!(!active.contains("u resume"), "{active:?}");
        let paused = bar(State {
            paused: true,
            skip_next: false,
        });
        assert!(paused.contains("u resume"));
        assert!(
            !paused.contains("p pause") && !paused.contains("s skip"),
            "{paused:?}"
        );
        let skipping = bar(State {
            paused: false,
            skip_next: true,
        });
        assert!(skipping.contains("p pause") && skipping.contains("u resume"));
        assert!(!skipping.contains("s skip"), "{skipping:?}");
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
            "new, edit, delete a job",
            "what a sync would do",
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
        assert!(bar.contains("e prompt"));
        assert!(!bar.contains("n new"));
        // A column is kept at the end, as on every other row.
        assert!(bar.chars().count() < 68, "{bar:?}");

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
        assert_eq!(lines[entry + 5].trim_end(), " │ notify   failures");
        assert!(lines[entry + 6].starts_with(" ├──"));
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

    /// Puts the focus of the form on a field.
    fn on(app: &mut App, focus: Focus) {
        app.form.as_mut().unwrap().focus = focus;
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
        // In the order an entry reads: what and when, then where and with what.
        // The smallest terminal has a row for each field and none for the
        // rule under them.
        assert_eq!(
            &lines[2..10],
            [
                " │ name     linear-updates",
                "▸│ agent    ✓claude  ·codex",
                " │ at       16:05",
                " │ days     ✓mon ✓tue ✓wed ✓thu ✓fri ·sat ·sun",
                " │ workdir  ~/Workspace/app",
                " │ prompt   prompts/linear-updates.md",
                " │ args     --permission-mode ⏎ auto",
                " │ notify   ·off  ·failures  ✓finish  ·all",
            ]
        );
        let taller = screen(&app, 78, 20);
        let tall: Vec<&str> = taller.lines().collect();
        assert!(tall[9].starts_with(" │ notify"), "{taller}");
        assert!(tall[10].starts_with(" ├──"), "{taller}");
        assert_eq!(
            lines[11],
            " tab next  ←→ move  space mark  ctrl-s save  esc cancel"
        );
    }

    #[test]
    fn the_bar_of_the_form_follows_the_field() {
        let mut app = creating("x");
        let bar = |app: &App| {
            let text = screen(app, MIN.0, MIN.1);
            text.lines().last().unwrap().trim_end().to_owned()
        };
        // Space is a space in a text.
        assert_eq!(bar(&app), " tab next  ctrl-s save  esc cancel");
        on(&mut app, Focus::Days);
        assert_eq!(
            bar(&app),
            " tab next  ←→ move  space mark  ctrl-s save  esc cancel"
        );
        on(&mut app, Focus::Args);
        assert_eq!(bar(&app), " tab next  enter line  ctrl-s save  esc cancel");
    }

    #[test]
    fn no_day_marked_reads_as_daily() {
        let mut app = creating("x");
        assert!(screen(&app, MIN.0, MIN.1).contains("·fri ·sat ·sun  daily"));
        on(&mut app, Focus::Days);
        app.act(Action::Toggle);
        assert!(!screen(&app, MIN.0, MIN.1).contains("daily"));
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
        on(&mut app, Focus::Days);
        app.act(Action::Input(Edit::Right));
        app.act(Action::Input(Edit::Right));
        app.act(Action::Toggle);
        let (text, cursor) = screen_and_cursor(&app, 80, 24);
        assert!(text.contains("▸│ days     ·mon ·tue ✓wed ·thu"), "{text}");
        assert_eq!(cursor, (22, 5));
    }

    #[test]
    fn a_text_longer_than_its_room_shows_where_the_cursor_is() {
        let mut app = creating("x");
        on(&mut app, Focus::Workdir);
        let long = format!("~/{}/the-end", "deep/".repeat(30));
        for c in long.chars() {
            app.act(Action::Input(Edit::Insert(c)));
        }
        let row = |app: &App| {
            let (text, cursor) = screen_and_cursor(app, MIN.0, MIN.1);
            let row = text.lines().find(|line| line.contains("workdir")).unwrap();
            (row.trim_end().to_owned(), cursor)
        };
        let (shown, cursor) = row(&app);
        assert!(shown.ends_with("the-end"), "{shown:?}");
        assert!(shown.contains("workdir  …"), "{shown:?}");
        // After the last character, a column short of the edge.
        assert_eq!(cursor, (MIN.0 - 2, 6));

        // In the middle, both ends say they were cut.
        for _ in 0..20 {
            app.act(Action::Input(Edit::Left));
        }
        let (shown, cursor) = row(&app);
        assert!(
            shown.contains("workdir  …") && shown.ends_with('…'),
            "{shown:?}"
        );
        assert!(cursor.0 < MIN.0 - 2);

        // From the start of the field, the start is what shows.
        app.act(Action::Input(Edit::Home));
        let (shown, cursor) = row(&app);
        assert!(
            shown.contains("workdir  ~/deep/") && shown.ends_with('…'),
            "{shown:?}"
        );
        assert_eq!(cursor, (12, 6));
    }

    #[test]
    fn the_form_says_what_saving_would_run_into() {
        let mut app = creating("nightly");
        on(&mut app, Focus::Agent);
        app.show_warnings(vec![
            "workdir not found".to_owned(),
            "codex not found in PATH".to_owned(),
        ]);
        let text = screen(&app, MIN.0, MIN.1);
        assert!(
            text.contains(" note: workdir not found; codex not found in PATH"),
            "{text}"
        );
        // On the arguments, what they are for is never out of sight.
        on(&mut app, Focus::Args);
        let text = screen(&app, 80, 24);
        assert!(text.contains("▸│ args"));
        assert!(text.contains("a scheduled run has nobody to answer a permission prompt"));
        // Why it could not be saved comes before both, and stays.
        app.act(Action::Save);
        app.act(Action::NextField);
        let text = screen(&app, 80, 24);
        assert!(!text.contains("nobody to answer"));
        assert!(text.contains(" workdir is empty"), "{text}");
    }

    #[test]
    fn a_field_says_what_it_calls_for() {
        let mut app = creating("");
        let notice = |app: &App| {
            let text = screen(app, MIN.0, MIN.1);
            text.lines().nth(10).unwrap().trim_end().to_owned()
        };
        assert_eq!(notice(&app), " lowercase letters, digits and dashes");
        on(&mut app, Focus::At);
        assert_eq!(notice(&app), " 24-hour time, as in 16:05");
        on(&mut app, Focus::Days);
        assert_eq!(notice(&app), " no day marked runs every day");
        on(&mut app, Focus::Agent);
        assert_eq!(notice(&app), "");
    }

    #[test]
    fn arguments_the_field_cannot_hold_are_said_to_be_by_hand() {
        let text = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = { at = \"07:00\" }\nargs = [\"-p\", \" padded \"]\n";
        let mut app = app(vec![job("a")]);
        app.open_form(Ok(text.to_owned()), Some("a"));
        on(&mut app, Focus::Args);
        let shown = screen(&app, MIN.0, MIN.1);
        assert!(
            shown.contains("▸│ args     written by hand in the jobs file"),
            "{shown}"
        );
        assert!(shown.contains(" these arguments can only be changed in the jobs file"));
        // A key that would do nothing there is not offered.
        assert!(
            shown.contains(" tab next  ctrl-s save  esc cancel"),
            "{shown}"
        );
        assert!(!shown.contains("padded"));
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

    fn pend(job: &str, change: Result<sync::Action, &str>) -> Pending {
        Pending {
            job: job.to_owned(),
            change: change.map_err(str::to_owned),
        }
    }

    /// `report` and `triage` in the jobs file, with this still to apply.
    fn unsynced(pending: Vec<Pending>) -> App {
        App::new(Snapshot {
            config: PathBuf::from("/home/me/.config/otto/jobs.toml"),
            jobs: vec![job("report"), job("triage")],
            pending,
            can_sync: true,
            ..Snapshot::default()
        })
    }

    /// Every kind of change at once.
    fn every_change() -> Vec<Pending> {
        vec![
            pend("report", Ok(sync::Action::Add)),
            pend(
                "triage",
                Err("prompt file not found: /home/me/work/prompts/triage.md"),
            ),
            pend("nightly", Ok(sync::Action::Busy)),
            pend("old", Ok(sync::Action::Remove)),
        ]
    }

    #[test]
    fn a_job_that_is_not_applied_says_so_on_its_entry() {
        let app = unsynced(vec![pend("report", Ok(sync::Action::Update))]);
        let text = screen(&app, 78, 20);
        let rows: Vec<&str> = text.lines().collect();
        // Where its coming run would be: the scheduler has no such run yet.
        assert!(rows[3].starts_with(" │ not applied"), "{text}");
        assert!(!rows[3].contains("next today"), "{text}");
        assert!(rows[6].starts_with(" │ next today 16:05"), "{text}");
        assert!(!rows[6].contains("not applied"), "{text}");
        assert!(
            rows[18].starts_with(" 1 change not applied: S to review"),
            "{text}"
        );
        assert!(rows[19].starts_with(" S sync  enter open"), "{text}");
    }

    #[test]
    fn a_unit_to_remove_counts_though_it_has_no_entry() {
        let app = unsynced(vec![
            pend("report", Ok(sync::Action::Add)),
            pend("old", Ok(sync::Action::Remove)),
        ]);
        let text = screen(&app, 60, 12);
        assert!(
            text.contains(" 2 changes not applied: S to review"),
            "{text}"
        );
        for row in text.lines() {
            assert!(width_in_columns(row.trim_end()) < 60, "{row}");
        }
    }

    #[test]
    fn what_the_sync_cannot_do_is_an_error_not_a_change() {
        let app = unsynced(vec![
            pend("report", Ok(sync::Action::Add)),
            pend("triage", Err("prompt file not found: /prompts/triage.md")),
            pend("nightly", Ok(sync::Action::Busy)),
        ]);
        let text = screen(&app, 78, 20);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[3].starts_with(" │ not applied"), "{text}");
        assert!(rows[6].starts_with(" │ sync error"), "{text}");
        assert!(
            rows[18].starts_with(" 2 changes not applied, 1 error: S to review"),
            "{text}"
        );
        let app = unsynced(vec![
            pend("report", Err("workdir not found")),
            pend("triage", Err("workdir not found")),
        ]);
        let text = screen(&app, 60, 12);
        assert!(
            text.contains(" 2 errors in the sync: S to review"),
            "{text}"
        );
        assert!(!text.contains("not applied"), "{text}");
        let app = unsynced(vec![pend("*", Err("cannot read the units"))]);
        let text = screen(&app, 60, 12);
        assert!(text.contains(" 1 error in the sync: S to review"), "{text}");
    }

    #[test]
    fn a_job_not_applied_keeps_its_strip_in_the_column() {
        let eight = |name: &str| JobView {
            runs: (0..8)
                .map(|n| run(&format!("r{n}"), Outcome::Ok, Trigger::Manual))
                .collect(),
            ..job(name)
        };
        for (width, height) in [(78, 20), (60, 12)] {
            let app = App::new(Snapshot {
                jobs: vec![eight("report"), eight("triage")],
                pending: vec![pend("triage", Ok(sync::Action::Update))],
                can_sync: true,
                ..Snapshot::default()
            });
            let text = screen(&app, width, height);
            let newest: Vec<usize> = text
                .lines()
                .filter(|line| line.starts_with(" │ "))
                .map(|line| {
                    assert_eq!(line.matches('✓').count(), 8, "{text}");
                    line.chars()
                        .collect::<Vec<_>>()
                        .iter()
                        .rposition(|c| *c == '✓')
                        .unwrap()
                })
                .collect();
            assert_eq!(newest.len(), 2, "{text}");
            assert_eq!(newest[0], newest[1], "{text}");
        }
    }

    #[test]
    fn a_long_schedule_gives_way_to_the_word_at_the_edge() {
        let six = JobView {
            job: Job {
                schedule: Schedule {
                    at: "16:05".to_owned(),
                    days: vec![
                        Weekday::Mon,
                        Weekday::Tue,
                        Weekday::Wed,
                        Weekday::Thu,
                        Weekday::Fri,
                        Weekday::Sat,
                    ],
                },
                ..job("six-days").job
            },
            state: State {
                paused: false,
                skip_next: true,
            },
            ..job("six-days")
        };
        let mut app = App::new(Snapshot {
            jobs: vec![six],
            pending: vec![pend("six-days", Ok(sync::Action::Update))],
            can_sync: true,
            ..Snapshot::default()
        });
        let text = screen(&app, 60, 12);
        let entry = text.lines().nth(2).unwrap();
        assert!(entry.trim_end().ends_with("…  skip next"), "{text}");
        app.act(Action::Sync);
        app.columns = columns(60);
        app.page = page(12);
        let text = screen(&app, 60, 12);
        let change = text.lines().nth(2).unwrap();
        assert!(change.trim_end().ends_with("…  update"), "{text}");
        for row in text.lines() {
            assert!(width_in_columns(row.trim_end()) < 60, "{row}");
        }
    }

    #[test]
    fn with_nothing_pending_the_list_does_not_mention_the_sync() {
        let app = unsynced(Vec::new());
        let text = screen(&app, 78, 20);
        assert!(!text.contains("not applied"), "{text}");
        assert!(!text.contains("S sync"), "{text}");
    }

    #[test]
    fn a_notice_comes_before_the_reminder_of_the_sync() {
        let mut app = unsynced(vec![pend("report", Ok(sync::Action::Add))]);
        app.notice = Some(Notice {
            text: "report saved".to_owned(),
            error: false,
        });
        let text = screen(&app, 78, 20);
        assert!(text.contains(" report saved"), "{text}");
        assert!(!text.contains("S to review"), "{text}");
    }

    #[test]
    fn the_preview_lists_what_the_sync_would_do() {
        let mut app = unsynced(every_change());
        app.act(Action::Sync);
        app.columns = columns(78);
        let text = screen(&app, 78, 20);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].starts_with(" otto · sync"), "{text}");
        assert!(rows[2].starts_with("  report"), "{text}");
        assert!(rows[2].contains("claude  16:05 mon–fri"), "{text}");
        assert!(rows[2].trim_end().ends_with(" add"), "{text}");
        assert!(rows[3].trim_end().ends_with(" error"), "{text}");
        assert_eq!(
            rows[4].trim_end(),
            " │ prompt file not found: /home/me/work/prompts/triage.md"
        );
        assert!(rows[5].starts_with("  nightly"), "{text}");
        assert!(rows[5].trim_end().ends_with(" busy"), "{text}");
        assert!(rows[6].trim_end().ends_with(" remove"), "{text}");
        assert!(rows[7].starts_with(" ├──"), "{text}");
        assert!(
            rows[18].contains("busy: sync again after its run ends"),
            "{text}"
        );
        assert_eq!(rows[19].trim_end(), " a apply  esc back  ? help");
    }

    #[test]
    fn the_preview_fits_the_smallest_terminal() {
        let mut app = unsynced(every_change());
        app.act(Action::Sync);
        app.columns = columns(60);
        app.page = page(12);
        let text = screen(&app, 60, 12);
        let rows: Vec<&str> = text.lines().collect();
        for row in &rows {
            assert!(width_in_columns(row.trim_end()) < 60, "{row}");
        }
        assert_eq!(
            rows[4].trim_end(),
            " │ prompt file not found: /home/me/work/prompts/triage.md"
        );
        assert!(rows[11].starts_with(" a apply  esc back  ? help"), "{text}");
    }

    #[test]
    fn a_preview_longer_than_the_screen_offers_to_scroll() {
        let many = (0..12)
            .map(|n| pend(&format!("job-{n:02}"), Ok(sync::Action::Add)))
            .collect();
        let mut app = unsynced(many);
        app.act(Action::Sync);
        app.page = page(12);
        app.columns = columns(60);
        let text = screen(&app, 60, 12);
        assert!(text.contains("job-00"), "{text}");
        assert!(!text.contains("job-08"), "{text}");
        assert!(text.contains(" 1–8 of 12"), "{text}");
        assert!(
            text.contains(" a apply  ↑↓ scroll  esc back  ? help"),
            "{text}"
        );
        app.act(Action::Bottom);
        let text = screen(&app, 60, 12);
        assert!(!text.contains("job-00"), "{text}");
        assert!(text.contains("job-11"), "{text}");
        // The end of the list is the rule that closes it.
        assert!(text.contains(" ├──"), "{text}");
        assert!(text.contains(" 6–12 of 12"), "{text}");
        // What was applied is as long, and says where it is too.
        let done = app.changes().to_vec();
        app.show_applied(done);
        let text = screen(&app, 60, 12);
        assert!(text.contains(" 12 applied · 1–8 of 12"), "{text}");
    }

    #[test]
    fn applying_is_a_question_named_after_the_action() {
        let mut app = unsynced(every_change());
        app.act(Action::Sync);
        app.act(Action::Apply);
        let text = screen(&app, 60, 12);
        let bar = text.lines().last().unwrap();
        assert_eq!(bar.trim_end(), " apply 2 changes?  y apply  n not now");
        let mut app = unsynced(vec![pend("report", Ok(sync::Action::Add))]);
        app.act(Action::Sync);
        app.act(Action::Apply);
        let text = screen(&app, 60, 12);
        assert!(text.contains(" apply 1 change?  y apply"), "{text}");
    }

    #[test]
    fn what_was_applied_is_said_in_the_words_of_otto_sync() {
        let mut app = unsynced(every_change());
        app.act(Action::Sync);
        app.columns = columns(78);
        app.show_applied(vec![
            pend("report", Ok(sync::Action::Add)),
            pend("triage", Ok(sync::Action::Update)),
            pend("nightly", Err("the scheduler refused nightly")),
            pend("old", Ok(sync::Action::Remove)),
        ]);
        let text = screen(&app, 78, 20);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[2].trim_end().ends_with(" added"), "{text}");
        assert!(rows[3].trim_end().ends_with(" updated"), "{text}");
        assert!(rows[4].trim_end().ends_with(" error"), "{text}");
        assert_eq!(rows[5].trim_end(), " │ the scheduler refused nightly");
        assert!(rows[6].trim_end().ends_with(" removed"), "{text}");
        assert!(rows[18].starts_with(" 3 applied, 1 not"), "{text}");
        // There is nothing left to ask on this screen.
        assert_eq!(rows[19].trim_end(), " esc back  ? help");
    }

    #[test]
    fn what_a_busy_job_waits_for_is_said_after_applying_too() {
        let mut app = unsynced(every_change());
        app.act(Action::Sync);
        app.columns = columns(60);
        app.page = page(12);
        app.show_applied(vec![
            pend("report", Ok(sync::Action::Add)),
            pend("nightly", Ok(sync::Action::Busy)),
        ]);
        let text = screen(&app, 60, 12);
        assert!(
            text.contains(" 1 applied, 1 not · busy: sync again after its run ends"),
            "{text}"
        );
        app.show_applied(vec![pend("nightly", Ok(sync::Action::Busy))]);
        let text = screen(&app, 60, 12);
        assert!(text.contains(" 0 applied, 1 not · busy:"), "{text}");
    }

    #[test]
    fn the_help_still_fits_with_the_sync_in_it() {
        let mut app = unsynced(Vec::new());
        app.act(Action::Help);
        let text = screen(&app, 60, 12);
        assert!(text.contains(" S     what a sync would do"), "{text}");
        for row in text.lines() {
            assert!(width_in_columns(row.trim_end()) < 60, "{row}");
        }
    }

    #[test]
    fn the_level_in_focus_says_what_it_does() {
        let said = [
            (Level::Off, " never tells"),
            (Level::Failures, " tells when a run fails"),
            (Level::Finish, " tells when a run ends, ok or failed"),
            (
                Level::All,
                " tells when a run starts, ends or does not happen",
            ),
        ];
        for (level, words) in said {
            let mut app = creating("nightly");
            let form = app.form.as_mut().unwrap();
            form.focus = Focus::Notify;
            form.notify = level;
            let (text, cursor) = screen_and_cursor(&app, MIN.0, MIN.1);
            let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
            assert!(lines[9].starts_with("▸│ notify   "), "{text}");
            assert_eq!(lines[10], words, "{text}");
            assert_eq!(
                lines[11],
                " tab next  ←→ move  space mark  ctrl-s save  esc cancel"
            );
            // The cursor is on the level that is chosen.
            let chosen = format!("✓{}", level.label());
            let column = width_in_columns(&lines[9][..lines[9].find(&chosen).unwrap()]);
            assert_eq!(cursor, (column as u16, 9), "{text}");
        }
    }

    #[test]
    fn the_form_fits_the_smallest_terminal_on_every_field() {
        let mut app = creating("nightly");
        for _ in 0..8 {
            let (text, cursor) = screen_and_cursor(&app, MIN.0, MIN.1);
            for row in text.lines() {
                assert!(width_in_columns(row.trim_end()) < 60, "{row}");
            }
            let row = text.lines().nth(usize::from(cursor.1)).unwrap();
            assert!(row.starts_with('▸'), "{text}");
            app.act(Action::NextField);
        }
    }

    #[test]
    fn the_job_screen_says_how_the_job_tells() {
        let louder = JobView {
            job: Job {
                notify: Level::All,
                ..job("report").job
            },
            ..job("report")
        };
        let mut app = app(vec![louder]);
        app.act(Action::Open);
        let text = screen(&app, 78, 20);
        assert!(text.contains(" │ notify   all"), "{text}");
    }

    #[test]
    fn how_a_job_tells_is_the_first_fact_to_give_way() {
        let app = on_the_job();
        // A row short of the whole sheet: what the screen showed before the
        // job had this fact.
        let short = screen(&app, 60, 13);
        assert!(short.contains(" │ args"), "{short}");
        assert!(!short.contains(" │ notify"), "{short}");
        assert!(short.contains("▸ ✓"), "{short}");
        let enough = screen(&app, 60, 14);
        assert!(enough.contains(" │ args"), "{enough}");
        assert!(enough.contains(" │ notify"), "{enough}");
    }

    #[test]
    fn what_a_level_does_is_said_whatever_else_there_is_to_say() {
        let mut app = creating("nightly");
        app.warnings = vec!["codex not found in PATH".to_owned()];
        let text = screen(&app, 78, 20);
        assert!(text.contains(" note: codex not found in PATH"), "{text}");
        app.form.as_mut().unwrap().focus = Focus::Notify;
        let text = screen(&app, 78, 20);
        assert!(text.contains(" tells when a run fails"), "{text}");
        assert!(!text.contains("note:"), "{text}");
    }
}
