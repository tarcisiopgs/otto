use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use super::runner::{Runner, must};
use super::{Context, Scheduler, Unit, file_names, remove_unit, write_units};
use crate::config::{Job, home_dir, is_job_name};

/// macOS: one LaunchAgent per job, in the user's GUI session so the agent CLI
/// can reach the login keychain.
pub struct Launchd {
    agents_dir: PathBuf,
}

impl Launchd {
    pub fn for_user() -> Result<Launchd> {
        let home = home_dir().context("cannot find the home directory")?;
        Ok(Launchd {
            agents_dir: home.join("Library").join("LaunchAgents"),
        })
    }
}

/// Every LaunchAgent with this prefix belongs to otto.
const PREFIX: &str = "io.github.tarcisiopgs.otto.";

/// How long `unload` waits for launchd to finish tearing a job down.
const GONE_ATTEMPTS: u32 = 50;
const GONE_PAUSE: Duration = Duration::from_millis(100);

pub fn label(job_name: &str) -> String {
    format!("{PREFIX}{job_name}")
}

/// The launchd domain of the logged-in user, `gui/<uid>`.
fn domain(runner: &dyn Runner) -> Result<String> {
    let output = runner.run("id", &["-u"])?;
    let uid = output.stdout.trim();
    if !output.success || uid.is_empty() {
        bail!("cannot find the user id: {}", output.stderr.trim());
    }
    Ok(format!("gui/{uid}"))
}

impl Scheduler for Launchd {
    fn installed(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = file_names(&self.agents_dir)?
            .iter()
            .filter_map(|name| name.strip_prefix(PREFIX)?.strip_suffix(".plist"))
            .filter(|job| is_job_name(job))
            .map(str::to_owned)
            .collect();
        names.sort();
        Ok(names)
    }

    fn load(&self, _job_name: &str, units: &[Unit], runner: &dyn Runner) -> Result<()> {
        write_units(&self.agents_dir, units)?;
        let domain = domain(runner)?;
        for unit in units {
            must(
                runner,
                "launchctl",
                &["bootstrap", &domain, &unit.path.to_string_lossy()],
            )?;
        }
        Ok(())
    }

    fn unload(&self, job_name: &str, runner: &dyn Runner) -> Result<()> {
        let label = label(job_name);
        let target = format!("{}/{label}", domain(runner)?);
        let loaded = |runner: &dyn Runner| -> Result<bool> {
            Ok(runner.run("launchctl", &["print", &target])?.success)
        };
        // `bootout` fails on a job that is not loaded, so ask first.
        if loaded(runner)? {
            must(runner, "launchctl", &["bootout", &target])?;
            // launchd tears the job down in the background, and a `bootstrap`
            // that arrives before it finishes fails with an I/O error.
            let mut attempts = 0;
            while loaded(runner)? {
                attempts += 1;
                if attempts >= GONE_ATTEMPTS {
                    bail!("{label} is still loaded after bootout");
                }
                thread::sleep(GONE_PAUSE);
            }
        }
        remove_unit(&self.agents_dir.join(format!("{label}.plist")))
    }

