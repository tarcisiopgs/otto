//! Telling the user about a run: which runs a job tells of, and in what
//! words.

use jiff::Zoned;
use serde::Deserialize;

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
}
