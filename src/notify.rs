//! Telling the user about a run: which runs a job tells of, and in what
//! words.

use anyhow::{Context as _, Result, bail};
use jiff::Zoned;
use serde::Deserialize;

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::scheduler::runner::{Output, Runner};
use crate::store::{Trigger, duration_label};

/// How much a job tells of its runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Never.
    Off,
    /// When a run fails.
    #[default]
    Failures,
    /// When a run ends, well or not.
    Finish,
    /// Also when a run starts and when it does not happen.
    All,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Off, Level::Failures, Level::Finish, Level::All];

    pub fn label(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Failures => "failures",
            Level::Finish => "finish",
            Level::All => "all",
        }
    }

    /// Whether a job of this level tells of `event`.
    pub fn tells(self, event: &Event) -> bool {
        match event {
            Event::Failed { .. } => self != Level::Off,
            Event::Ok { .. } => matches!(self, Level::Finish | Level::All),
            Event::Started { .. } | Event::Skipped { .. } | Event::Paused => self == Level::All,
        }
    }
}

/// Something about a run that a job may tell of.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// The agent is about to start.
    Started {
        trigger: Trigger,
        at: &'a Zoned,
    },
    Ok {
        seconds: i64,
    },
    /// `code` is what the agent exited with; `reason` is why it never
    /// started.
    Failed {
        code: Option<i32>,
        seconds: i64,
        reason: Option<&'a str>,
    },
    /// A scheduled run that did not happen, and why.
    Skipped {
        reason: &'a str,
    },
    /// A scheduled run of a paused job.
    Paused,
}

/// What a notification says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub body: String,
}

/// The longest body a notification is given: the systems cut a long one
/// where they please, and a reason ends in what it is about.
const BODY: usize = 200;

/// `text` on one line, and when it is too long its end.
fn one_line(text: &str) -> String {
    let said = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = said.chars().count();
    if length <= BODY {
        return said;
    }
    let kept: String = said.chars().skip(length - (BODY - 1)).collect();
    format!("…{kept}")
}

/// The words for `event` of `job`.
pub fn message(job: &str, event: &Event) -> Message {
    let (what, body) = match event {
        Event::Started { trigger, at } => (
            "started",
            format!("{}, {}", trigger.label(), at.strftime("%H:%M")),
        ),
        Event::Ok { seconds } => ("ok", duration_label(*seconds)),
        Event::Failed {
            reason: Some(reason),
            ..
        } => ("failed", one_line(reason)),
        Event::Failed {
            code: Some(code),
            seconds,
            ..
        } => (
            "failed",
            format!("exit {code} after {}", duration_label(*seconds)),
        ),
        Event::Failed { seconds, .. } => ("failed", format!("after {}", duration_label(*seconds))),
        Event::Skipped { reason } => ("skipped", one_line(reason)),
        Event::Paused => ("paused", "the job is paused".to_owned()),
    };
    Message {
        title: format!("{job} {what}"),
        body,
    }
}

/// Runs the command of a notification. One that fails is told by what it
/// said, not by the command, which is long and nobody's to read.
fn ask(runner: &dyn Runner, program: &str, args: &[&str]) -> Result<()> {
    let output = runner.run(program, args)?;
    if !output.success {
        bail!("{program} failed: {}", output.stderr.trim());
    }
    Ok(())
}

/// Shows a message to the user.
pub trait Notifier {
    fn notify(&self, message: &Message) -> Result<()>;
}

/// The Notification Center, through `osascript`.
pub struct Macos<'a> {
    pub runner: &'a dyn Runner,
}

/// The desktop's notification service, through `notify-send`.
pub struct Linux<'a> {
    pub runner: &'a dyn Runner,
}

/// A toast, through PowerShell.
pub struct Windows<'a> {
    pub runner: &'a dyn Runner,
}

impl Notifier for Macos<'_> {
    fn notify(&self, message: &Message) -> Result<()> {
        // The words are arguments of the script, never part of its text:
        // nothing in a job name or a reason is read as AppleScript.
        ask(
            self.runner,
            "osascript",
            &[
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
                "--",
                &message.title,
                &message.body,
            ],
        )
    }
}

impl Notifier for Linux<'_> {
    fn notify(&self, message: &Message) -> Result<()> {
        // After `--` nothing is an option, whatever it starts with.
        ask(
            self.runner,
            "notify-send",
            &["--app-name=otto", "--", &message.title, &message.body],
        )
    }
}