    fn name(&self) -> &'static str {
        "launchd"
    }

    fn units(&self, job_name: &str, job: &Job, ctx: &Context) -> Result<Vec<Unit>> {
        let (hour, minute) = job.schedule.time()?;
        let label = label(job_name);
        let log = ctx.log_file(job_name);
        // launchd fires a missed StartCalendarInterval once when the Mac wakes up.
        let intervals: String = job
            .schedule
            .days
            .iter()
            .map(|day| {
                format!(
                    "\t\t<dict>\n\t\t\t<key>Weekday</key>\n\t\t\t<integer>{}</integer>\n\t\t\t<key>Hour</key>\n\t\t\t<integer>{hour}</integer>\n\t\t\t<key>Minute</key>\n\t\t\t<integer>{minute}</integer>\n\t\t</dict>\n",
                    day.launchd_number()
                )
            })
            .collect();
        let contents = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{otto}</string>
		<string>--config</string>
		<string>{config}</string>
		<string>run</string>
		<string>{job_name}</string>
	</array>
	<key>StartCalendarInterval</key>
	<array>
{intervals}	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>{path}</string>
	</dict>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
	<key>ProcessType</key>
	<string>Background</string>
</dict>
</plist>
"#,
            label = escape(&label),
            otto = escape(&ctx.otto.to_string_lossy()),
            config = escape(&ctx.config.to_string_lossy()),
            job_name = escape(job_name),
            path = escape(&ctx.path),
            log = escape(&log.to_string_lossy()),
        );
        Ok(vec![Unit {
            path: self.agents_dir.join(format!("{label}.plist")),
            contents,
        }])
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::config::Config;
    use crate::scheduler::runner::Recorder;

    const LABEL: &str = "io.github.tarcisiopgs.otto.report";

    fn report_units(dir: &Path) -> Vec<Unit> {
        vec![Unit {
            path: dir.join(format!("{LABEL}.plist")),
            contents: "<plist/>".to_owned(),
        }]
    }

    #[test]
    fn installed_lists_only_otto_jobs() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "io.github.tarcisiopgs.otto.report.plist",
            "io.github.tarcisiopgs.otto.a-b.plist",
            "com.apple.other.plist",
            "io.github.tarcisiopgs.otto..plist",
            "io.github.tarcisiopgs.otto.Bad.plist",
        ] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let launchd = Launchd {
            agents_dir: dir.path().to_path_buf(),
        };
        assert_eq!(launchd.installed().unwrap(), ["a-b", "report"]);
    }

    #[test]
    fn installed_is_empty_without_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let launchd = Launchd {
            agents_dir: dir.path().join("missing"),
        };
        assert!(launchd.installed().unwrap().is_empty());
    }

    #[test]
    fn load_writes_the_plist_and_bootstraps_it() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join("nested").join("LaunchAgents");
        let launchd = Launchd {
            agents_dir: agents_dir.clone(),
        };
        let units = report_units(&agents_dir);
        let runner = Recorder::new();

        launchd.load("report", &units, &runner).unwrap();

        assert_eq!(
            fs::read_to_string(&units[0].path).unwrap(),
            units[0].contents
        );
        assert_eq!(
            runner.calls(),
            [
                "id -u".to_owned(),
                format!("launchctl bootstrap gui/501 {}", units[0].path.display())
            ]
        );
    }

    #[test]
    fn load_reports_a_failed_bootstrap() {
        let dir = tempfile::tempdir().unwrap();
        let launchd = Launchd {
            agents_dir: dir.path().to_path_buf(),
        };
        let runner = Recorder::new().answering("launchctl bootstrap", &[false]);
        assert!(
            launchd
                .load("report", &report_units(dir.path()), &runner)
                .is_err()
        );
    }

    #[test]
    fn unload_waits_until_the_service_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let launchd = Launchd {
            agents_dir: dir.path().to_path_buf(),
        };
        let plist_path = report_units(dir.path()).remove(0).path;
        fs::write(&plist_path, "<plist/>").unwrap();
        let runner = Recorder::new().answering("launchctl print", &[true, true, false]);

        launchd.unload("report", &runner).unwrap();

        assert_eq!(
            runner.calls(),
            [
                "id -u".to_owned(),
                format!("launchctl print gui/501/{LABEL}"),
                format!("launchctl bootout gui/501/{LABEL}"),
                format!("launchctl print gui/501/{LABEL}"),
                format!("launchctl print gui/501/{LABEL}"),
            ]
        );
        assert!(!plist_path.exists());
    }

    #[test]
    fn unload_removes_the_plist_of_a_job_that_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let launchd = Launchd {
            agents_dir: dir.path().to_path_buf(),
        };
        let plist_path = report_units(dir.path()).remove(0).path;
        fs::write(&plist_path, "<plist/>").unwrap();
        let runner = Recorder::new().answering("launchctl print", &[false]);

        launchd.unload("report", &runner).unwrap();

        assert!(!runner.calls().iter().any(|call| call.contains("bootout")));
        assert!(!plist_path.exists());
    }

    #[test]
    fn one_interval_per_day_and_otto_as_the_program() {
        let config = Config::parse(
            "[jobs.report]\nagent = \"claude\"\nprompt = \"/p.md\"\nworkdir = \"/w\"\nschedule = { at = \"16:05\", days = [\"mon\", \"sun\"] }\n",
            Path::new("/"),
            None,
        )
        .unwrap();
        let launchd = Launchd {
            agents_dir: PathBuf::from("/Users/me/Library/LaunchAgents"),
        };
        let ctx = Context {
            otto: PathBuf::from("/opt/R&D/otto"),
            config: PathBuf::from("/Users/me/.config/otto/jobs.toml"),
            path: "/Users/me/.local/bin:/opt/a<b/bin".to_owned(),
            log_dir: PathBuf::from("/Users/me/.local/state/otto/logs"),
        };
        let units = launchd
            .units("report", config.job("report").unwrap(), &ctx)
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].path,
            Path::new("/Users/me/Library/LaunchAgents/io.github.tarcisiopgs.otto.report.plist")
        );
        let plist = &units[0].contents;
        assert_eq!(plist.matches("<key>Weekday</key>").count(), 2);
        assert!(plist.contains("<integer>1</integer>"), "monday");
        assert!(plist.contains("<integer>0</integer>"), "sunday");
        assert!(plist.contains("<integer>16</integer>"));
        assert!(plist.contains("<integer>5</integer>"));
        assert!(plist.contains("<string>/opt/R&amp;D/otto</string>"));
        assert!(plist.contains("<string>report</string>"));

        let args = plist.split("<key>ProgramArguments</key>").nth(1).unwrap();
        let order = [
            "/opt/R&amp;D/otto",
            "--config",
            "/Users/me/.config/otto/jobs.toml",
            "run",
            "report",
        ];
        let mut from = 0;
        for value in order {
            let at = args[from..]
                .find(&format!("<string>{value}</string>"))
                .expect(value);
            from += at;
        }
        assert!(plist.contains(
            "<key>PATH</key>\n\t\t<string>/Users/me/.local/bin:/opt/a&lt;b/bin</string>"
        ));
        assert_eq!(
            plist
                .matches("<string>/Users/me/.local/state/otto/logs/report.log</string>")
                .count(),
            2
        );
        assert!(plist.contains("<key>StandardOutPath</key>"));
        assert!(plist.contains("<key>StandardErrorPath</key>"));
    }
}
