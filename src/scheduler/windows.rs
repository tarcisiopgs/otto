use std::path::PathBuf;

use anyhow::Result;

use super::runner::{Runner, must};
use super::{Context, PREFIX, Scheduler, Unit, file_names, remove_unit, write_units, xml_escape};
use crate::config::{self, Job, is_job_name};

/// Windows: one Task Scheduler task per job. Experimental: it is exercised by
/// the Windows runner of the CI and has not been used on a real machine.
///
/// Task Scheduler keeps tasks in its own store, not in files. The task
/// definitions otto registers are kept in a directory of its own so that, as
/// on the other systems, what is on disk there is what is scheduled.
pub struct Windows {
    tasks_dir: PathBuf,
}

impl Windows {
    pub fn for_user() -> Result<Windows> {
        Ok(Windows {
            tasks_dir: config::state_dir()?.join("tasks"),
        })
    }

    fn file(&self, job_name: &str) -> PathBuf {
        self.tasks_dir.join(format!("{PREFIX}{job_name}.xml"))
    }
}

/// The task's name: every otto task lives in the `otto` folder.
fn task_name(job_name: &str) -> String {
    format!(r"otto\{job_name}")
}

impl Scheduler for Windows {
    fn units(&self, job_name: &str, job: &Job, ctx: &Context) -> Result<Vec<Unit>> {
        let (hour, minute) = job.schedule.time()?;
        let days: String = job
            .schedule
            .days
            .iter()
            .map(|day| format!("          <{} />\n", day.windows_name()))
            .collect();
        // The start date only anchors the weekly schedule; it is in the past
        // on purpose. Task Scheduler runs a task with the user's own
        // environment, so no PATH is written here, and it has no setting for
        // where the output goes: otto keeps each run's output itself.
        let contents = format!(
            r#"<?xml version="1.0"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>otto job {job_name}</Description>
  </RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>2000-01-01T{hour:02}:{minute:02}:00</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByWeek>
        <DaysOfWeek>
{days}        </DaysOfWeek>
        <WeeksInterval>1</WeeksInterval>
      </ScheduleByWeek>
    </CalendarTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>false</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{otto}</Command>
      <Arguments>--config "{config}" run {job_name} --scheduled</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
            job_name = xml_escape(job_name),
            otto = xml_escape(&ctx.otto.to_string_lossy()),
            config = xml_escape(&ctx.config.to_string_lossy()),
        );
        Ok(vec![Unit {
            path: self.file(job_name),
            contents,
        }])
    }

    fn installed(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = file_names(&self.tasks_dir)?
            .iter()
            .filter_map(|name| name.strip_prefix(PREFIX)?.strip_suffix(".xml"))
            .filter(|job| is_job_name(job))
            .map(str::to_owned)
            .collect();
        names.sort();
        Ok(names)
    }

    fn load(&self, job_name: &str, units: &[Unit], runner: &dyn Runner) -> Result<()> {
        write_units(&self.tasks_dir, units)?;
        for unit in units {
            // /F replaces a task of the same name instead of failing on it.
            must(
                runner,
                "schtasks",
                &[
                    "/Create",
                    "/TN",
                    &task_name(job_name),
                    "/XML",
                    &unit.path.to_string_lossy(),
                    "/F",
                ],
            )?;
        }
        Ok(())
    }