/// The toast, with the words still to put in. It is shown in the name of
/// PowerShell: a toast needs an application to belong to, and otto is none.
const TOAST: &str = r"[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$text = $xml.GetElementsByTagName('text')
$text.Item(0).AppendChild($xml.CreateTextNode({title})) | Out-Null
$text.Item(1).AppendChild($xml.CreateTextNode({body})) | Out-Null
$toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe').Show($toast)";

/// `text` as a PowerShell string that nothing is expanded in. PowerShell
/// ends such a string at a curly quote as it does at a straight one, so each
/// of them is doubled.
fn quoted(text: &str) -> String {
    let mut out = String::from("'");
    for c in text.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}'..='\u{201b}') {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

impl Notifier for Windows<'_> {
    fn notify(&self, message: &Message) -> Result<()> {
        let script = TOAST
            .replace("{title}", &quoted(&message.title))
            .replace("{body}", &quoted(&message.body));
        // Encoded, so that no quoting stands between otto and PowerShell.
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        ask(
            self.runner,
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-OutputFormat",
                "Text",
                "-EncodedCommand",
                &base64(&bytes),
            ],
        )
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |group, (index, byte)| {
                group | u32::from(*byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                let six = (group >> (18 - 6 * index)) & 0x3f;
                text.push(char::from(ALPHABET[six as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// How long otto waits for a notification command.
pub const PATIENCE: Duration = Duration::from_secs(5);

/// Runs a notification command and gives up on one that does not return: a
/// dialog nobody answers, a desktop that is not there. No notification is
/// worth a run that waits on it.
pub struct Timed {
    pub limit: Duration,
}

impl Runner for Timed {
    fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot start {program}"))?;
        let asked = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if asked.elapsed() > self.limit {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "{program} took longer than {}s",
                    self.limit.as_secs_f32().ceil()
                );
            }
            thread::sleep(Duration::from_millis(20));
        };
        let mut stderr = String::new();
        if let Some(mut said) = child.stderr.take() {
            let _ = said.read_to_string(&mut stderr);
        }
        Ok(Output {
            success: status.success(),
            stdout: String::new(),
            stderr,
        })
    }
}

/// A system otto has no way to notify on.
struct Nowhere;

impl Notifier for Nowhere {
    fn notify(&self, _message: &Message) -> Result<()> {
        bail!("otto cannot show a notification on this system")
    }
}

/// The notifier of this system.
pub fn native(runner: &dyn Runner) -> Box<dyn Notifier + '_> {
    if cfg!(target_os = "macos") {
        Box::new(Macos { runner })
    } else if cfg!(target_os = "linux") {
        Box::new(Linux { runner })
    } else if cfg!(windows) {
        Box::new(Windows { runner })
    } else {
        Box::new(Nowhere)
    }
}

/// Keeps what it is asked to show, and can be made to fail.
#[cfg(test)]
#[cfg(unix)]
#[derive(Default)]
pub struct Recording {
    pub told: std::cell::RefCell<Vec<Message>>,
    pub broken: Option<String>,
}

#[cfg(test)]
#[cfg(unix)]
impl Notifier for Recording {
    fn notify(&self, message: &Message) -> Result<()> {
        if let Some(reason) = &self.broken {
            bail!("{reason}");
        }
        self.told.borrow_mut().push(message.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> Zoned {
        "2026-10-07T07:00:00-03:00[America/Recife]".parse().unwrap()
    }

    #[test]
    fn each_level_tells_what_the_spec_says() {
        let at = at();
        let events = [
            Event::Started {
                trigger: Trigger::Scheduled,
                at: &at,
            },
            Event::Ok { seconds: 1 },
            Event::Failed {
                code: Some(1),
                seconds: 1,
                reason: None,
            },
            Event::Skipped {
                reason: "skip next",
            },
            Event::Paused,
        ];
        // started, ok, failed, skipped, paused
        let table = [
            (Level::Off, [false, false, false, false, false]),
            (Level::Failures, [false, false, true, false, false]),
            (Level::Finish, [false, true, true, false, false]),
            (Level::All, [true, true, true, true, true]),
        ];
        for (level, expected) in table {
            let tells = events.map(|event| level.tells(&event));
            assert_eq!(tells, expected, "{level:?}");
        }
    }

    #[test]
    fn the_default_level_is_failures() {
        assert_eq!(Level::default(), Level::Failures);
        assert_eq!(
            Level::ALL.map(Level::label),
            ["off", "failures", "finish", "all"]
        );
    }

    fn said(event: Event) -> Message {
        message("slack-bom-dia", &event)
    }

    #[test]
    fn every_event_has_its_words() {
        let at = at();
        assert_eq!(
            said(Event::Failed {
                code: Some(3),
                seconds: 206,
                reason: None
            }),
            Message {
                title: "slack-bom-dia failed".to_owned(),
                body: "exit 3 after 3m 26s".to_owned()
            }
        );
        assert_eq!(
            said(Event::Ok { seconds: 206 }),
            Message {
                title: "slack-bom-dia ok".to_owned(),
                body: "3m 26s".to_owned()
            }
        );
        let started = said(Event::Started {
            trigger: Trigger::Scheduled,
            at: &at,
        });
        assert_eq!(started.title, "slack-bom-dia started");
        assert_eq!(started.body, "scheduled, 07:00");
        let by_hand = said(Event::Started {
            trigger: Trigger::Manual,
            at: &at,
        });
        assert_eq!(by_hand.body, "manual, 07:00");
        assert_eq!(
            said(Event::Skipped {
                reason: "already running"
            }),
            Message {
                title: "slack-bom-dia skipped".to_owned(),
                body: "already running".to_owned()
            }
        );
        assert_eq!(
            said(Event::Paused),
            Message {
                title: "slack-bom-dia paused".to_owned(),
                body: "the job is paused".to_owned()
            }
        );
    }

    #[test]
    fn a_run_that_never_started_says_why() {
        let event = Event::Failed {
            code: None,
            seconds: 0,
            reason: Some("working directory not found: /work/app"),
        };
        assert_eq!(said(event).body, "working directory not found: /work/app");
    }

    #[test]
    fn a_run_killed_by_a_signal_has_no_exit_code_to_show() {
        let event = Event::Failed {
            code: None,
            seconds: 206,
            reason: None,
        };
        assert_eq!(said(event).body, "after 3m 26s");
    }

    #[test]
    fn a_long_reason_keeps_its_end() {
        let reason = format!("cannot read prompt /{}/report.md", "a".repeat(300));
        let body = said(Event::Failed {
            code: None,
            seconds: 0,
            reason: Some(&reason),
        })
        .body;
        assert_eq!(body.chars().count(), 200);
        assert!(body.starts_with('…'), "{body}");
        assert!(body.ends_with("/report.md"), "{body}");
    }

    #[test]
    fn a_reason_of_many_lines_reads_as_one() {
        let event = Event::Failed {
            code: None,
            seconds: 0,
            reason: Some("cannot start claude\n  no such file"),
        };
        assert_eq!(said(event).body, "cannot start claude no such file");
    }

    /// Keeps each command whole: the program and every argument.
    #[derive(Default)]
    struct Commands {
        run: std::cell::RefCell<Vec<Vec<String>>>,
        failing: bool,
    }

    impl Runner for Commands {
        fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
            let mut command = vec![program.to_owned()];
            command.extend(args.iter().map(|arg| (*arg).to_owned()));
            self.run.borrow_mut().push(command);
            Ok(Output {
                success: !self.failing,
                stdout: String::new(),
                stderr: "not allowed".to_owned(),
            })
        }
    }

    fn awkward() -> Message {
        Message {
            title: "-job \"it's\" failed".to_owned(),
            body: "exit 3, `x` \\ $HOME\nsecond ’quoted‘".to_owned(),
        }
    }

    fn only(commands: &Commands) -> Vec<String> {
        let run = commands.run.borrow();
        assert_eq!(run.len(), 1);
        run[0].clone()
    }

    #[test]
    fn macos_passes_the_words_as_arguments_of_the_script() {
        let commands = Commands::default();
        Macos { runner: &commands }.notify(&awkward()).unwrap();
        let command = only(&commands);
        assert_eq!(command[0], "osascript");
        // After `--`, so a title is never read as an option of osascript.
        assert_eq!(command[command.len() - 3], "--");
        assert_eq!(command[command.len() - 2], awkward().title);
        assert_eq!(command[command.len() - 1], awkward().body);
        // Nothing of the message is in the text of the script.
        let script = command[1..command.len() - 2].join(" ");
        assert!(!script.contains("it's"), "{script}");
        assert!(
            script.contains("display notification (item 2 of argv) with title (item 1 of argv)")
        );
    }

    #[test]
    fn linux_keeps_a_title_from_being_read_as_an_option() {
        let commands = Commands::default();
        Linux { runner: &commands }.notify(&awkward()).unwrap();
        assert_eq!(
            only(&commands),
            [
                "notify-send",
                "--app-name=otto",
                "--",
                &awkward().title,
                &awkward().body
            ]
        );
    }

    /// What a base64 text holds, for a test to read back.
    fn decoded(text: &str) -> Vec<u8> {
        const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let sixes: Vec<u32> = text
            .chars()
            .filter(|c| *c != '=')
            .map(|c| ALPHABET.find(c).unwrap() as u32)
            .collect();
        let mut bytes = Vec::new();
        for group in sixes.chunks(4) {
            let all = group
                .iter()
                .enumerate()
                .fold(0_u32, |all, (index, six)| all | six << (18 - 6 * index));
            for index in 0..group.len() - 1 {
                bytes.push((all >> (16 - 8 * index)) as u8);
            }
        }
        bytes
    }

    #[test]
    fn windows_quotes_the_words_inside_the_script() {
        let commands = Commands::default();
        Windows { runner: &commands }.notify(&awkward()).unwrap();
        let command = only(&commands);
        assert_eq!(
            command[..6],
            [
                "powershell",
                "-NoProfile",
                "-NonInteractive",
                "-OutputFormat",
                "Text",
                "-EncodedCommand"
            ]
        );
        let units: Vec<u16> = decoded(&command[6])
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(
            script.contains("CreateTextNode('-job \"it''s\" failed')"),
            "{script}"
        );
        assert!(
            // A curly quote ends a PowerShell string as a straight one does.
            script.contains("CreateTextNode('exit 3, `x` \\ $HOME\nsecond ’’quoted‘‘')"),
            "{script}"
        );
        assert!(script.ends_with(".Show($toast)"), "{script}");
        assert!(!script.contains("{title}") && !script.contains("{body}"));
    }

    #[test]
    fn base64_matches_the_known_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in vectors {
            assert_eq!(base64(plain.as_bytes()), encoded);
            assert_eq!(decoded(encoded), plain.as_bytes());
        }
    }

    #[test]
    fn a_command_that_fails_is_an_error_with_what_it_said() {
        let commands = Commands {
            failing: true,
            ..Commands::default()
        };
        let error = Macos { runner: &commands }.notify(&awkward()).unwrap_err();
        let said = format!("{error:#}");
        assert!(said.contains("osascript"), "{said}");
        assert!(said.contains("not allowed"), "{said}");
        // The command is not the user's to read: only what it said.
        assert!(!said.contains("display notification"), "{said}");
        assert!(!said.contains("it's"), "{said}");
    }
}

// These tests start `sh` in the place of the notification command.
#[cfg(test)]
#[cfg(unix)]
mod unix_tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_command_that_does_not_return_is_given_up_on() {
        let timed = Timed {
            limit: Duration::from_millis(200),
        };
        let asked = Instant::now();
        let error = timed.run("sh", &["-c", "sleep 5"]).unwrap_err();
        assert!(asked.elapsed() < Duration::from_secs(3));
        assert!(
            format!("{error:#}").contains("sh took longer than"),
            "{error:#}"
        );
    }

    #[test]
    fn a_command_that_returns_says_how_it_went() {
        let timed = Timed { limit: PATIENCE };
        let output = timed.run("sh", &["-c", "echo no >&2; exit 3"]).unwrap();
        assert!(!output.success);
        assert_eq!(output.stderr.trim(), "no");
        assert!(timed.run("sh", &["-c", "true"]).unwrap().success);
    }

    #[test]
    fn a_command_that_is_not_there_is_named() {
        let timed = Timed { limit: PATIENCE };
        let error = timed.run("otto-no-such-notifier", &[]).unwrap_err();
        assert!(
            format!("{error:#}").contains("cannot start otto-no-such-notifier"),
            "{error:#}"
        );
        // And through a backend it is still only an error.
        let error = Linux { runner: &timed }
            .notify(&Message {
                title: "report failed".to_owned(),
                body: "exit 3".to_owned(),
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("cannot start notify-send"));
    }
}
