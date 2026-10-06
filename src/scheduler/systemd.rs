use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::{Context, Scheduler, Unit};
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

    fn units(&self, job_name: &str, job: &Job, ctx: &Context) -> Result<Vec<Unit>> {
        let (hour, minute) = job.schedule.time()?;
        let days: Vec<&str> = job
            .schedule
            .days
            .iter()
            .map(|day| day.systemd_name())
            .collect();
        let log = specifiers(&ctx.log_file(job_name).to_string_lossy());
        let service = format!(
            "[Unit]\nDescription=otto job {job_name}\n\n[Service]\nType=oneshot\nEnvironment=\"PATH={path}\"\nExecStart=\"{otto}\" --config \"{config}\" run {job_name}\nStandardOutput=append:{log}\nStandardError=append:{log}\n",
            path = quoted(&ctx.path),
            otto = quoted(&ctx.otto.to_string_lossy()),
            config = quoted(&ctx.config.to_string_lossy()),
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

/// systemd expands `%` specifiers in unit files; a literal one is doubled.
fn specifiers(text: &str) -> String {
    text.replace('%', "%%")
}

/// A value placed between double quotes in a unit file.
fn quoted(text: &str) -> String {
    specifiers(text).replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::Config;

    fn units_with_path(path: &str) -> Vec<Unit> {
        let config = Config::parse(
            "[jobs.report]\nagent = \"codex\"\nprompt = \"/p.md\"\nworkdir = \"/w\"\nschedule = { at = \"07:00\", days = [\"mon\", \"fri\"] }\n",
            Path::new("/"),
            None,
        )
        .unwrap();
        let systemd = Systemd {
            units_dir: PathBuf::from("/home/me/.config/systemd/user"),
        };
        let ctx = Context {
            otto: PathBuf::from("/usr/bin/otto"),
            config: PathBuf::from("/home/me/my config/jobs.toml"),
            path: path.to_owned(),
            log_dir: PathBuf::from("/home/me/.local/state/otto/logs"),
        };
        systemd
            .units("report", config.job("report").unwrap(), &ctx)
            .unwrap()
    }

    #[test]
    fn quotes_and_backslashes_survive_in_quoted_values() {
        let units = units_with_path(r#"/a"b:/c\d"#);
        assert!(
            units[0]
                .contents
                .contains(r#"Environment="PATH=/a\"b:/c\\d""#)
        );
    }

    #[test]
    fn the_timer_carries_days_and_time() {
        let units = units_with_path("/home/me/bin:/opt/50%/bin");

        assert_eq!(units.len(), 2);
        let service = &units[0].contents;
        assert!(service.contains(
            "ExecStart=\"/usr/bin/otto\" --config \"/home/me/my config/jobs.toml\" run report\n"
        ));
        assert!(service.contains("Environment=\"PATH=/home/me/bin:/opt/50%%/bin\"\n"));
        assert!(
            service.contains("StandardOutput=append:/home/me/.local/state/otto/logs/report.log\n")
        );
        assert!(
            service.contains("StandardError=append:/home/me/.local/state/otto/logs/report.log\n")
        );
        assert!(units[1].path.ends_with("otto-report.timer"));
        assert!(
            units[1]
                .contents
                .contains("OnCalendar=Mon,Fri *-*-* 07:00:00")
        );
    }
}
