use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::runner::{Runner, must};
use super::{Context, PREFIX, Scheduler, Unit, file_names, remove_unit, write_units};
use crate::config::{Job, home_dir, is_job_name};

/// Linux: a user service plus a timer per job.
pub struct Systemd {
    units_dir: PathBuf,
}

impl Systemd {
    pub fn for_user() -> Result<Systemd> {
        let xdg_config = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        Ok(Systemd {
            units_dir: Systemd::dir_from(xdg_config.as_deref(), home_dir().as_deref())?,
        })
    }

    /// Where systemd looks for the user's own units: `<xdg_config>/systemd/user`,
    /// falling back to `<home>/.config/systemd/user`.
    fn dir_from(xdg_config: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
        let config = match (xdg_config, home) {
            (Some(dir), _) => dir.to_path_buf(),
            (None, Some(home)) => home.join(".config"),
            (None, None) => bail!("cannot find the home directory"),
        };
        Ok(config.join("systemd").join("user"))
    }
}

impl Systemd {
    fn timer(job_name: &str) -> String {
        format!("{PREFIX}{job_name}.timer")
    }

    fn service(job_name: &str) -> String {
        format!("{PREFIX}{job_name}.service")
    }
}

impl Scheduler for Systemd {
    fn installed(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = file_names(&self.units_dir)?
            .iter()
            .filter_map(|name| {
                let rest = name.strip_prefix(PREFIX)?;
                rest.strip_suffix(".service")
                    .or_else(|| rest.strip_suffix(".timer"))
            })
            .filter(|job| is_job_name(job))
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn load(&self, job_name: &str, units: &[Unit], runner: &dyn Runner) -> Result<()> {
        write_units(&self.units_dir, units)?;
        must(runner, "systemctl", &["--user", "daemon-reload"])?;
        must(
            runner,
            "systemctl",
            &["--user", "enable", "--now", &Systemd::timer(job_name)],
        )
    }

    fn unload(&self, job_name: &str, runner: &dyn Runner) -> Result<()> {
        let timer = Systemd::timer(job_name);
        // `disable` fails on a timer systemd has no file for.
        if self.units_dir.join(&timer).exists() {
            must(runner, "systemctl", &["--user", "disable", "--now", &timer])?;
        }
        remove_unit(&self.units_dir.join(&timer))?;
        remove_unit(&self.units_dir.join(Systemd::service(job_name)))?;
        must(runner, "systemctl", &["--user", "daemon-reload"])
    }

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
            "[Unit]\nDescription=otto job {job_name}\n\n[Service]\nType=oneshot\nEnvironment=\"PATH={path}\"\nExecStart=\"{otto}\" --config \"{config}\" run {job_name} --scheduled\nStandardOutput=append:{log}\nStandardError=append:{log}\n",
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
                path: self.units_dir.join(Systemd::service(job_name)),
                contents: service,
            },
            Unit {
                path: self.units_dir.join(Systemd::timer(job_name)),
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
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::config::Config;
    use crate::scheduler::runner::Recorder;

    fn report_units(dir: &Path) -> Vec<Unit> {
        vec![
            Unit {
                path: dir.join("io.github.tarcisiopgs.otto.report.service"),
                contents: "[Service]\n".to_owned(),
            },
            Unit {
                path: dir.join("io.github.tarcisiopgs.otto.report.timer"),
                contents: "[Timer]\n".to_owned(),
            },
        ]
    }

    #[test]
    fn installed_lists_each_otto_job_once() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "io.github.tarcisiopgs.otto.report.service",
            "io.github.tarcisiopgs.otto.report.timer",
            "io.github.tarcisiopgs.otto.a-b.timer",
            "other.service",
            "otto-backup.timer",
            "io.github.tarcisiopgs.otto..timer",
            "io.github.tarcisiopgs.otto.Bad.service",
        ] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let systemd = Systemd {
            units_dir: dir.path().to_path_buf(),
        };
        assert_eq!(systemd.installed().unwrap(), ["a-b", "report"]);
    }

    #[test]
    fn units_live_where_systemd_looks_for_user_units() {
        assert_eq!(
            Systemd::dir_from(Some(Path::new("/xdg")), Some(Path::new("/home/me"))).unwrap(),
            Path::new("/xdg/systemd/user")
        );
        assert_eq!(
            Systemd::dir_from(None, Some(Path::new("/home/me"))).unwrap(),
            Path::new("/home/me/.config/systemd/user")
        );
        assert!(Systemd::dir_from(None, None).is_err());
    }

    #[test]
    fn installed_is_empty_without_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let systemd = Systemd {
            units_dir: dir.path().join("missing"),
        };
        assert!(systemd.installed().unwrap().is_empty());
    }

    #[test]
    fn load_writes_both_units_and_enables_the_timer() {
        let dir = tempfile::tempdir().unwrap();
        let units_dir = dir.path().join("systemd").join("user");
        let systemd = Systemd {
            units_dir: units_dir.clone(),
        };
        let units = report_units(&units_dir);
        let runner = Recorder::new();

        systemd.load("report", &units, &runner).unwrap();

        assert!(units.iter().all(|unit| unit.path.exists()));
        assert_eq!(
            runner.calls(),
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now io.github.tarcisiopgs.otto.report.timer"
            ]
        );
    }

    #[test]
    fn load_reports_a_failed_enable() {
        let dir = tempfile::tempdir().unwrap();
        let systemd = Systemd {
            units_dir: dir.path().to_path_buf(),
        };
        let runner = Recorder::new().answering("systemctl --user enable", &[false]);
        assert!(
            systemd
                .load("report", &report_units(dir.path()), &runner)
                .is_err()
        );
    }

    #[test]
    fn unload_disables_the_timer_and_removes_both_units() {
        let dir = tempfile::tempdir().unwrap();
        let systemd = Systemd {
            units_dir: dir.path().to_path_buf(),
        };
        let units = report_units(dir.path());
        for unit in &units {
            fs::write(&unit.path, &unit.contents).unwrap();
        }
        let runner = Recorder::new();

        systemd.unload("report", &runner).unwrap();

        assert_eq!(
            runner.calls(),
            [
                "systemctl --user disable --now io.github.tarcisiopgs.otto.report.timer",
                "systemctl --user daemon-reload"
            ]
        );
        assert!(units.iter().all(|unit| !unit.path.exists()));
    }

    #[test]
    fn unload_skips_disable_when_only_the_service_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let systemd = Systemd {
            units_dir: dir.path().to_path_buf(),
        };
        let service_path = dir.path().join("io.github.tarcisiopgs.otto.report.service");
        fs::write(&service_path, "[Service]\n").unwrap();
        let runner = Recorder::new();

        systemd.unload("report", &runner).unwrap();

        assert_eq!(runner.calls(), ["systemctl --user daemon-reload"]);
        assert!(!service_path.exists());
    }

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
            "ExecStart=\"/usr/bin/otto\" --config \"/home/me/my config/jobs.toml\" run report --scheduled\n"
        ));
        assert!(service.contains("Environment=\"PATH=/home/me/bin:/opt/50%%/bin\"\n"));
        assert!(
            service.contains("StandardOutput=append:/home/me/.local/state/otto/logs/report.log\n")
        );
        assert!(
            service.contains("StandardError=append:/home/me/.local/state/otto/logs/report.log\n")
        );
        assert!(
            units[1]
                .path
                .ends_with("io.github.tarcisiopgs.otto.report.timer")
        );
        assert!(
            units[1]
                .contents
                .contains("OnCalendar=Mon,Fri *-*-* 07:00:00")
        );
    }
}
