use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::{Context, Scheduler, Unit};
use crate::config::{Job, home_dir};

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

pub fn label(job_name: &str) -> String {
    format!("io.github.tarcisiopgs.otto.{job_name}")
}

impl Scheduler for Launchd {
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
    use std::path::Path;

    use super::*;
    use crate::config::Config;

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
