//! Writing the jobs file. It stays the user's file, so nothing here writes the
//! file back from a parsed form: a change is made on the text itself, at the
//! bytes of the value that changed, and every other byte is left as it was.
//! Comments, order, spacing, line endings and a byte order mark are never
//! touched because they are never rewritten. Nothing else writes `jobs.toml`.

use std::fs;
use std::io::ErrorKind;
use std::ops::Range;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use toml_edit::{Document, InlineTable, Item, Key, Table, TableLike, Value};

use crate::agent::Agent;
use crate::atomic;
use crate::config::{self, Config, Weekday};
use crate::notify::Level;

/// A job as the user writes it: paths as typed, not resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    pub agent: Agent,
    pub prompt: String,
    pub workdir: String,
    /// Local time of day, `HH:MM`.
    pub at: String,
    /// None and all seven both mean every day.
    pub days: Vec<Weekday>,
    pub args: Vec<String>,
    /// Which runs the job tells of. The one a job has without saying so is
    /// not written.
    pub notify: Level,
}

impl JobSpec {
    /// The days in the order of the week, with none read as every day, so
    /// that two ways of saying the same schedule compare equal.
    fn week(&self) -> Vec<Weekday> {
        let week = Weekday::every_day();
        if self.days.is_empty() {
            return week;
        }
        week.into_iter()
            .filter(|day| self.days.contains(day))
            .collect()
    }

    fn every_day(&self) -> bool {
        self.week() == Weekday::every_day()
    }
}

/// What `save` says when the file is no longer the one that was read.
pub const CHANGED: &str = "the jobs file changed on disk since it was read";

/// A place in the text of the jobs file.
type Span = Range<usize>;

/// The byte order mark some editors put at the start of a file. The parser's
/// positions are worked out without it, so it is set aside and put back.
fn without_mark(text: &str) -> (&str, &str) {
    match text.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", text),
    }
}

fn parse(text: &str) -> Result<Document<&str>> {
    Document::parse(text).context("the jobs file is not valid TOML as it is")
}

/// The rules the jobs file is read with, applied to the file before it is
/// edited: a file that is already invalid is not made the edit's fault.
fn valid(text: &str) -> Result<()> {
    Config::parse(text, Path::new("."), None)
        .map(drop)
        .context("the jobs file is not valid as it is")
}

/// What reading the file takes and otto still does not write: an empty path
/// is read as the directory of the jobs file, which nobody means.
fn written_out(spec: &JobSpec) -> Result<()> {
    if spec.workdir.trim().is_empty() {
        bail!("workdir is empty");
    }
    if spec.prompt.trim().is_empty() {
        bail!("prompt is empty");
    }
    Ok(())
}

/// The same rules, applied to what would be written. Paths are not looked at.
fn checked(text: String) -> Result<String> {
    Config::parse(&text, Path::new("."), None).context("the jobs file would not be valid")?;
    Ok(text)
}

/// What an edit of `original` gives. An edit may repair a file that was
/// invalid, by removing the job that broke it; one that leaves it invalid
/// is refused, and the blame goes where it belongs.
fn settled(original: &str, edited: String) -> Result<String> {
    match checked(edited) {
        Ok(text) => Ok(text),
        Err(error) => {
            valid(original)?;
            Err(error)
        }
    }
}

const BY_HAND: &str = "this job is written with dotted keys inside an inline table, \
     where otto cannot add or remove a key: change it by hand in the jobs file";

fn no_job(name: &str) -> String {
    format!("no job named {name:?}")
}

/// Where the line holding position `at` starts.
fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |found| found + 1)
}

/// Just past the line holding position `at`, its line ending included.
fn line_end(text: &str, at: usize) -> usize {
    text[at..]
        .find('\n')
        .map_or(text.len(), |found| at + found + 1)
}

/// The line ending in use around position `at`: the one of that line, or of
/// the file when the line has none.
fn ending(text: &str, at: usize) -> &'static str {
    let line = &text[..line_end(text, at.min(text.len()))];
    if line.ends_with("\r\n") || (!line.ends_with('\n') && text.contains("\r\n")) {
        "\r\n"
    } else {
        "\n"
    }
}

