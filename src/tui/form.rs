//! The form that creates and edits a job: its fields, what is typed into
//! them, and the jobs file that would result. It reads and writes nothing;
//! whether it can be saved is what `jobs_file` says of the text it builds.

use anyhow::Result;
use unicode_segmentation::UnicodeSegmentation;

use crate::agent::Agent;
use crate::config::Weekday;
use crate::jobs_file::{self, JobSpec};

/// A line of text being edited. The cursor is a place in the text, always
/// between two things that read as one character each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    text: String,
    cursor: usize,
}

impl Field {
    /// A field holding `text`, with the cursor after it.
    pub fn new(text: &str) -> Field {
        Field {
            text: text.to_owned(),
            cursor: text.len(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where the cursor is, as a byte offset into the text.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The place before the cursor that is one character back.
    fn before(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(start, _)| start)
    }

    /// The place after the cursor that is one character on.
    fn after(&self) -> usize {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |next| self.cursor + next.len())
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        let start = self.before();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete(&mut self) {
        let end = self.after();
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.before();
    }

    pub fn right(&mut self) {
        self.cursor = self.after();
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }
}

/// The part of the form the keys go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Name,
    Agent,
    Workdir,
    At,
    Days,
    Args,
    Prompt,
}

/// The order the fields are walked in.
const ORDER: [Focus; 7] = [
    Focus::Name,
    Focus::Agent,
    Focus::Workdir,
    Focus::At,
    Focus::Days,
    Focus::Args,
    Focus::Prompt,
];

/// One keystroke of editing, whichever key it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Insert(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    /// The job being edited; `None` while creating one.
    pub editing: Option<String>,
    pub focus: Focus,
    pub name: Field,
    pub agent: Agent,
    pub workdir: Field,
    pub at: Field,
    /// Monday to Sunday.
    pub days: [bool; 7],
    /// The day under the cursor while the focus is on the days.
    pub day: usize,
    /// One argument to a line.
    pub args: Field,
    pub prompt: Field,
    /// The jobs file as it was when the form opened.
    pub read: String,
    /// The prompt path is still the one made from the name.
    prompt_follows: bool,
    /// What the form held when it opened, to tell whether it changed.
    opened_with: (String, JobSpec),
}

impl Form {
    /// An empty form for a new job. Nothing is filled in for the user, the
    /// arguments least of all: what an agent may do is theirs to write.
    pub fn create(read: String) -> Form {
        let mut form = Form {
            editing: None,
            focus: Focus::Name,
            name: Field::new(""),
            agent: Agent::Claude,
            workdir: Field::new(""),
            at: Field::new(""),
            days: [false; 7],
            day: 0,
            args: Field::new(""),
            prompt: Field::new(""),
            read,
            prompt_follows: true,
            opened_with: (
                String::new(),
                JobSpec {
                    agent: Agent::Claude,
                    prompt: String::new(),
                    workdir: String::new(),
                    at: String::new(),
                    days: Vec::new(),
                    args: Vec::new(),
                },
            ),
        };
        form.opened_with.1 = form.spec();
        form
    }

    /// A form holding the job `name` as the jobs file writes it.
    pub fn edit(read: String, name: &str) -> Result<Form> {
        let spec = jobs_file::read(&read, name)?;
        let week = Weekday::every_day();
        // A job that runs every day opens with no day marked, which says the
        // same and is what the file gets back.
        let mut days = [false; 7];
        if spec.days.len() < week.len() {
            for (marked, day) in days.iter_mut().zip(&week) {
                *marked = spec.days.contains(day);
            }
        }
        let mut form = Form {
            editing: Some(name.to_owned()),
            focus: Focus::Agent,
            name: Field::new(name),
            agent: spec.agent,
            workdir: Field::new(&spec.workdir),
            at: Field::new(&spec.at),
            days,
            day: 0,
            args: Field::new(&spec.args.join("\n")),
            prompt: Field::new(&spec.prompt),
            read,
            prompt_follows: false,
            opened_with: (name.to_owned(), spec),
        };
        form.opened_with.1 = form.spec();
        Ok(form)
    }

