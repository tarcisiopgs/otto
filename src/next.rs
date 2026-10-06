//! When a job runs next, worked out from its schedule. This is otto's own
//! estimate: nothing here asks the OS scheduler.

use anyhow::{Context as _, Result, bail};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

use crate::config::Schedule;
#[cfg(test)]
use crate::config::Weekday;
use crate::store::State;

/// What the schedule and the job's state say about the coming runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// A paused job starts no agent until it is resumed.
    Paused,
    At(Zoned),
    /// `skipped` fires without starting the agent; `then` is the run after it.
    Skipping {
        skipped: Zoned,
        then: Zoned,
    },
}

impl Next {
    /// The run that will start the agent.
    pub fn runs_at(&self) -> Option<&Zoned> {
        match self {
            Next::Paused => None,
            Next::At(when) => Some(when),
            Next::Skipping { then, .. } => Some(then),
        }
    }
}

/// The coming runs of a job, strictly after `now`, in `zone`.
pub fn next(schedule: &Schedule, state: State, now: Timestamp, zone: &TimeZone) -> Result<Next> {
    if state.paused {
        return Ok(Next::Paused);
    }
    let first = after(schedule, now, zone)?;
    if state.skip_next {
        let then = after(schedule, first.timestamp(), zone)?;
        return Ok(Next::Skipping {
            skipped: first,
            then,
        });
    }
    Ok(Next::At(first))
}

/// The first time the schedule fires after `now`. A time of day the clocks
/// skip moves forward, and one they repeat counts the first time, which is
/// how jiff reads an ambiguous local time; the OS scheduler may differ on
/// those two days of the year.
fn after(schedule: &Schedule, now: Timestamp, zone: &TimeZone) -> Result<Zoned> {
    let (hour, minute) = schedule.time()?;
    let today = now.to_zoned(zone.clone()).date();
    // Any day of the week comes around within seven days.
    for offset in 0..=7 {
        let date = today
            .checked_add(offset.days())
            .context("the schedule runs past the calendar")?;
        if !schedule
            .days
            .iter()
            .any(|day| day.civil() == date.weekday())
        {
            continue;
        }
        let when = date
            .at(hour.cast_signed(), minute.cast_signed(), 0, 0)
            .to_zoned(zone.clone())
            .context("cannot place the schedule in the local time zone")?;
        if when.timestamp() > now {
            return Ok(when);
        }
    }
    bail!("the schedule has no day to run on")
}

/// `today 16:05`, `tomorrow 07:00`, `Mon 16:05` within the week, a date after that.
pub fn label(when: &Zoned, now: &Zoned) -> String {
    let (day, today) = (when.date(), now.date());
    let format = if day == today {
        "today %H:%M"
    } else if today.tomorrow().is_ok_and(|tomorrow| tomorrow == day) {
        "tomorrow %H:%M"
    } else if day > today && day <= today.saturating_add(6.days()) {
        "%a %H:%M"
    } else {
        "%Y-%m-%d %H:%M"
    };
    when.strftime(format).to_string()
}

#[cfg(test)]
mod tests {
    use jiff::civil::DateTime;

    use super::*;

    fn zone(name: &str) -> TimeZone {
        TimeZone::get(name).unwrap()
    }

    fn sao_paulo() -> TimeZone {
        zone("America/Sao_Paulo")
    }

    /// Tuesday, 09:00 in São Paulo.
    fn tuesday_morning() -> Timestamp {
        "2026-10-06T12:00:00Z".parse().unwrap()
    }

    fn local(text: &str) -> Zoned {
        text.parse::<DateTime>()
            .unwrap()
            .to_zoned(sao_paulo())
            .unwrap()
    }

    fn schedule(at: &str, days: &[Weekday]) -> Schedule {
        Schedule {
            at: at.to_owned(),
            days: if days.is_empty() {
                Weekday::every_day()
            } else {
                days.to_vec()
            },
        }
    }

    const WEEKDAYS: [Weekday; 5] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
    ];

    fn active(at: &str, days: &[Weekday]) -> Next {
        next(
            &schedule(at, days),
            State::default(),
            tuesday_morning(),
            &sao_paulo(),
        )
        .unwrap()
    }

    #[test]
    fn a_weekday_job_runs_later_today() {
        assert_eq!(
            active("16:05", &WEEKDAYS),
            Next::At(local("2026-10-06T16:05"))
        );
    }

    #[test]
    fn a_time_already_past_goes_to_tomorrow() {
        assert_eq!(active("07:00", &[]), Next::At(local("2026-10-07T07:00")));
    }

    #[test]
    fn the_current_minute_is_not_the_next_run() {
        assert_eq!(active("09:00", &[]), Next::At(local("2026-10-07T09:00")));
    }

    #[test]
    fn a_single_day_waits_for_that_day() {
        assert_eq!(
            active("16:05", &[Weekday::Mon]),
            Next::At(local("2026-10-12T16:05"))
        );
    }

    #[test]
    fn a_paused_job_has_no_next_run() {
        let state = State {
            paused: true,
            skip_next: true,
        };
        let found = next(
            &schedule("16:05", &WEEKDAYS),
            state,
            tuesday_morning(),
            &sao_paulo(),
        )
        .unwrap();
        assert_eq!(found, Next::Paused);
        assert_eq!(found.runs_at(), None);
    }

    #[test]
    fn a_skip_shows_the_skipped_run_and_the_one_after() {
        let state = State {
            paused: false,
            skip_next: true,
        };
        let found = next(
            &schedule("16:05", &WEEKDAYS),
            state,
            tuesday_morning(),
            &sao_paulo(),
        )
        .unwrap();
        assert_eq!(
            found,
            Next::Skipping {
                skipped: local("2026-10-06T16:05"),
                then: local("2026-10-07T16:05"),
            }
        );
        assert_eq!(found.runs_at(), Some(&local("2026-10-07T16:05")));
    }

    #[test]
    fn a_time_that_does_not_exist_moves_forward() {
        // Clocks in New York jump from 02:00 to 03:00 on this day.
        let now: Timestamp = "2026-03-08T05:00:00Z".parse().unwrap();
        let found = next(
            &schedule("02:30", &[]),
            State::default(),
            now,
            &zone("America/New_York"),
        )
        .unwrap();
        let expected: Timestamp = "2026-03-08T07:30:00Z".parse().unwrap();
        assert_eq!(found.runs_at().map(Zoned::timestamp), Some(expected));
    }

    #[test]
    fn a_time_that_happens_twice_takes_the_first() {
        // Clocks in New York go back from 02:00 to 01:00 on this day.
        let now: Timestamp = "2026-11-01T04:00:00Z".parse().unwrap();
        let found = next(
            &schedule("01:30", &[]),
            State::default(),
            now,
            &zone("America/New_York"),
        )
        .unwrap();
        let expected: Timestamp = "2026-11-01T05:30:00Z".parse().unwrap();
        assert_eq!(found.runs_at().map(Zoned::timestamp), Some(expected));
    }

    #[test]
    fn labels_are_relative_within_a_week() {
        let now = local("2026-10-06T09:00");
        assert_eq!(label(&local("2026-10-06T16:05"), &now), "today 16:05");
        assert_eq!(label(&local("2026-10-07T07:00"), &now), "tomorrow 07:00");
        assert_eq!(label(&local("2026-10-12T16:05"), &now), "Mon 16:05");
        assert_eq!(label(&local("2026-10-20T16:05"), &now), "2026-10-20 16:05");
    }
}
