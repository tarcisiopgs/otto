//! Writing the jobs file. It stays the user's file: a change touches the keys
//! that changed and leaves every comment, the order and the layout as they
//! were. Nothing else writes `jobs.toml`.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use toml_edit::{Array, DocumentMut, Item, TableLike, Value};

use crate::agent::Agent;
use crate::atomic;
use crate::config::{self, Config, Weekday};

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

fn parse(text: &str) -> Result<DocumentMut> {
    text.parse()
        .context("the jobs file is not valid TOML as it is")
}

/// The same rules the jobs file is read with. Paths are not looked at.
fn checked(text: String) -> Result<String> {
    Config::parse(&text, Path::new("."), None).context("the jobs file would not be valid")?;
    Ok(text)
}

fn no_job(name: &str) -> String {
    format!("no job named {name:?}")
}

fn array(items: impl IntoIterator<Item = impl Into<Value>>) -> Value {
    let mut array: Array = items.into_iter().collect();
    array.fmt();
    Value::Array(array)
}

/// The job `name` as it is written in `text`. A schedule with no `days` comes
/// back with all seven.
pub fn read(text: &str, name: &str) -> Result<JobSpec> {
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
    Ok(JobSpec {
        agent,
        prompt: string(job, "prompt")?,
        workdir: string(job, "workdir")?,
        at: string(schedule, "at")?,
        days,
        args: strings(job, "args")?.unwrap_or_default(),
    })
}

/// `text` with a new job at its end. The block always has the same shape:
/// agent, prompt, workdir, schedule and, when there are any, args.
pub fn add(text: &str, name: &str, spec: &JobSpec) -> Result<String> {
    // Checked here: a name that is not a bare key would not even parse below.
    if !config::is_job_name(name) {
        bail!("job name {name:?} must be lowercase letters, digits and dashes");
    }
    let taken = parse(text)?
        .get("jobs")
        .and_then(Item::as_table_like)
        .is_some_and(|jobs| jobs.contains_key(name));
    if taken {
        bail!("job {name:?} already exists");
    }
    // Written as text, so the block has one shape whatever the file looks
    // like, in the line ending the file already uses.
    let end = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let quoted = |text: &str| Value::from(text).to_string();
    let mut schedule = format!("{{ at = {}", quoted(&spec.at));
    if !spec.every_day() {
        let days = array(spec.week().into_iter().map(Weekday::toml_name));
        schedule.push_str(&format!(", days = {days}"));
    }
    schedule.push_str(" }");
    let mut lines = vec![
        format!("[jobs.{name}]"),
        format!("agent = {}", quoted(spec.agent.program())),
        format!("prompt = {}", quoted(&spec.prompt)),
        format!("workdir = {}", quoted(&spec.workdir)),
        format!("schedule = {schedule}"),
    ];
    if !spec.args.is_empty() {
        lines.push(format!(
            "args = {}",
            array(spec.args.iter().map(String::as_str))
        ));
    }

    let mut out = text.to_owned();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push_str(end);
        }
        out.push_str(end);
    }
    for line in lines {
        out.push_str(&line);
        out.push_str(end);
    }
    checked(out)
}

/// Puts `new` where `key` is, keeping the spacing and the comment the old
/// value had beside it. A key that is not there yet goes at the end.
fn set(table: &mut dyn TableLike, key: &str, mut new: Value) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(old) => {
            *new.decor_mut() = old.decor().clone();
            *old = new;
        }
        None => {
            table.insert(key, Item::Value(new));
        }
    }
}

/// `text` with the job `name` changed to `spec`. Only what differs from what
/// is written is touched, so saving a job as it is gives the same text back.
pub fn update(text: &str, name: &str, spec: &JobSpec) -> Result<String> {
    let old = read(text, name)?;
    let mut doc = parse(text)?;
    let job = doc
        .get_mut("jobs")
        .and_then(Item::as_table_like_mut)
        .and_then(|jobs| jobs.get_mut(name))
        .and_then(Item::as_table_like_mut)
        .with_context(|| no_job(name))?;

    if old.agent != spec.agent {
        set(job, "agent", spec.agent.program().into());
    }
    if old.prompt != spec.prompt {
        set(job, "prompt", spec.prompt.as_str().into());
    }
    if old.workdir != spec.workdir {
        set(job, "workdir", spec.workdir.as_str().into());
    }
    let days_changed = old.week() != spec.week();
    if old.at != spec.at || days_changed {
        let item = job
            .get_mut("schedule")
            .with_context(|| format!("job {name}: schedule must be a table"))?;
        if let Some(schedule) = item.as_table_like_mut() {
            if old.at != spec.at {
                set(schedule, "at", spec.at.as_str().into());
            }
            if days_changed && spec.every_day() {
                schedule.remove("days");
            } else if days_changed {
                let days = array(spec.week().into_iter().map(Weekday::toml_name));
                set(schedule, "days", days);
            }
        }
        // A key that came or went leaves an inline table with uneven spacing.
        if days_changed && let Some(inline) = item.as_inline_table_mut() {
            inline.fmt();
        }
    }
    if old.args != spec.args {
        if spec.args.is_empty() {
            job.remove("args");
        } else {
            set(job, "args", array(spec.args.iter().map(String::as_str)));
        }
    }
    checked(written(doc.to_string(), text))
}

/// `text` without the job `name`.
pub fn remove(text: &str, name: &str) -> Result<String> {
    let mut doc = parse(text)?;
    doc.get_mut("jobs")
        .and_then(Item::as_table_like_mut)
        .and_then(|jobs| jobs.remove(name))
        .with_context(|| no_job(name))?;
    // The parser hangs everything above the first table on that table, so
    // removing it would take what the file opens with.
    let mut out = doc.to_string();
    let opening = opening(text, name).replace("\r\n", "\n");
    if !opening.is_empty() {
        out = format!("{opening}{}", out.trim_start_matches('\n'));
    }
    checked(written(out, text))
}

/// What the file opens with, when the job `name` is the first thing in it:
/// the comments and blank lines down to the last blank line before the job.
/// A comment right above the job, with no blank line between, is the job's.
fn opening<'a>(text: &'a str, name: &str) -> &'a str {
    let header = format!("[jobs.{name}]");
    let (mut at, mut end) = (0, 0);
    for line in text.split_inclusive('\n') {
        let content = line.trim();
        at += line.len();
        if content.is_empty() {
            end = at;
        } else if !content.starts_with('#') {
            return if content == header { &text[..end] } else { "" };
        }
    }
    ""
}

/// `edited`, as the document writer gives it, in the line ending of `original`.
/// The parser reads `\r\n` as `\n` and writes `\n` back.
fn written(edited: String, original: &str) -> String {
    if original.contains("\r\n") {
        edited.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        edited
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
        bail!("the jobs file changed on disk since it was read");
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
        }
    }

    fn jobs(text: &str) -> usize {
        Config::parse(text, Path::new("/etc/otto"), None)
            .unwrap()
            .jobs
            .len()
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