    /// The job the form describes. No day marked and all seven both come out
    /// as no days, which the jobs file reads as every day.
    pub fn spec(&self) -> JobSpec {
        let marked: Vec<Weekday> = Weekday::every_day()
            .into_iter()
            .zip(self.days)
            .filter_map(|(day, marked)| marked.then_some(day))
            .collect();
        JobSpec {
            agent: self.agent,
            prompt: self.prompt.text().to_owned(),
            workdir: self.workdir.text().to_owned(),
            at: self.at.text().to_owned(),
            days: if marked.len() == self.days.len() {
                Vec::new()
            } else {
                marked
            },
            args: self
                .args
                .text()
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    /// The jobs file with this job in it, or why it cannot be saved. The
    /// reason is the one reading the file would give: the form has no rules
    /// of its own.
    pub fn result(&self) -> Result<String> {
        match &self.editing {
            Some(name) => jobs_file::update(&self.read, name, &self.spec()),
            None => jobs_file::add(&self.read, self.name.text(), &self.spec()),
        }
    }

    /// Whether anything differs from what the form opened with.
    pub fn dirty(&self) -> bool {
        (self.name.text(), &self.spec()) != (self.opened_with.0.as_str(), &self.opened_with.1)
    }

    /// The fields the focus can be on: a job that exists keeps its name.
    fn reachable(&self) -> &'static [Focus] {
        if self.editing.is_some() {
            &ORDER[1..]
        } else {
            &ORDER
        }
    }

    fn step(&mut self, by: usize) {
        let fields = self.reachable();
        let at = fields.iter().position(|field| *field == self.focus);
        // From a field that cannot be reached, the walk starts at the first.
        self.focus = at.map_or(fields[0], |at| fields[(at + by) % fields.len()]);
    }

    pub fn next_field(&mut self) {
        self.step(1);
    }

    pub fn prev_field(&mut self) {
        self.step(self.reachable().len() - 1);
    }

    /// The next agent, or the day under the cursor on or off.
    pub fn toggle(&mut self) {
        match self.focus {
            Focus::Agent => {
                let at = Agent::ALL.iter().position(|agent| *agent == self.agent);
                self.agent = Agent::ALL[at.map_or(0, |at| (at + 1) % Agent::ALL.len())];
            }
            Focus::Days => self.days[self.day] = !self.days[self.day],
            _ => {}
        }
    }

    /// One keystroke, to whatever has the focus.
    pub fn input(&mut self, edit: Edit) {
        let field = match self.focus {
            // Chosen, not typed: left and right go through the choices.
            Focus::Agent => {
                if matches!(edit, Edit::Left | Edit::Right) {
                    self.toggle();
                }
                return;
            }
            Focus::Days => {
                self.day = match edit {
                    Edit::Left => self.day.saturating_sub(1),
                    Edit::Right => (self.day + 1).min(self.days.len() - 1),
                    Edit::Home => 0,
                    Edit::End => self.days.len() - 1,
                    _ => self.day,
                };
                return;
            }
            // The name is the job's history and its unit: it does not change.
            Focus::Name if self.editing.is_some() => return,
            Focus::Name => &mut self.name,
            Focus::Workdir => &mut self.workdir,
            Focus::At => &mut self.at,
            Focus::Args => &mut self.args,
            Focus::Prompt => &mut self.prompt,
        };
        let before = field.text().to_owned();
        match edit {
            Edit::Insert(c) => field.insert(c),
            Edit::Backspace => field.backspace(),
            Edit::Delete => field.delete(),
            Edit::Left => field.left(),
            Edit::Right => field.right(),
            Edit::Home => field.home(),
            Edit::End => field.end(),
        }
        let changed = field.text() != before;
        match self.focus {
            // A prompt the user has put a hand on is theirs from then on.
            Focus::Prompt if changed => self.prompt_follows = false,
            Focus::Name if changed && self.prompt_follows => {
                self.prompt = Field::new(&match self.name.text() {
                    "" => String::new(),
                    name => format!("prompts/{name}.md"),
                });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../../examples/jobs.toml");

    fn typed(form: &mut Form, text: &str) {
        for c in text.chars() {
            form.input(Edit::Insert(c));
        }
    }

    /// A new form with a name, a working directory and a time typed in.
    fn nightly() -> Form {
        let mut form = Form::create(SAMPLE.to_owned());
        typed(&mut form, "nightly");
        form.focus = Focus::Workdir;
        typed(&mut form, "~/app");
        form.focus = Focus::At;
        typed(&mut form, "02:00");
        form
    }

    #[test]
    fn a_field_edits_in_the_middle() {
        let mut field = Field::new("ação");
        assert_eq!(field.cursor(), "ação".len());
        field.left();
        field.left();
        field.insert('X');
        assert_eq!(field.text(), "açXão");
        field.backspace();
        assert_eq!(field.text(), "ação");
        field.home();
        field.delete();
        assert_eq!(field.text(), "ção");
        field.end();
        field.insert('!');
        assert_eq!(field.text(), "ção!");
    }

    #[test]
    fn a_field_never_moves_past_its_ends() {
        let mut field = Field::new("ab");
        field.right();
        field.delete();
        assert_eq!((field.text(), field.cursor()), ("ab", 2));
        field.home();
        field.left();
        field.backspace();
        assert_eq!((field.text(), field.cursor()), ("ab", 0));
        let mut empty = Field::new("");
        empty.backspace();
        empty.delete();
        empty.left();
        empty.right();
        assert_eq!((empty.text(), empty.cursor()), ("", 0));
    }

    #[test]
    fn a_field_steps_over_what_reads_as_one_character() {
        let mut field = Field::new("a👨\u{200d}👩\u{200d}👧e\u{301}");
        field.left();
        assert_eq!(&field.text()[field.cursor()..], "e\u{301}");
        field.left();
        assert_eq!(field.cursor(), 1);
        field.delete();
        assert_eq!(field.text(), "ae\u{301}");
        field.end();
        field.backspace();
        assert_eq!(field.text(), "a");
    }

    #[test]
    fn a_new_form_is_empty_and_starts_at_the_name() {
        let form = Form::create(SAMPLE.to_owned());
        assert_eq!(form.focus, Focus::Name);
        assert_eq!(form.editing, None);
        assert_eq!(form.agent, Agent::Claude);
        assert_eq!(form.days, [false; 7]);
        assert!(!form.dirty());
        let spec = form.spec();
        assert!(spec.workdir.is_empty() && spec.at.is_empty());
    }

    #[test]
    fn the_form_never_fills_in_arguments() {
        for agent in Agent::ALL {
            let mut form = Form::create(SAMPLE.to_owned());
            form.agent = agent;
            assert!(form.spec().args.is_empty());
        }
    }

    #[test]
    fn the_prompt_follows_the_name_until_it_is_edited() {
        let mut form = Form::create(SAMPLE.to_owned());
        assert_eq!(form.prompt.text(), "");
        typed(&mut form, "nightly");
        assert_eq!(form.prompt.text(), "prompts/nightly.md");
        form.input(Edit::Backspace);
        assert_eq!(form.prompt.text(), "prompts/nightl.md");

        // Moving the cursor in the prompt is not editing it.
        form.focus = Focus::Prompt;
        form.input(Edit::Left);
        form.focus = Focus::Name;
        typed(&mut form, "y");
        assert_eq!(form.prompt.text(), "prompts/nightly.md");

        form.focus = Focus::Prompt;
        form.input(Edit::End);
        form.input(Edit::Backspace);
        form.input(Edit::Backspace);
        typed(&mut form, "txt");
        form.focus = Focus::Name;
        typed(&mut form, "-2");
        assert_eq!(form.prompt.text(), "prompts/nightly.txt");
    }

    #[test]
    fn a_new_form_becomes_a_job() {
        let form = nightly();
        assert!(form.dirty());
        let spec = form.spec();
        assert_eq!(spec.prompt, "prompts/nightly.md");
        assert_eq!(
            form.result().unwrap(),
            jobs_file::add(SAMPLE, "nightly", &spec).unwrap()
        );
    }

    #[test]
    fn editing_starts_from_the_file() {
        let form = Form::edit(SAMPLE.to_owned(), "linear-updates").unwrap();
        assert_eq!(form.editing.as_deref(), Some("linear-updates"));
        assert_eq!(form.focus, Focus::Agent);
        assert_eq!(form.name.text(), "linear-updates");
        assert_eq!(form.at.text(), "16:05");
        assert_eq!(form.days, [true, true, true, true, true, false, false]);
        assert_eq!(form.args.text(), "--permission-mode\nauto");
        assert_eq!(form.prompt.text(), "prompts/linear-updates.md");
        assert!(!form.dirty());
        assert!(Form::edit(SAMPLE.to_owned(), "gone").is_err());
    }

    #[test]
    fn editing_without_changes_gives_the_same_file() {
        for name in ["linear-updates", "morning-triage"] {
            let form = Form::edit(SAMPLE.to_owned(), name).unwrap();
            assert_eq!(form.result().unwrap(), SAMPLE);
        }
        // A job that runs every day opens with no day marked.
        let form = Form::edit(SAMPLE.to_owned(), "morning-triage").unwrap();
        assert_eq!(form.days, [false; 7]);
    }

    #[test]
    fn an_edit_changes_the_one_value() {
        let mut form = Form::edit(SAMPLE.to_owned(), "linear-updates").unwrap();
        form.focus = Focus::At;
        form.input(Edit::Backspace);
        form.input(Edit::Backspace);
        typed(&mut form, "30");
        assert!(form.dirty());
        assert_eq!(form.result().unwrap(), SAMPLE.replace("16:05", "16:30"));
    }

    #[test]
    fn the_name_cannot_be_reached_or_changed_while_editing() {
        let mut form = Form::edit(SAMPLE.to_owned(), "linear-updates").unwrap();
        form.focus = Focus::Prompt;
        form.next_field();
        assert_eq!(form.focus, Focus::Agent);
        form.prev_field();
        assert_eq!(form.focus, Focus::Prompt);
        // Even put there, typing does nothing.
        form.focus = Focus::Name;
        typed(&mut form, "x");
        assert_eq!(form.name.text(), "linear-updates");
    }

    #[test]
    fn the_fields_come_in_one_order_and_wrap() {
        let mut form = Form::create(SAMPLE.to_owned());
        let mut seen = vec![form.focus];
        for _ in 0..7 {
            form.next_field();
            seen.push(form.focus);
        }
        assert_eq!(
            seen,
            [
                Focus::Name,
                Focus::Agent,
                Focus::Workdir,
                Focus::At,
                Focus::Days,
                Focus::Args,
                Focus::Prompt,
                Focus::Name
            ]
        );
        form.prev_field();
        assert_eq!(form.focus, Focus::Prompt);
    }

    #[test]
    fn the_agent_and_the_days_are_chosen_not_typed() {
        let mut form = Form::create(SAMPLE.to_owned());
        form.focus = Focus::Agent;
        form.toggle();
        assert_eq!(form.agent, Agent::Codex);
        form.input(Edit::Right);
        assert_eq!(form.agent, Agent::Claude);
        typed(&mut form, "x");
        assert_eq!(form.agent, Agent::Claude);

        form.focus = Focus::Days;
        form.toggle();
        form.input(Edit::Right);
        form.input(Edit::Right);
        form.toggle();
        assert_eq!(form.day, 2);
        assert_eq!(form.days, [true, false, true, false, false, false, false]);
        assert_eq!(form.spec().days, [Weekday::Mon, Weekday::Wed]);
        form.input(Edit::Home);
        form.input(Edit::Left);
        assert_eq!(form.day, 0);
        form.input(Edit::End);
        form.input(Edit::Right);
        assert_eq!(form.day, 6);
        form.toggle();
        form.toggle();
        assert!(!form.days[6]);
    }

    #[test]
    fn no_day_and_every_day_are_the_same() {
        let mut form = nightly();
        assert!(form.spec().days.is_empty());
        form.days = [true; 7];
        assert!(form.spec().days.is_empty());
        assert!(
            form.result()
                .unwrap()
                .ends_with("schedule = { at = \"02:00\" }\n")
        );
    }

    #[test]
    fn arguments_are_one_to_a_line_and_blank_lines_are_dropped() {
        let mut form = nightly();
        form.focus = Focus::Args;
        typed(&mut form, "--model\n\n  \nsonnet 5\n");
        assert_eq!(form.spec().args, ["--model", "sonnet 5"]);
    }

    #[test]
    fn the_reason_it_cannot_be_saved_comes_from_the_file_rules() {
        let reason = |form: &Form| format!("{:#}", form.result().unwrap_err());

        let mut late = nightly();
        late.at = Field::new("25:00");
        assert!(reason(&late).contains("HH:MM"), "{}", reason(&late));

        let mut bad = nightly();
        bad.name = Field::new("Bad Name");
        assert!(reason(&bad).contains("lowercase"));

        let mut taken = nightly();
        taken.name = Field::new("linear-updates");
        assert!(reason(&taken).contains("already exists"));

        let mut nameless = nightly();
        nameless.name = Field::new("");
        assert!(nameless.result().is_err());
    }
}
