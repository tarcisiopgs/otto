//! The form that creates and edits a job: its fields, what is typed into
//! them, and the jobs file that would result. It reads and writes nothing;
//! whether it can be saved is what `jobs_file` says of the text it builds.

use anyhow::Result;
use unicode_segmentation::UnicodeSegmentation;

use crate::agent::Agent;
use crate::config::Weekday;
use crate::jobs_file::{self, JobSpec};
use crate::notify::Level;

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
    Notify,
}

/// The order the fields are walked in.
const ORDER: [Focus; 8] = [
    Focus::Name,
    Focus::Agent,
    Focus::At,
    Focus::Days,
    Focus::Workdir,
    Focus::Prompt,
    Focus::Args,
    Focus::Notify,
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
    /// The job has arguments this field cannot hold: one with a line break,
    /// with a space at an end, or empty. They are kept as the file has them
    /// and can only be changed there.
    pub args_by_hand: bool,
    /// The arguments as the file has them, for as long as the field is not
    /// edited: what is read back from a field is only what it can hold.
    written_args: Option<Vec<String>>,
    pub prompt: Field,
    /// Which runs the job tells of.
    pub notify: Level,
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
            args_by_hand: false,
            written_args: None,
            prompt: Field::new(""),
            notify: Level::default(),
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
                    notify: Level::default(),
                },
            ),
        };
        form.opened_with.1 = form.spec();
        form
    }

    /// A form holding the job `name` as the jobs file writes it.
    pub fn edit(read: String, name: &str) -> Result<Form> {
        let spec = jobs_file::read(&read, name)?;
        let mut days = [false; 7];
        for (marked, day) in days.iter_mut().zip(Weekday::every_day()) {
            *marked = spec.days.contains(&day);
        }
        // A job that runs every day opens with no day marked, which says the
        // same and is what the file gets back.
        if days == [true; 7] {
            days = [false; 7];
        }
        let fits_a_line =
            |arg: &String| !arg.is_empty() && arg.trim() == arg && !arg.contains(['\n', '\r']);
        let args_by_hand = !spec.args.iter().all(fits_a_line);
        let mut form = Form {
            editing: Some(name.to_owned()),
            focus: Focus::Agent,
            name: Field::new(name),
            agent: spec.agent,
            workdir: Field::new(&spec.workdir),
            at: Field::new(&spec.at),
            days,
            day: 0,
            args: Field::new(&if args_by_hand {
                String::new()
            } else {
                spec.args.join("\n")
            }),
            args_by_hand,
            written_args: Some(spec.args.clone()),
            prompt: Field::new(&spec.prompt),
            notify: spec.notify,
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
            args: match &self.written_args {
                Some(written) => written.clone(),
                None => self
                    .args
                    .text()
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect(),
            },
            notify: self.notify,
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

    /// The next agent or the next level, or the day under the cursor on or
    /// off.
    pub fn toggle(&mut self) {
        match self.focus {
            Focus::Agent => {
                let at = Agent::ALL.iter().position(|agent| *agent == self.agent);
                self.agent = Agent::ALL[at.map_or(0, |at| (at + 1) % Agent::ALL.len())];
            }
            Focus::Days => self.days[self.day] = !self.days[self.day],
            Focus::Notify => {
                let at = Level::ALL.iter().position(|level| *level == self.notify);
                self.notify = Level::ALL[at.map_or(0, |at| (at + 1) % Level::ALL.len())];
            }
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
            // Typing here would replace arguments the field cannot show.
            Focus::Args if self.args_by_hand => return,
            // Chosen too, and in an order: left is less and right is more.
            Focus::Notify => {
                let at = Level::ALL
                    .iter()
                    .position(|level| *level == self.notify)
                    .unwrap_or(0);
                self.notify = match edit {
                    Edit::Left => Level::ALL[at.saturating_sub(1)],
                    Edit::Right => Level::ALL[(at + 1).min(Level::ALL.len() - 1)],
                    Edit::Home => Level::ALL[0],
                    Edit::End => Level::ALL[Level::ALL.len() - 1],
                    _ => self.notify,
                };
                return;
            }
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
            // From here on the arguments are what the field holds.
            Focus::Args if changed => self.written_args = None,
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
        form.focus = Focus::Notify;
        form.next_field();
        assert_eq!(form.focus, Focus::Agent);
        form.prev_field();
        assert_eq!(form.focus, Focus::Notify);
        // Even put there, typing does nothing.
        form.focus = Focus::Name;
        typed(&mut form, "x");
        assert_eq!(form.name.text(), "linear-updates");
    }

    #[test]
    fn the_fields_come_in_one_order_and_wrap() {
        let mut form = Form::create(SAMPLE.to_owned());
        let mut seen = vec![form.focus];
        for _ in 0..8 {
            form.next_field();
            seen.push(form.focus);
        }
        assert_eq!(
            seen,
            [
                Focus::Name,
                Focus::Agent,
                Focus::At,
                Focus::Days,
                Focus::Workdir,
                Focus::Prompt,
                Focus::Args,
                Focus::Notify,
                Focus::Name
            ]
        );
        form.prev_field();
        assert_eq!(form.focus, Focus::Notify);
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

    /// One job, `a`, at 07:00, with these arguments as TOML.
    fn with_args(args: &str) -> String {
        format!(
            "[jobs.a]\nagent = \"claude\"\nprompt = \"a.md\"\nworkdir = \".\"\nschedule = {{ at = \"07:00\" }}\nargs = {args}\n"
        )
    }

    /// A line break, spaces at the ends, an argument that is empty: none of
    /// these fits a field that holds one argument to a line.
    #[test]
    fn arguments_the_field_cannot_hold_are_left_as_written() {
        for args in [
            r#"["--append-system-prompt", "line one\nline two"]"#,
            r#"["-p", " padded "]"#,
            r#"["--flag", ""]"#,
        ] {
            let text = with_args(args);
            let mut form = Form::edit(text.clone(), "a").unwrap();
            assert!(form.args_by_hand, "{args}");
            assert!(!form.dirty());
            let written = jobs_file::read(&text, "a").unwrap().args;
            assert_eq!(form.spec().args, written);

            // Another field changes: the arguments stay byte for byte.
            form.focus = Focus::At;
            form.input(Edit::Backspace);
            typed(&mut form, "5");
            assert_eq!(form.result().unwrap(), text.replace("07:00", "07:05"));

            // And the field does not take typing that would lose them.
            form.focus = Focus::Args;
            typed(&mut form, "x");
            form.input(Edit::Backspace);
            assert_eq!(form.spec().args, written);
        }
    }

    #[test]
    fn arguments_are_written_again_only_when_their_field_is_edited() {
        let text = with_args(r#"["--model",   "sonnet"]"#);
        let mut form = Form::edit(text.clone(), "a").unwrap();
        assert!(!form.args_by_hand);
        assert_eq!(form.args.text(), "--model\nsonnet");
        form.focus = Focus::Args;
        // Moving in the field is not editing it.
        form.input(Edit::Left);
        assert_eq!(form.result().unwrap(), text);
        form.input(Edit::End);
        typed(&mut form, "\n--verbose");
        assert_eq!(form.spec().args, ["--model", "sonnet", "--verbose"]);
        assert!(form.dirty());
    }

    #[test]
    fn seven_entries_of_one_day_are_that_day() {
        let text = with_args("[]").replace(
            "{ at = \"07:00\" }",
            "{ at = \"07:00\", days = [\"mon\", \"mon\", \"mon\", \"mon\", \"mon\", \"mon\", \"mon\"] }",
        );
        let mut form = Form::edit(text.clone(), "a").unwrap();
        assert_eq!(form.days, [true, false, false, false, false, false, false]);
        form.focus = Focus::At;
        form.input(Edit::Backspace);
        typed(&mut form, "5");
        assert_eq!(form.result().unwrap(), text.replace("07:00", "07:05"));
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

    #[test]
    fn a_new_form_tells_of_failures() {
        let form = Form::create(String::new());
        assert_eq!(form.notify, Level::Failures);
        assert_eq!(form.spec().notify, Level::Failures);
        assert!(!form.dirty());
    }

    #[test]
    fn the_level_comes_last_in_the_order_of_the_fields() {
        let mut form = Form::create(String::new());
        form.prev_field();
        assert_eq!(form.focus, Focus::Notify);
        form.next_field();
        assert_eq!(form.focus, Focus::Name);
        form.focus = Focus::Args;
        form.next_field();
        assert_eq!(form.focus, Focus::Notify);
    }

    #[test]
    fn the_level_is_chosen_not_typed() {
        let mut form = Form::create(String::new());
        form.focus = Focus::Notify;
        let mut seen = Vec::new();
        for _ in 0..3 {
            form.input(Edit::Right);
            seen.push(form.notify);
        }
        // It stops at each end.
        assert_eq!(seen, [Level::Finish, Level::All, Level::All]);
        seen.clear();
        for _ in 0..4 {
            form.input(Edit::Left);
            seen.push(form.notify);
        }
        assert_eq!(
            seen,
            [Level::Finish, Level::Failures, Level::Off, Level::Off]
        );
        typed(&mut form, "all");
        form.input(Edit::Backspace);
        assert_eq!(form.notify, Level::Off);
        // The space bar goes on to the next, and round.
        let mut marked = Vec::new();
        for _ in 0..4 {
            form.toggle();
            marked.push(form.notify);
        }
        assert_eq!(
            marked,
            [Level::Failures, Level::Finish, Level::All, Level::Off]
        );
    }

    #[test]
    fn changing_only_the_level_makes_the_form_dirty_and_writes_one_key() {
        let mut form = Form::edit(SAMPLE.to_owned(), "morning-triage").unwrap();
        assert_eq!(form.notify, Level::Failures);
        assert!(!form.dirty());
        form.focus = Focus::Notify;
        form.input(Edit::Right);
        assert!(form.dirty());
        assert_eq!(
            form.result().unwrap(),
            format!("{SAMPLE}notify = \"finish\"\n")
        );
    }

    #[test]
    fn editing_another_field_of_a_job_without_the_key_adds_none() {
        let mut form = Form::edit(SAMPLE.to_owned(), "morning-triage").unwrap();
        form.focus = Focus::At;
        form.input(Edit::Backspace);
        form.input(Edit::Insert('5'));
        let text = form.result().unwrap();
        assert_eq!(text, SAMPLE.replace("07:00", "07:05"));
    }

    #[test]
    fn a_form_opens_with_the_level_the_file_has() {
        let form = Form::edit(SAMPLE.to_owned(), "linear-updates").unwrap();
        assert_eq!(form.notify, Level::Finish);
        assert_eq!(form.result().unwrap(), SAMPLE);
    }
}