/// A string as TOML. One with a line break in it is written with the break
/// escaped: written raw it would be at the mercy of the file's line endings.
fn literal(text: &str) -> String {
    if !text.contains(['\n', '\r']) {
        return Value::from(text).to_string();
    }
    let mut out = String::from('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<String> = items.into_iter().map(literal).collect();
    format!("[{}]", items.join(", "))
}

/// A table of the jobs file, in either of the two ways TOML writes one.
#[derive(Clone, Copy)]
enum Container<'a> {
    /// Under a `[header]`, or spelled out with dotted keys: a key per line.
    Table(&'a Table),
    /// `{ … }` on the right of a key.
    Inline(&'a InlineTable),
}

impl<'a> Container<'a> {
    fn of(item: &'a Item) -> Option<Container<'a>> {
        match item {
            Item::Table(table) => Some(Container::Table(table)),
            Item::Value(Value::InlineTable(table)) => Some(Container::Inline(table)),
            _ => None,
        }
    }

    fn entry(self, key: &str) -> Option<(&'a Key, &'a Item)> {
        match self {
            Container::Table(table) => table.get_key_value(key),
            Container::Inline(table) => table.get_key_value(key),
        }
    }

    /// Whether a key can be added to or taken from this table as one piece
    /// of text. Inside `{ … }`, dotted keys spell out tables whose entries
    /// can sit anywhere among the others.
    fn in_one_piece(self) -> bool {
        match self {
            Container::Table(_) => true,
            Container::Inline(table) => {
                !table.is_dotted()
                    && table.iter().all(|(_, value)| {
                        !value.as_inline_table().is_some_and(InlineTable::is_dotted)
                    })
            }
        }
    }

    fn entries(self) -> Vec<(&'a Key, &'a Item)> {
        let names: Vec<&str> = match self {
            Container::Table(table) => table.iter().map(|(name, _)| name).collect(),
            Container::Inline(table) => table.iter().map(|(name, _)| name).collect(),
        };
        names
            .into_iter()
            .filter_map(|name| self.entry(name))
            .collect()
    }
}

fn span_of(item: &Item) -> Result<Span> {
    item.span()
        .context("the jobs file has a value otto cannot place")
}

fn key_start(key: &Key) -> Result<usize> {
    let span = key
        .span()
        .context("the jobs file has a key otto cannot place")?;
    Ok(span.start)
}

/// The end of the last value written under `table`'s own header: its keys and
/// what dotted keys spell out under it, not a table with a header of its own.
fn section_end(table: &Table) -> Option<usize> {
    table
        .iter()
        .filter_map(|(_, item)| match item {
            Item::Value(value) => value.span().map(|span| span.end),
            Item::Table(inner) if inner.is_dotted() => section_end(inner),
            _ => None,
        })
        .max()
}

/// One change to the text: these bytes become this.
type Edit = (Span, String);

/// Adds `key = value` to a table that does not have it.
fn insert(text: &str, container: Container, key: &str, value: &str) -> Result<Edit> {
    if !container.in_one_piece() {
        bail!(BY_HAND);
    }
    match container {
        // After the last entry, on the same line as it.
        Container::Inline(_) => {
            let mut last = None;
            for (_, item) in container.entries() {
                last = last.max(Some(span_of(item)?.end));
            }
            let at = last.context("an inline table with nothing in it")?;
            Ok((at..at, format!(", {key} = {value}")))
        }
        // On a line of its own after the last key, spelled the way the keys
        // around it are: indented, or with the same dotted path before it.
        Container::Table(table) => {
            let mut before = "";
            for (existing, item) in container.entries() {
                if item.is_value() {
                    let start = key_start(existing)?;
                    before = &text[line_start(text, start)..start];
                    break;
                }
            }
            let last = section_end(table)
                .or(table.span().map(|header| header.end))
                .context("a table otto cannot place")?;
            let at = line_end(text, last);
            let end = ending(text, last);
            let line = format!("{before}{key} = {value}");
            Ok(if at == text.len() && !text.ends_with('\n') {
                (at..at, format!("{end}{line}"))
            } else {
                (at..at, format!("{line}{end}"))
            })
        }
    }
}

/// Takes `key` and its value out of a table.
fn delete(text: &str, container: Container, key: &str) -> Result<Edit> {
    let (found, item) = container
        .entry(key)
        .with_context(|| format!("no key named {key}"))?;
    if !container.in_one_piece() {
        bail!(BY_HAND);
    }
    let (start, end) = (key_start(found)?, span_of(item)?.end);
    let lines = line_start(text, start)..line_end(text, end);
    // Alone on its lines: nothing before the key, and after the value only a
    // comma and a comment.
    let alone = text[lines.start..start].trim().is_empty() && {
        let rest = text[end..lines.end].trim();
        let rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
        rest.is_empty() || rest.starts_with('#')
    };
    let span = match container {
        // The lines of the key and its value. A comment above them stays.
        Container::Table(_) => lines,
        // An inline table over several lines, one entry to a line: the same.
        Container::Inline(_) if alone => lines,
        // The entry and the comma that goes with it.
        Container::Inline(_) => {
            let mut before = None;
            let mut after = None;
            for (other, value) in container.entries() {
                let at = key_start(other)?;
                if at < start {
                    before = before.max(Some(span_of(value)?.end));
                } else if at > start {
                    after = Some(after.map_or(at, |first: usize| first.min(at)));
                }
            }
            match (before, after) {
                (Some(before), _) => before..end,
                (None, Some(after)) => start..after,
                (None, None) => start..end,
            }
        }
    };
    Ok((span, String::new()))
}

/// The text with every edit made, from the last place to the first, so no
/// edit moves the place of one still to make. Where a key is taken out and
/// another put in at the same place, the one taken out goes first: the other
/// way round, the new text would be what is removed.
fn apply(text: &str, mut edits: Vec<Edit>) -> Result<String> {
    edits.sort_by_key(|(span, _)| std::cmp::Reverse((span.start, span.len())));
    let mut out = text.to_owned();
    let mut floor = text.len();
    for (span, new) in edits {
        // Each edit is one value, or one key with its value: two of them
        // reaching into each other is a file otto did not foresee.
        if span.end > floor {
            bail!("otto cannot make these changes together: change the jobs file by hand");
        }
        floor = span.start;
        out.replace_range(span, &new);
    }
    Ok(out)
}

/// The job `name` as it is written in `text`. A schedule with no `days` comes
/// back with all seven.
pub fn read(text: &str, name: &str) -> Result<JobSpec> {
    let (_, text) = without_mark(text);
    let doc = parse(text)?;
    let job = doc
        .get("jobs")
        .and_then(Item::as_table_like)
        .and_then(|jobs| jobs.get(name))
        .and_then(Item::as_table_like)
        .with_context(|| no_job(name))?;
    let string = |table: &dyn TableLike, key: &str| {
        table
            .get(key)
            .and_then(Item::as_str)
            .map(str::to_owned)
            .with_context(|| format!("job {name}: {key} must be a string"))
    };
    let strings = |table: &dyn TableLike, key: &str| -> Result<Option<Vec<String>>> {
        let Some(item) = table.get(key) else {
            return Ok(None);
        };
        item.as_array()
            .into_iter()
            .flatten()
            .map(|value| value.as_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>()
            .filter(|_| item.is_array())
            .map(Some)
            .with_context(|| format!("job {name}: {key} must be a list of strings"))
    };

    let agent = string(job, "agent")?;
    let agent = Agent::ALL
        .into_iter()
        .find(|known| known.program() == agent)
        .with_context(|| format!("job {name}: no agent named {agent:?}"))?;
    let schedule = job
        .get("schedule")
        .and_then(Item::as_table_like)
        .with_context(|| format!("job {name}: schedule must be a table"))?;
    let days = match strings(schedule, "days")? {
        None => Weekday::every_day(),
        Some(names) => names
            .iter()
            .map(|name| {
                Weekday::every_day()
                    .into_iter()
                    .find(|day| day.toml_name() == name)
            })
            .collect::<Option<Vec<Weekday>>>()
            .with_context(|| format!("job {name}: schedule.days has a day otto does not know"))?,
    };
    let notify = match job.get("notify") {
        None => Level::default(),
        Some(_) => {
            let level = string(job, "notify")?;
            Level::ALL
                .into_iter()
                .find(|known| known.label() == level)
                .with_context(|| format!("job {name}: no notify level named {level:?}"))?
        }
    };
    Ok(JobSpec {
        agent,
        prompt: string(job, "prompt")?,
        workdir: string(job, "workdir")?,
        at: string(schedule, "at")?,
        days,
        args: strings(job, "args")?.unwrap_or_default(),
        notify,
    })
}

/// `text` with a new job at its end. The block always has the same shape:
/// agent, prompt, workdir, schedule and, when there are any, args, and then
/// how it tells of its runs when that is not what a job does anyway.
pub fn add(text: &str, name: &str, spec: &JobSpec) -> Result<String> {
    // Checked here: a name that is not a bare key would not even parse below.
    if !config::is_job_name(name) {
        bail!("job name {name:?} must be lowercase letters, digits and dashes");
    }
    written_out(spec)?;
    let (_, body) = without_mark(text);
    valid(body)?;
    let taken = parse(body)?
        .get("jobs")
        .and_then(Item::as_table_like)
        .is_some_and(|jobs| jobs.contains_key(name));
    if taken {
        bail!("job {name:?} already exists");
    }
    let end = ending(text, text.len());
    let mut schedule = format!("{{ at = {}", literal(&spec.at));
    if !spec.every_day() {
        let days = list(spec.week().into_iter().map(Weekday::toml_name));
        schedule.push_str(&format!(", days = {days}"));
    }
    schedule.push_str(" }");
    let mut lines = vec![
        format!("[jobs.{name}]"),
        format!("agent = {}", literal(spec.agent.program())),
        format!("prompt = {}", literal(&spec.prompt)),
        format!("workdir = {}", literal(&spec.workdir)),
        format!("schedule = {schedule}"),
    ];
    if !spec.args.is_empty() {
        lines.push(format!(
            "args = {}",
            list(spec.args.iter().map(String::as_str))
        ));
    }
    if spec.notify != Level::default() {
        lines.push(format!("notify = {}", literal(spec.notify.label())));
    }

    // One blank line between what is there and the new job, and no more.
    let mut out = text.to_owned();
    if !body.trim().is_empty() {
        if !out.ends_with('\n') {
            out.push_str(end);
        }
        // The last line, which may hold nothing but spaces.
        let last = out.trim_end_matches(['\r', '\n']).lines().next_back();
        let blank = out.ends_with("\n\n")
            || out.ends_with("\n\r\n")
            || last.is_some_and(|line| line.trim().is_empty());
        if !blank {
            out.push_str(end);
        }
    }
    for line in lines {
        out.push_str(&line);
        out.push_str(end);
    }
    checked(out)
}

/// `text` with the job `name` changed to `spec`. Only the values that differ
/// from what is written are replaced, where they are, so saving a job as it
/// is gives the same text back.
pub fn update(text: &str, name: &str, spec: &JobSpec) -> Result<String> {
    let (mark, text) = without_mark(text);
    let old = read(text, name)?;
    written_out(spec)?;
    let doc = parse(text)?;
    let job = doc
        .get("jobs")
        .and_then(Container::of)
        .and_then(|jobs| jobs.entry(name))
        .and_then(|(_, job)| Container::of(job))
        .with_context(|| no_job(name))?;
    let value = |container: Container, key: &str| -> Result<Span> {
        let (_, item) = container
            .entry(key)
            .with_context(|| format!("job {name}: no {key}"))?;
        span_of(item)
    };

    let mut edits = Vec::new();
    if old.agent != spec.agent {
        edits.push((value(job, "agent")?, literal(spec.agent.program())));
    }
    if old.prompt != spec.prompt {
        edits.push((value(job, "prompt")?, literal(&spec.prompt)));
    }
    if old.workdir != spec.workdir {
        edits.push((value(job, "workdir")?, literal(&spec.workdir)));
    }
    let schedule = job
        .entry("schedule")
        .and_then(|(_, schedule)| Container::of(schedule))
        .with_context(|| format!("job {name}: schedule must be a table"))?;
    if old.at != spec.at {
        edits.push((value(schedule, "at")?, literal(&spec.at)));
    }
    // `days = []` is read as every day and is not valid: it counts as a
    // change even when every day is what is asked for.
    if old.week() != spec.week() || (old.days.is_empty() && schedule.entry("days").is_some()) {
        let days = list(spec.week().into_iter().map(Weekday::toml_name));
        let written = schedule.entry("days").is_some();
        edits.push(match (written, spec.every_day()) {
            (true, true) => delete(text, schedule, "days")?,
            (true, false) => (value(schedule, "days")?, days),
            (false, _) => insert(text, schedule, "days", &days)?,
        });
    }
    // Before the arguments: of two keys added at the end of the job, the one
    // asked for last is the one written first.
    if old.notify != spec.notify {
        let level = literal(spec.notify.label());
        let written = job.entry("notify").is_some();
        edits.push(match (written, spec.notify == Level::default()) {
            (true, true) => delete(text, job, "notify")?,
            (true, false) => (value(job, "notify")?, level),
            (false, _) => insert(text, job, "notify", &level)?,
        });
    }
    if old.args != spec.args {
        let args = list(spec.args.iter().map(String::as_str));
        let written = job.entry("args").is_some();
        edits.push(match (written, spec.args.is_empty()) {
            (true, true) => delete(text, job, "args")?,
            (true, false) => (value(job, "args")?, args),
            (false, _) => insert(text, job, "args", &args)?,
        });
    }
    if edits.is_empty() {
        return Ok(format!("{mark}{text}"));
    }
    Ok(format!("{mark}{}", settled(text, apply(text, edits)?)?))
}

/// The lines a job takes in the file: under its own header, the header and
/// everything down to its last value; spelled with dotted keys or as an
/// inline table, the lines of each key.
fn lines_of(text: &str, key: &Key, item: &Item, out: &mut Vec<Span>) -> Result<()> {
    match item {
        Item::Value(value) => {
            let end = value.span().context("a value otto cannot place")?.end;
            out.push(line_start(text, key_start(key)?)..line_end(text, end));
        }
        Item::Table(table) => {
            // A header of its own: `[jobs.a]`, and `[jobs.a.schedule]` too.
            if !table.is_dotted() && !table.is_implicit() {
                let header = table.span().context("a table otto cannot place")?;
                let end = section_end(table).map_or(header.end, |end| end.max(header.end));
                out.push(line_start(text, header.start)..line_end(text, end));
            }
            for (name, inner) in table.iter() {
                let dotted = !matches!(inner, Item::Table(inner) if !inner.is_dotted());
                // What is under this header is already in its lines.
                if dotted && !table.is_dotted() && !table.is_implicit() {
                    continue;
                }
                if let Some((key, inner)) = table.get_key_value(name) {
                    lines_of(text, key, inner, out)?;
                }
            }
        }
        Item::ArrayOfTables(tables) => {
            for table in tables.iter() {
                let header = table.span().context("a table otto cannot place")?;
                let end = section_end(table).map_or(header.end, |end| end.max(header.end));
                out.push(line_start(text, header.start)..line_end(text, end));
            }
        }
        Item::None => {}
    }
    Ok(())
}

fn is_blank(text: &str, line: Span) -> bool {
    text[line].trim().is_empty()
}

/// `text` without the job `name`. A comment right above the job goes with it;
/// one a blank line away, or one the file opens with, belongs to the file.
pub fn remove(text: &str, name: &str) -> Result<String> {
    let (mark, text) = without_mark(text);
    let doc = parse(text)?;
    let jobs = doc
        .get("jobs")
        .and_then(Container::of)
        .with_context(|| no_job(name))?;
    let (key, job) = jobs.entry(name).with_context(|| no_job(name))?;

    // `jobs = { a = { … } }`: the job is a piece of one line.
    if let Container::Inline(_) = jobs {
        let edit = delete(text, jobs, name)?;
        return Ok(format!(
            "{mark}{}",
            settled(text, apply(text, vec![edit])?)?
        ));
    }

    // Where every value and header of the file ends. A line that starts with
    // `#` above one of these is inside a value, and is no comment.
    let mut ends = Vec::new();
    // From the top-level entries down: the document itself has no place.
    doc.iter().for_each(|(_, item)| ends_of(item, &mut ends));

    let mut spans = Vec::new();
    lines_of(text, key, job, &mut spans)?;
    for span in &mut spans {
        // The comment right above, unless it is what the file opens with.
        let written = ends
            .iter()
            .copied()
            .filter(|end| *end <= span.start)
            .max()
            .map_or(0, |end| line_end(text, end));
        let mut start = span.start;
        while start > written {
            let above = line_start(text, start - 1);
            if !text[above..start].trim_start().starts_with('#') {
                break;
            }
            start = above;
        }
        if start > 0 {
            span.start = start;
        }
    }
    spans.sort_by_key(|span| span.start);
    let mut merged: Vec<Span> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    for span in &mut merged {
        // The blank line that set the job apart goes with it: the one after
        // it, or at the end of the file the one before.
        let above_is_blank =
            span.start == 0 || is_blank(text, line_start(text, span.start - 1)..span.start);
        while above_is_blank
            && span.end < text.len()
            && is_blank(text, span.end..line_end(text, span.end))
        {
            span.end = line_end(text, span.end);
        }
        while span.end == text.len()
            && span.start > 0
            && is_blank(text, line_start(text, span.start - 1)..span.start)
        {
            span.start = line_start(text, span.start - 1);
        }
    }
    // Two pieces of the job a blank line apart have both reached for it.
    let mut pieces: Vec<Span> = Vec::new();
    for span in merged {
        match pieces.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => pieces.push(span),
        }
    }
    let edits = pieces
        .into_iter()
        .map(|span| (span, String::new()))
        .collect();
    Ok(format!("{mark}{}", settled(text, apply(text, edits)?)?))
}

/// Where each value and each table header under `item` ends in the text.
fn ends_of(item: &Item, out: &mut Vec<usize>) {
    match item {
        Item::Value(value) => out.extend(value.span().map(|span| span.end)),
        Item::Table(table) => {
            if !table.is_dotted() && !table.is_implicit() {
                out.extend(table.span().map(|span| span.end));
            }
            table.iter().for_each(|(_, inner)| ends_of(inner, out));
        }
        Item::ArrayOfTables(tables) => {
            for table in tables.iter() {
                out.extend(table.span().map(|span| span.end));
                table.iter().for_each(|(_, inner)| ends_of(inner, out));
            }
        }
        Item::None => {}
    }
}

/// Writes `text` to the jobs file at `path`. `read` is the text the change was
/// made from, empty when the file did not exist: if the file is no longer
/// that, someone edited it meanwhile and nothing is written.
pub fn save(path: &Path, read: &str, text: &str) -> Result<()> {
    let on_disk = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", path.display()));
        }
    };
    if on_disk != read {
        bail!(CHANGED);
    }
    atomic::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../examples/jobs.toml");

    const WEEKDAYS: [Weekday; 5] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
    ];

    fn nightly() -> JobSpec {
        JobSpec {
            agent: Agent::Codex,
            prompt: "prompts/nightly.md".to_owned(),
            workdir: "~/Workspace/app".to_owned(),
            at: "02:00".to_owned(),
            days: vec![Weekday::Mon, Weekday::Wed],
            args: vec!["--model".to_owned(), "x".to_owned()],
            notify: Level::default(),
        }
    }

    fn jobs(text: &str) -> usize {
        Config::parse(text, Path::new("/etc/otto"), None)
            .unwrap()
            .jobs
            .len()
    }

    /// Days added on the line where the arguments are taken out: two edits
    /// that start at the same byte.
    #[test]
    fn an_insert_and_a_delete_at_the_same_place_do_not_collide() {
        let dotted = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule.at = \"07:00\"\nARGS\n";
        let all_dotted = "jobs.a.agent = \"claude\"\njobs.a.prompt = \"a.md\"\njobs.a.workdir = \".\"\njobs.a.schedule.at = \"07:00\"\njobs.a.ARGS\n";
        let lines = [
            "args = [\"-x\"]",
            "args = [\"xxxxxxxxxxxxxxxxxxx\"]",
            "args = [\"-x\"] # nnnnnnnnnnnnnnnnnnnnnnnnnnnn",
        ];
        for style in [dotted, all_dotted] {
            for line in lines {
                let text = style.replace("ARGS", line);
                let wanted = JobSpec {
                    days: vec![Weekday::Tue, Weekday::Thu],
                    args: Vec::new(),
                    ..read(&text, "a").unwrap()
                };
                let changed = update(&text, "a", &wanted).unwrap();
                assert_eq!(read(&changed, "a").unwrap(), wanted, "{changed}");
            }
        }
    }

    /// Every pair of changes on every style: none may undo or mangle another.
    #[test]
    fn any_two_changes_at_once_both_take() {
        let bare = [
            "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = { at = \"07:00\" }\n",
            "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\n\n[jobs.a.schedule]\nat = \"07:00\"\n",
            "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule.at = \"07:00\"\n",
            "jobs.a.agent = \"claude\"\njobs.a.prompt = \"a.md\"\njobs.a.workdir = \".\"\njobs.a.schedule.at = \"07:00\"\n",
            "[jobs]\na = { agent = \"claude\", prompt = \"a.md\", workdir = \".\", schedule = { at = \"07:00\" } }\n",
        ];
        for style in STYLES.into_iter().chain(bare) {
            let written = read(style, "a").unwrap();
            let days = [Vec::new(), vec![Weekday::Tue, Weekday::Thu]];
            let args = [Vec::new(), vec!["--one".to_owned(), "two".to_owned()]];
            for days in &days {
                for args in &args {
                    for at in ["07:00", "21:30"] {
                        let wanted = JobSpec {
                            days: days.clone(),
                            args: args.clone(),
                            at: at.to_owned(),
                            ..written.clone()
                        };
                        let text = update(style, "a", &wanted).unwrap_or_else(|error| {
                            panic!("{error:#}\nstyle:\n{style}\nwanted: {wanted:?}")
                        });
                        let got = read(&text, "a").unwrap();
                        assert_eq!(got.week(), wanted.week(), "{text}");
                        assert_eq!((got.args, got.at), (wanted.args, wanted.at), "{text}");
                    }
                }
            }
        }
    }

    #[test]
    fn dotted_keys_inside_an_inline_table_are_changed_but_never_added_or_removed() {
        let text = "[jobs]\na = { agent = \"claude\", prompt = \"a.md\", workdir = \".\", schedule.at = \"07:00\", args = [\"-x\"], schedule.days = [\"mon\"] }\n";
        let written = read(text, "a").unwrap();
        // A value is changed where it is.
        let later = JobSpec {
            at: "08:00".to_owned(),
            days: vec![Weekday::Fri],
            ..written.clone()
        };
        assert_eq!(
            update(text, "a", &later).unwrap(),
            text.replace("07:00", "08:00").replace("\"mon\"", "\"fri\"")
        );
        // Taking a key out is refused, with the reason, and nothing is lost.
        for wanted in [
            JobSpec {
                days: Vec::new(),
                ..written.clone()
            },
            JobSpec {
                args: Vec::new(),
                ..written.clone()
            },
            JobSpec {
                days: Vec::new(),
                args: Vec::new(),
                ..written.clone()
            },
        ] {
            let error = update(text, "a", &wanted).unwrap_err();
            assert!(format!("{error:#}").contains("by hand"), "{error:#}");
        }
        let no_days = text.replace(", schedule.days = [\"mon\"]", "");
        let wanted = JobSpec {
            days: vec![Weekday::Mon],
            ..read(&no_days, "a").unwrap()
        };
        let error = update(&no_days, "a", &wanted).unwrap_err();
        assert!(format!("{error:#}").contains("by hand"), "{error:#}");
        // The job as a whole can still go.
        assert_eq!(jobs(&remove(text, "a").unwrap()), 0);
    }

    #[test]
    fn a_key_on_its_own_line_of_an_inline_table_goes_with_its_line_only() {
        let text = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = {\n  # k0\n  at = \"07:00\", # k1\n  # k2\n  days = [\"mon\"], # k3\n  # k4\n} # k5\n";
        let wanted = JobSpec {
            days: Vec::new(),
            ..read(text, "a").unwrap()
        };
        assert_eq!(
            update(text, "a", &wanted).unwrap(),
            text.replace("  days = [\"mon\"], # k3\n", "")
        );
    }

    #[test]
    fn a_line_that_starts_with_a_hash_inside_a_string_is_not_a_comment() {
        let text = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = { at = \"07:00\" }\nargs = [\"-p\", \"\"\"\nDo this.\n# Heading\"\"\"]\n[jobs.b]\nagent = \"codex\"\nprompt = \"b.md\"\nworkdir = \".\"\nschedule = { at = \"08:00\" }\n";
        let removed = remove(text, "b").unwrap();
        assert_eq!(removed, &text[..text.find("[jobs.b]").unwrap()]);
        assert_eq!(read(&removed, "a").unwrap(), read(text, "a").unwrap());
    }

    #[test]
    fn a_broken_job_can_be_removed_and_empty_days_repaired() {
        let good = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = { at = \"07:00\" }\n";
        let broken = format!(
            "{good}\n[jobs.b]\nagent = \"gemini\"\nprompt = \"b.md\"\nworkdir = \".\"\nschedule = {{ at = \"08:00\" }}\n"
        );
        assert_eq!(remove(&broken, "b").unwrap(), good);
        // Removing the good one leaves the file as broken as it was.
        let error = remove(&broken, "a").unwrap_err();
        assert!(
            format!("{error:#}").contains("not valid as it is"),
            "{error:#}"
        );

        let empty = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = { at = \"07:00\", days = [] }\n";
        let every_day = JobSpec {
            days: Weekday::every_day(),
            ..read(empty, "a").unwrap()
        };
        assert_eq!(update(empty, "a", &every_day).unwrap(), good);
        let mondays = JobSpec {
            days: vec![Weekday::Mon],
            ..every_day
        };
        assert_eq!(
            update(empty, "a", &mondays).unwrap(),
            empty.replace("days = []", "days = [\"mon\"]")
        );
    }

    #[test]
    fn a_last_line_of_only_spaces_counts_as_blank() {
        let spaced = format!("{SAMPLE}   \n");
        let added = add(&spaced, "nightly", &nightly()).unwrap();
        assert!(
            added[spaced.len()..].starts_with("[jobs.nightly]\n"),
            "{added}"
        );
    }

    /// A job written every way the form may find one.
    const STYLES: [&str; 5] = [
        // A header and an inline schedule.
        "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = { at = \"07:00\", days = [\"mon\"] }\nargs = [\"-x\"]\n",
        // The schedule under a header of its own.
        "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nargs = [\"-x\"]\n\n[jobs.a.schedule]\nat = \"07:00\"\ndays = [\"mon\"]\n",
        // Dotted keys inside the job.
        "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule.at = \"07:00\"\nschedule.days = [\"mon\"]\nargs = [\"-x\"]\n",
        // Dotted keys all the way.
        "jobs.a.agent = \"claude\"\njobs.a.prompt = \"a.md\"\njobs.a.workdir = \".\"\njobs.a.schedule.at = \"07:00\"\njobs.a.schedule.days = [\"mon\"]\njobs.a.args = [\"-x\"]\n",
        // The whole job on one line.
        "[jobs]\na = { agent = \"claude\", prompt = \"a.md\", workdir = \".\", schedule = { at = \"07:00\", days = [\"mon\"] }, args = [\"-x\"] }\n",
    ];

    #[test]
    fn every_style_takes_every_change_and_reads_back() {
        for style in STYLES {
            let written = read(style, "a").unwrap();
            assert_eq!(update(style, "a", &written).unwrap(), style);

            let changes = [
                JobSpec {
                    at: "08:00".to_owned(),
                    ..written.clone()
                },
                JobSpec {
                    agent: Agent::Codex,
                    workdir: "~/w".to_owned(),
                    ..written.clone()
                },
                JobSpec {
                    days: vec![Weekday::Tue, Weekday::Sat],
                    ..written.clone()
                },
                JobSpec {
                    days: Vec::new(),
                    ..written.clone()
                },
                JobSpec {
                    args: Vec::new(),
                    ..written.clone()
                },
                JobSpec {
                    args: vec!["a b".to_owned(), "c".to_owned()],
                    ..written.clone()
                },
                JobSpec {
                    days: Vec::new(),
                    args: Vec::new(),
                    at: "23:59".to_owned(),
                    ..written.clone()
                },
                JobSpec {
                    notify: Level::All,
                    ..written.clone()
                },
                JobSpec {
                    notify: Level::Off,
                    args: Vec::new(),
                    ..written.clone()
                },
                JobSpec {
                    notify: Level::Finish,
                    args: vec!["x".to_owned()],
                    days: vec![Weekday::Sun],
                    ..written.clone()
                },
            ];
            for wanted in changes {
                let text = update(style, "a", &wanted).unwrap_or_else(|error| {
                    panic!("{error:#}\nstyle:\n{style}\nwanted: {wanted:?}")
                });
                let read_back = read(&text, "a").unwrap();
                assert_eq!(read_back.week(), wanted.week(), "{text}");
                assert_eq!(
                    JobSpec {
                        days: wanted.days.clone(),
                        ..read_back
                    },
                    wanted,
                    "{text}"
                );
                // And back again: what was taken out can be put in.
                let restored = update(&text, "a", &written).unwrap();
                assert_eq!(read(&restored, "a").unwrap(), written, "{restored}");
            }
            assert_eq!(jobs(&remove(style, "a").unwrap()), 0, "{style}");
        }
    }

    #[test]
    fn a_string_with_a_line_break_is_the_same_whatever_the_file_uses() {
        let spec = JobSpec {
            args: vec!["line one\nline two".to_owned(), "tab\there".to_owned()],
            ..nightly()
        };
        for sample in [SAMPLE.to_owned(), SAMPLE.replace('\n', "\r\n")] {
            let added = add(&sample, "nightly", &spec).unwrap();
            assert_eq!(read(&added, "nightly").unwrap(), spec);
            let changed = update(&sample, "linear-updates", &spec).unwrap();
            assert_eq!(read(&changed, "linear-updates").unwrap(), spec);
            // Nothing is written raw across lines.
            assert!(!added.contains("line one\n"));
            assert!(!added.contains("line one\r\n"));
        }
    }

    #[test]
    fn editing_one_job_never_changes_a_byte_of_another() {
        // A file with Windows line endings whose second job holds a string
        // with a bare line feed in it, on purpose.
        let other = "[jobs.b]\r\nagent = \"codex\"\r\nprompt = \"b.md\"\r\nworkdir = \".\"\r\nschedule = { at = \"08:00\" }\r\nargs = [\"-p\", \"\"\"line one\nline two\"\"\"]\r\n";
        let text = format!(
            "[jobs.a]\r\nagent = \"claude\"\r\nprompt = \"a.md\"\r\nworkdir = \".\"\r\nschedule = {{ at = \"07:00\" }}\r\n\r\n{other}"
        );
        let before = read(&text, "b").unwrap();
        let spec = JobSpec {
            at: "09:00".to_owned(),
            ..read(&text, "a").unwrap()
        };
        let changed = update(&text, "a", &spec).unwrap();
        assert!(changed.ends_with(other), "{changed:?}");
        assert_eq!(read(&changed, "b").unwrap(), before);
        assert_eq!(changed, text.replace("07:00", "09:00"));
    }

    #[test]
    fn a_file_is_given_back_as_it_was_whatever_its_shape() {
        let job = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule={at=\"07:00\",days=[\"mon\"]}   # keep";
        let shapes = [
            // No line break at the end.
            job.to_owned(),
            // A byte order mark.
            format!("\u{feff}{job}\n"),
            // Both line endings in one file.
            format!("# x\r\n{job}\n"),
            // Tabs and a header with a comment.
            job.replace("[jobs.a]", "[ jobs . a ]\t# the one")
                .replace("agent = ", "\tagent\t=\t"),
        ];
        for shape in shapes {
            let spec = read(&shape, "a").unwrap();
            assert_eq!(update(&shape, "a", &spec).unwrap(), shape);
            let later = JobSpec {
                at: "08:15".to_owned(),
                ..spec
            };
            assert_eq!(
                update(&shape, "a", &later).unwrap(),
                shape.replace("07:00", "08:15")
            );
        }
    }

    #[test]
    fn changing_the_days_touches_nothing_around_them() {
        let compact = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule={at=\"07:00\",days=[\"mon\"]}   # keep\n";
        let spec = JobSpec {
            days: vec![Weekday::Tue],
            ..read(compact, "a").unwrap()
        };
        assert_eq!(
            update(compact, "a", &spec).unwrap(),
            compact.replace("[\"mon\"]", "[\"tue\"]")
        );

        // An inline table over several lines, with a comment on another key.
        let tall = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = {\n  at = \"07:00\", # early\n  days = [\"mon\"],\n}\n";
        let spec = JobSpec {
            days: vec![Weekday::Tue],
            ..read(tall, "a").unwrap()
        };
        assert_eq!(
            update(tall, "a", &spec).unwrap(),
            tall.replace("[\"mon\"]", "[\"tue\"]")
        );
    }

    #[test]
    fn a_comment_above_a_key_that_goes_stays() {
        let none = JobSpec {
            args: Vec::new(),
            ..read(SAMPLE, "linear-updates").unwrap()
        };
        let text = update(SAMPLE, "linear-updates", &none).unwrap();
        assert!(
            text.contains("# Extra arguments for the agent CLI."),
            "{text}"
        );
        assert!(text.contains("# permission prompts, so the agent needs"));
        assert_eq!(
            text,
            SAMPLE.replace("args = [\"--permission-mode\", \"auto\"]\n", "")
        );
    }

    #[test]
    fn removing_a_job_takes_only_what_is_the_jobs() {
        let text = "\
# FILE HEADER

[jobs.a]
agent = \"claude\"
prompt = \"a.md\"
workdir = \".\"
schedule = { at = \"07:00\" }

# --- section ---

# about b
[jobs.b]   # the second
agent = \"codex\"
prompt = \"b.md\"
workdir = \".\"
# why these arguments
args = [\"-x\"]

[jobs.b.schedule]
at = \"08:00\"

# --- tail ---

[jobs.c]
agent = \"codex\"
prompt = \"c.md\"
workdir = \".\"
schedule = { at = \"09:00\" }
";
        let b_starts = text.find("# about b").unwrap();
        let tail_starts = text.find("# --- tail ---").unwrap();
        assert_eq!(
            remove(text, "b").unwrap(),
            format!("{}{}", &text[..b_starts], &text[tail_starts..])
        );
        let a_starts = text.find("[jobs.a]").unwrap();
        let section_starts = text.find("# --- section ---").unwrap();
        assert_eq!(
            remove(text, "a").unwrap(),
            format!("{}{}", &text[..a_starts], &text[section_starts..])
        );
        let c_starts = text.find("\n[jobs.c]").unwrap();
        assert_eq!(remove(text, "c").unwrap(), &text[..c_starts]);
    }

    #[test]
    fn what_the_file_opens_with_is_never_removed() {
        for header in ["[jobs.a]   # weekdays", "[ jobs.a ]", "[jobs.\"a\"]"] {
            let text = format!(
                "# Jobs for this machine.\n# Do not run these while travelling.\n\n{header}\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = {{ at = \"07:00\" }}\n\n[jobs.b]\nagent = \"codex\"\nprompt = \"b.md\"\nworkdir = \".\"\nschedule = {{ at = \"08:00\" }}\n"
            );
            let removed = remove(&text, "a").unwrap();
            assert!(
                removed.starts_with(
                    "# Jobs for this machine.\n# Do not run these while travelling.\n\n[jobs.b]\n"
                ),
                "{header}: {removed}"
            );
        }
        // Right above the only job, with no blank line: still the file's.
        let tight = "# FILE HEADER\n# more\n[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = { at = \"07:00\" }\n";
        assert_eq!(remove(tight, "a").unwrap(), "# FILE HEADER\n# more\n");
        // And behind a byte order mark.
        let marked = format!(
            "\u{feff}# FILE HEADER\n\n{}",
            &tight["# FILE HEADER\n# more\n".len()..]
        );
        assert_eq!(remove(&marked, "a").unwrap(), "\u{feff}# FILE HEADER\n");
    }

    #[test]
    fn adding_and_removing_a_job_gives_the_file_back() {
        let with_notes = format!("{SAMPLE}\n# trailing notes\n");
        for text in [SAMPLE.to_owned(), with_notes, SAMPLE.replace('\n', "\r\n")] {
            let added = add(&text, "nightly", &nightly()).unwrap();
            assert_eq!(remove(&added, "nightly").unwrap(), text);
        }
    }

    #[test]
    fn a_new_job_is_one_blank_line_from_what_is_there() {
        let spaced = format!("{SAMPLE}\n\n\n");
        let added = add(&spaced, "nightly", &nightly()).unwrap();
        assert!(added.starts_with(&spaced));
        assert!(added[spaced.len()..].starts_with("[jobs.nightly]\n"));
    }

    #[test]
    fn a_file_that_is_already_invalid_says_so() {
        let broken = "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = { at = \"07:00\" }\nbogus = 1\n";
        let spec = JobSpec {
            at: "08:00".to_owned(),
            ..read(broken, "a").unwrap()
        };
        for error in [
            update(broken, "a", &spec).unwrap_err(),
            add(broken, "nightly", &nightly()).unwrap_err(),
        ] {
            assert!(
                format!("{error:#}").contains("not valid as it is"),
                "{error:#}"
            );
        }
        // Taking the broken job out is how such a file is repaired.
        assert_eq!(remove(broken, "a").unwrap(), "");
    }

    #[test]
    fn read_gives_the_job_as_written() {
        assert_eq!(
            read(SAMPLE, "linear-updates").unwrap(),
            JobSpec {
                agent: Agent::Claude,
                prompt: "prompts/linear-updates.md".to_owned(),
                workdir: "~/Workspace/app".to_owned(),
                at: "16:05".to_owned(),
                days: WEEKDAYS.to_vec(),
                args: vec!["--permission-mode".to_owned(), "auto".to_owned()],
                notify: Level::Finish,
            }
        );
        let triage = read(SAMPLE, "morning-triage").unwrap();
        assert_eq!(triage.agent, Agent::Codex);
        assert_eq!(triage.days, Weekday::every_day());
        assert!(triage.args.is_empty());
        let error = read(SAMPLE, "gone").unwrap_err();
        assert!(format!("{error:#}").contains("no job named"));
    }

    #[test]
    fn changing_the_time_changes_nothing_else() {
        let spec = JobSpec {
            at: "17:30".to_owned(),
            ..read(SAMPLE, "linear-updates").unwrap()
        };
        assert_eq!(
            update(SAMPLE, "linear-updates", &spec).unwrap(),
            SAMPLE.replace("16:05", "17:30")
        );
    }

    #[test]
    fn changing_a_value_keeps_the_comment_beside_it() {
        let spec = JobSpec {
            agent: Agent::Codex,
            workdir: "~/elsewhere".to_owned(),
            ..read(SAMPLE, "linear-updates").unwrap()
        };
        let text = update(SAMPLE, "linear-updates", &spec).unwrap();
        assert!(text.contains("agent = \"codex\"                       # claude | codex\n"));
        assert!(text.contains("workdir = \"~/elsewhere\"            # where the agent runs\n"));
        assert_eq!(read(&text, "linear-updates").unwrap(), spec);
    }

    #[test]
    fn saving_what_is_already_there_changes_nothing() {
        for name in ["linear-updates", "morning-triage"] {
            let spec = read(SAMPLE, name).unwrap();
            assert_eq!(update(SAMPLE, name, &spec).unwrap(), SAMPLE);
        }
    }

    #[test]
    fn every_day_drops_the_days_key() {
        for days in [Weekday::every_day(), Vec::new()] {
            let spec = JobSpec {
                days,
                ..read(SAMPLE, "linear-updates").unwrap()
            };
            let text = update(SAMPLE, "linear-updates", &spec).unwrap();
            assert!(text.contains("schedule = { at = \"16:05\" }\n"), "{text}");
            assert!(text.contains("# where the agent runs"));
            assert_eq!(
                read(&text, "linear-updates").unwrap().days,
                Weekday::every_day()
            );
        }
    }

    #[test]
    fn days_are_added_to_a_schedule_that_had_none() {
        let spec = JobSpec {
            days: vec![Weekday::Sat, Weekday::Sun],
            ..read(SAMPLE, "morning-triage").unwrap()
        };
        let text = update(SAMPLE, "morning-triage", &spec).unwrap();
        assert!(
            text.contains("schedule = { at = \"07:00\", days = [\"sat\", \"sun\"] }"),
            "{text}"
        );
        // The comment beside the schedule is still beside it.
        assert!(text.contains("# no days: every day"));
        assert_eq!(read(&text, "morning-triage").unwrap(), spec);
    }

    #[test]
    fn arguments_come_and_go() {
        let none = JobSpec {
            args: Vec::new(),
            ..read(SAMPLE, "linear-updates").unwrap()
        };
        let text = update(SAMPLE, "linear-updates", &none).unwrap();
        assert!(!text.contains("args"), "{text}");
        assert!(text.contains("[jobs.morning-triage]"));
        assert_eq!(jobs(&text), 2);

        let some = JobSpec {
            args: vec!["--model".to_owned(), "x".to_owned()],
            ..read(SAMPLE, "morning-triage").unwrap()
        };
        let text = update(SAMPLE, "morning-triage", &some).unwrap();
        assert!(text.contains("args = [\"--model\", \"x\"]\n"), "{text}");
        assert_eq!(read(&text, "morning-triage").unwrap(), some);
    }

    #[test]
    fn a_job_without_the_key_reads_as_failures() {
        assert_eq!(
            read(SAMPLE, "morning-triage").unwrap().notify,
            Level::Failures
        );
        assert_eq!(
            read(SAMPLE, "linear-updates").unwrap().notify,
            Level::Finish
        );
    }

    #[test]
    fn a_level_otto_does_not_know_is_said_with_the_job() {
        let text = SAMPLE.replace("\"finish\"", "\"sometimes\"");
        let error = read(&text, "linear-updates").unwrap_err();
        let said = format!("{error:#}");
        assert!(said.contains("linear-updates"), "{said}");
        assert!(said.contains("sometimes"), "{said}");
    }

    #[test]
    fn a_key_written_by_hand_survives_a_change_of_the_job() {
        // `expect` is not a field of the form: a change leaves it where it is.
        let by_hand = format!("{SAMPLE}expect = \"DONE\"\n");
        let louder = JobSpec {
            notify: Level::All,
            ..read(&by_hand, "morning-triage").unwrap()
        };
        let text = update(&by_hand, "morning-triage", &louder).unwrap();
        assert_eq!(text, format!("{by_hand}notify = \"all\"\n"));
    }

    #[test]
    fn a_level_that_is_not_the_default_is_written() {
        let louder = JobSpec {
            notify: Level::All,
            ..read(SAMPLE, "morning-triage").unwrap()
        };
        let text = update(SAMPLE, "morning-triage", &louder).unwrap();
        assert_eq!(text, format!("{SAMPLE}notify = \"all\"\n"));
        assert_eq!(read(&text, "morning-triage").unwrap(), louder);
    }

    #[test]
    fn a_level_changes_where_it_is() {
        let quiet = JobSpec {
            notify: Level::Off,
            ..read(SAMPLE, "linear-updates").unwrap()
        };
        let text = update(SAMPLE, "linear-updates", &quiet).unwrap();
        assert_eq!(text, SAMPLE.replace("\"finish\"", "\"off\""));
    }

    #[test]
    fn going_back_to_the_default_drops_the_key() {
        let plain = JobSpec {
            notify: Level::Failures,
            ..read(SAMPLE, "linear-updates").unwrap()
        };
        let text = update(SAMPLE, "linear-updates", &plain).unwrap();
        // The line goes; the comment above it is the user's and stays.
        assert_eq!(text, SAMPLE.replace("notify = \"finish\"\n", ""));
        assert_eq!(read(&text, "linear-updates").unwrap(), plain);
    }

    #[test]
    fn the_default_written_out_is_left_as_it_is() {
        let text = SAMPLE.replace("\"finish\"", "\"failures\"");
        let same = read(&text, "linear-updates").unwrap();
        assert_eq!(same.notify, Level::Failures);
        assert_eq!(update(&text, "linear-updates", &same).unwrap(), text);
        // And another change does not take it out.
        let later = JobSpec {
            at: "17:00".to_owned(),
            ..same
        };
        let changed = update(&text, "linear-updates", &later).unwrap();
        assert!(changed.contains("notify = \"failures\""), "{changed}");
    }

    #[test]
    fn arguments_and_a_level_added_at_once_keep_their_order() {
        let both = JobSpec {
            args: vec!["--model".to_owned(), "x".to_owned()],
            notify: Level::All,
            ..read(SAMPLE, "morning-triage").unwrap()
        };
        let text = update(SAMPLE, "morning-triage", &both).unwrap();
        assert!(
            text.ends_with("args = [\"--model\", \"x\"]\nnotify = \"all\"\n"),
            "{text}"
        );
    }

    #[test]
    fn a_new_job_writes_the_level_after_the_arguments() {
        let spec = JobSpec {
            notify: Level::Finish,
            ..nightly()
        };
        let text = add(SAMPLE, "nightly", &spec).unwrap();
        assert!(
            text.ends_with("args = [\"--model\", \"x\"]\nnotify = \"finish\"\n"),
            "{text}"
        );
        assert_eq!(read(&text, "nightly").unwrap(), spec);
        // The level a job has anyway is not written.
        let text = add(SAMPLE, "nightly", &nightly()).unwrap();
        assert!(text.ends_with("args = [\"--model\", \"x\"]\n"), "{text}");
    }

    #[test]
    fn a_new_job_goes_at_the_end_in_a_fixed_shape() {
        let spec = nightly();
        let text = add(SAMPLE, "nightly", &spec).unwrap();
        assert!(text.starts_with(SAMPLE));
        assert!(text.ends_with(concat!(
            "\n[jobs.nightly]\n",
            "agent = \"codex\"\n",
            "prompt = \"prompts/nightly.md\"\n",
            "workdir = \"~/Workspace/app\"\n",
            "schedule = { at = \"02:00\", days = [\"mon\", \"wed\"] }\n",
            "args = [\"--model\", \"x\"]\n",
        )));
        assert_eq!(read(&text, "nightly").unwrap(), spec);
        assert_eq!(jobs(&text), 3);
    }

    #[test]
    fn a_job_with_every_day_and_no_args_writes_neither_key() {
        let spec = JobSpec {
            days: Weekday::every_day(),
            args: Vec::new(),
            ..nightly()
        };
        let text = add(SAMPLE, "nightly", &spec).unwrap();
        assert!(text.ends_with("schedule = { at = \"02:00\" }\n"), "{text}");
    }

    #[test]
    fn the_first_job_of_an_empty_file() {
        let text = add("", "nightly", &nightly()).unwrap();
        assert!(text.starts_with("[jobs.nightly]\n"));
        assert_eq!(read(&text, "nightly").unwrap(), nightly());
    }

    #[test]
    fn a_file_without_a_final_line_break_gets_one_before_the_job() {
        let text = add(SAMPLE.trim_end(), "nightly", &nightly()).unwrap();
        assert!(text.contains("# no days: every day\n\n[jobs.nightly]\n"));
        assert_eq!(jobs(&text), 3);
    }

    #[test]
    fn a_name_in_use_is_refused() {
        let error = add(SAMPLE, "morning-triage", &nightly()).unwrap_err();
        assert!(format!("{error:#}").contains("already exists"));
    }

    #[test]
    fn a_value_that_needs_quoting_survives() {
        let spec = JobSpec {
            workdir: "C:\\work\\my \"app\"".to_owned(),
            args: vec!["--note".to_owned(), "it's \"quoted\"\nand long".to_owned()],
            ..nightly()
        };
        let text = add(SAMPLE, "nightly", &spec).unwrap();
        assert_eq!(read(&text, "nightly").unwrap(), spec);
        let changed = update(&text, "linear-updates", &spec).unwrap();
        assert_eq!(read(&changed, "linear-updates").unwrap(), spec);
    }

    #[test]
    fn removing_a_job_leaves_the_others_alone() {
        let text = remove(SAMPLE, "morning-triage").unwrap();
        let before = &SAMPLE[..SAMPLE.find("[jobs.morning-triage]").unwrap()];
        assert!(text.starts_with(before.trim_end()), "{text}");
        assert!(!text.contains("morning-triage"));
        assert_eq!(jobs(&text), 1);

        let text = remove(SAMPLE, "linear-updates").unwrap();
        assert!(text.contains("# Copy to ~/.config/otto/jobs.toml and adapt."));
        assert!(text.contains("schedule = { at = \"07:00\" }            # no days: every day"));
        assert_eq!(jobs(&text), 1);
    }

    #[test]
    fn removing_the_first_job_keeps_what_the_file_opens_with() {
        let text = "\
# my jobs

# about a
[jobs.a]
agent = \"claude\"
prompt = \"a.md\"
workdir = \".\"
schedule = { at = \"07:00\" }

# about b
[jobs.b]
agent = \"codex\"
prompt = \"b.md\"
workdir = \".\"
schedule = { at = \"08:00\" }
";
        // The comment right above a job goes with it; the one a blank line
        // away belongs to the file.
        let without_a = remove(text, "a").unwrap();
        assert!(
            without_a.starts_with("# my jobs\n\n# about b\n[jobs.b]\n"),
            "{without_a}"
        );
        let without_b = remove(text, "b").unwrap();
        assert!(without_b.starts_with("# my jobs\n\n# about a\n[jobs.a]\n"));
        assert!(!without_b.contains("about b"));

        let crlf = text.replace('\n', "\r\n");
        let removed = remove(&crlf, "a").unwrap();
        assert!(removed.starts_with("# my jobs\r\n\r\n# about b\r\n[jobs.b]\r\n"));
        assert!(
            !removed.replace("\r\n", "").contains('\n'),
            "a bare line feed"
        );
    }

    #[test]
    fn the_last_job_can_be_removed() {
        let one = add("", "nightly", &nightly()).unwrap();
        assert_eq!(jobs(&remove(&one, "nightly").unwrap()), 0);
    }

    #[test]
    fn an_unknown_job_cannot_be_updated_or_removed() {
        for error in [
            update(SAMPLE, "gone", &nightly()).unwrap_err(),
            remove(SAMPLE, "gone").unwrap_err(),
        ] {
            assert!(format!("{error:#}").contains("no job named"));
        }
    }

    #[test]
    fn a_schedule_written_as_a_table_is_changed_in_place() {
        let text = "\
[jobs.a]
agent = \"claude\"
prompt = \"a.md\"
workdir = \".\"

[jobs.a.schedule]
at = \"07:00\"   # early
days = [\"mon\"]
";
        let spec = JobSpec {
            at: "08:00".to_owned(),
            ..read(text, "a").unwrap()
        };
        assert_eq!(
            update(text, "a", &spec).unwrap(),
            text.replace("07:00", "08:00")
        );
    }

    #[test]
    fn windows_line_endings_survive() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        let spec = JobSpec {
            at: "17:30".to_owned(),
            ..read(&crlf, "linear-updates").unwrap()
        };
        assert_eq!(
            update(&crlf, "linear-updates", &spec).unwrap(),
            crlf.replace("16:05", "17:30")
        );
        let added = add(&crlf, "nightly", &nightly()).unwrap();
        assert!(added.starts_with(&crlf));
        assert!(
            !added.replace("\r\n", "").contains('\n'),
            "a bare line feed"
        );
        assert_eq!(read(&added, "nightly").unwrap(), nightly());
    }

    #[test]
    fn seven_days_written_out_are_left_as_they_are() {
        let text = "\
[jobs.a]
agent = \"claude\"
prompt = \"a.md\"
workdir = \".\"
schedule = { at = \"07:00\", days = [\"mon\", \"tue\", \"wed\", \"thu\", \"fri\", \"sat\", \"sun\"] }
";
        let spec = read(text, "a").unwrap();
        assert_eq!(update(text, "a", &spec).unwrap(), text);
    }

    #[test]
    fn a_change_that_breaks_the_file_is_refused() {
        let late = JobSpec {
            at: "25:00".to_owned(),
            ..nightly()
        };
        let error = update(SAMPLE, "linear-updates", &late).unwrap_err();
        assert!(format!("{error:#}").contains("HH:MM"));
        assert!(add(SAMPLE, "nightly", &late).is_err());
        let error = add(SAMPLE, "Bad Name", &nightly()).unwrap_err();
        assert!(format!("{error:#}").contains("lowercase"));
        assert!(add(SAMPLE, "", &nightly()).is_err());
    }

    /// Reading takes an empty path, which then means the directory of the
    /// jobs file: a job nobody means to write.
    #[test]
    fn a_job_is_not_written_without_a_working_directory_or_a_prompt() {
        for (spec, said) in [
            (
                JobSpec {
                    workdir: String::new(),
                    ..nightly()
                },
                "workdir is empty",
            ),
            (
                JobSpec {
                    workdir: "  ".to_owned(),
                    ..nightly()
                },
                "workdir is empty",
            ),
            (
                JobSpec {
                    prompt: String::new(),
                    ..nightly()
                },
                "prompt is empty",
            ),
        ] {
            let error = add(SAMPLE, "nightly", &spec).unwrap_err();
            assert!(format!("{error:#}").contains(said), "{error:#}");
            let error = update(SAMPLE, "linear-updates", &spec).unwrap_err();
            assert!(format!("{error:#}").contains(said), "{error:#}");
        }
    }

    #[test]
    fn a_file_that_is_not_valid_is_not_edited() {
        assert!(add("jobs = 3\n", "nightly", &nightly()).is_err());
        assert!(update("[jobs", "a", &nightly()).is_err());
        assert!(remove("[jobs", "a").is_err());
    }

    #[test]
    fn save_refuses_a_file_that_changed_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        fs::write(&path, "# edited by hand\n").unwrap();
        let error = save(&path, SAMPLE, "anything").unwrap_err();
        assert!(format!("{error:#}").contains("changed on disk"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "# edited by hand\n");
        // A file that appeared where there was none counts too.
        assert!(save(&path, "", "anything").is_err());
    }

    #[test]
    fn save_creates_the_file_and_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("otto").join("jobs.toml");
        save(&path, "", SAMPLE).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), SAMPLE);
    }

    #[test]
    fn save_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        fs::write(&path, SAMPLE).unwrap();
        save(&path, SAMPLE, "# new\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "# new\n");
    }
}
