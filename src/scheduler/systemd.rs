use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{Scheduler, Unit};
use crate::config::{Job, home_dir};

/// Linux: a user service plus a timer per job.
pub struct Systemd {
    units_dir: PathBuf,
}

impl Systemd {
    pub fn for_user() -> Result<Systemd> {
        let home = home_dir().context("cannot find the home directory")?;
        Ok(Systemd {
            units_dir: home.join(".config").join("systemd").join("user"),
        })
    }
}

impl Scheduler for Systemd {
    fn name(&self) -> &'static str {
        "systemd"
    }

    fn units(&self, job_name: &str, job: &Job, otto: &Path) -> Result<Vec<Unit>> {
        let (hour, minute) = job.schedule.time()?;
        let days: Vec<&str> = job
            .schedule
            .days
            .iter()
            .map(|day| day.systemd_name())
            .collect();
        let service = format!(
            "[Unit]\nDescription=otto job {job_name}\n\n[Service]\nType=oneshot\nExecStart=\"{}\" run {job_name}\n",
            otto.display()
        );
        // Persistent=true runs a missed activation at the next boot or login.
        let timer = format!(
            "[Unit]\nDescription=Schedule for otto job {job_name}\n\n[Timer]\nOnCalendar={} *-*-* {hour:02}:{minute:02}:00\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
            days.join(",")
        );
        Ok(vec![
            Unit {
                path: self.units_dir.join(format!("otto-{job_name}.service")),
                contents: service,
            },
            Unit {
                path: self.units_dir.join(format!("otto-{job_name}.timer")),
                contents: timer,
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn the_timer_carries_days_and_time() {
        let config = Config::parse(
            "[jobs.report]\nagent = \"codex\"\nprompt = \"/p.md\"\nworkdir = \"/w\"\nschedule = { at = \"07:00\", days = [\"mon\", \"fri\"] }\n",
            Path::new("/"),
            None,
        )
        .unwrap();
        let systemd = Systemd {
            units_dir: PathBuf::from("/home/me/.config/systemd/user"),
        };
        let units = systemd
            .units(
                "report",
                config.job("report").unwrap(),
                Path::new("/usr/bin/otto"),
            )
            .unwrap();

        assert_eq!(units.len(), 2);
        assert!(
            units[0]
                .contents
                .contains("ExecStart=\"/usr/bin/otto\" run report")
        );
        assert!(units[1].path.ends_with("otto-report.timer"));
        assert!(
            units[1]
                .contents
                .contains("OnCalendar=Mon,Fri *-*-* 07:00:00")
        );
    }
}