    fn unload(&self, job_name: &str, runner: &dyn Runner) -> Result<()> {
        let task = task_name(job_name);
        // `/Delete` fails on a task that is not registered, so ask first.
        if runner.run("schtasks", &["/Query", "/TN", &task])?.success {
            must(runner, "schtasks", &["/Delete", "/TN", &task, "/F"])?;
        }
        remove_unit(&self.file(job_name))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::config::Config;
    use crate::scheduler::runner::Recorder;

    const FILE: &str = "io.github.tarcisiopgs.otto.report.xml";

    fn backend(dir: &Path) -> Windows {
        Windows {
            tasks_dir: dir.to_path_buf(),
        }
    }

    fn report_units(dir: &Path) -> Vec<Unit> {
        vec![Unit {
            path: dir.join(FILE),
            contents: "<Task/>".to_owned(),
        }]
    }

    #[test]
    fn the_task_carries_the_schedule_and_the_command() {
        let config = Config::parse(
            "[jobs.report]\nagent = \"claude\"\nprompt = \"p.md\"\nworkdir = \"w\"\nschedule = { at = \"16:05\", days = [\"mon\", \"sun\"] }\n",
            Path::new("base"),
            None,
        )
        .unwrap();
        let dir = PathBuf::from("tasks");
        let ctx = Context {
            otto: PathBuf::from(r"C:\Program Files\otto\otto.exe"),
            config: PathBuf::from(r"C:\Users\me\R&D\jobs.toml"),
            path: "a-path-that-must-not-appear".to_owned(),
            log_dir: PathBuf::from("logs"),
        };

        let units = backend(&dir)
            .units("report", config.job("report").unwrap(), &ctx)
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, dir.join(FILE));
        let xml = &units[0].contents;
        assert!(xml.contains(r"<Command>C:\Program Files\otto\otto.exe</Command>"));
        assert!(xml.contains(
            r#"<Arguments>--config "C:\Users\me\R&amp;D\jobs.toml" run report --scheduled</Arguments>"#
        ));
        assert!(xml.contains("<StartBoundary>2000-01-01T16:05:00</StartBoundary>"));
        assert!(xml.contains("<Monday />") && xml.contains("<Sunday />"));
        assert!(!xml.contains("<Tuesday />"));
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        // Task Scheduler gives a task the user's own environment.
        assert!(!xml.contains("a-path-that-must-not-appear"));
    }

    #[test]
    fn installed_lists_only_otto_tasks() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "io.github.tarcisiopgs.otto.report.xml",
            "io.github.tarcisiopgs.otto.a-b.xml",
            "other.xml",
            "io.github.tarcisiopgs.otto..xml",
            "io.github.tarcisiopgs.otto.Bad.xml",
        ] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        assert_eq!(backend(dir.path()).installed().unwrap(), ["a-b", "report"]);
    }

    #[test]
    fn installed_is_empty_without_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            backend(&dir.path().join("missing"))
                .installed()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn load_writes_the_task_and_registers_it() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("nested").join("tasks");
        let units = report_units(&tasks_dir);
        let runner = Recorder::new();

        backend(&tasks_dir).load("report", &units, &runner).unwrap();

        assert_eq!(
            fs::read_to_string(&units[0].path).unwrap(),
            units[0].contents
        );
        assert_eq!(
            runner.calls(),
            [format!(
                r"schtasks /Create /TN otto\report /XML {} /F",
                units[0].path.display()
            )]
        );
    }

    #[test]
    fn load_reports_a_failed_registration() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Recorder::new().answering("schtasks /Create", &[false]);
        assert!(
            backend(dir.path())
                .load("report", &report_units(dir.path()), &runner)
                .is_err()
        );
    }

    #[test]
    fn unload_deletes_the_task_and_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        fs::write(&file, "<Task/>").unwrap();
        let runner = Recorder::new();

        backend(dir.path()).unload("report", &runner).unwrap();

        assert_eq!(
            runner.calls(),
            [
                r"schtasks /Query /TN otto\report",
                r"schtasks /Delete /TN otto\report /F"
            ]
        );
        assert!(!file.exists());
    }

    #[test]
    fn unload_removes_the_file_of_a_task_that_is_not_registered() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        fs::write(&file, "<Task/>").unwrap();
        let runner = Recorder::new().answering("schtasks /Query", &[false]);

        backend(dir.path()).unload("report", &runner).unwrap();

        assert!(!runner.calls().iter().any(|call| call.contains("/Delete")));
        assert!(!file.exists());
    }
}
