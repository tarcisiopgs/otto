use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::agent::Agent;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub jobs: BTreeMap<String, Job>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub agent: Agent,
    /// File holding the prompt. Relative paths resolve against the config file.
    pub prompt: PathBuf,
    /// Directory the agent runs in.
    pub workdir: PathBuf,
    pub schedule: Schedule,
    /// Extra arguments for the agent CLI, placed before the prompt.
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    /// Local time of day, `HH:MM`.
    pub at: String,
    #[serde(default = "Weekday::every_day")]
    pub days: Vec<Weekday>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    fn every_day() -> Vec<Weekday> {
        use Weekday::*;
        vec![Mon, Tue, Wed, Thu, Fri, Sat, Sun]
    }

    /// launchd counts from Sunday = 0.
    pub fn launchd_number(self) -> u8 {
        match self {
            Weekday::Sun => 0,
            Weekday::Mon => 1,
            Weekday::Tue => 2,
            Weekday::Wed => 3,
            Weekday::Thu => 4,
            Weekday::Fri => 5,
            Weekday::Sat => 6,
        }
    }

    pub fn systemd_name(self) -> &'static str {
        match self {
            Weekday::Mon => "Mon",
            Weekday::Tue => "Tue",
            Weekday::Wed => "Wed",
            Weekday::Thu => "Thu",
            Weekday::Fri => "Fri",
            Weekday::Sat => "Sat",
            Weekday::Sun => "Sun",
        }
    }

    /// The element Task Scheduler names the day with.
    pub fn windows_name(self) -> &'static str {
        match self {
            Weekday::Mon => "Monday",
            Weekday::Tue => "Tuesday",
            Weekday::Wed => "Wednesday",
            Weekday::Thu => "Thursday",
            Weekday::Fri => "Friday",
            Weekday::Sat => "Saturday",
            Weekday::Sun => "Sunday",
        }
    }
}

impl Schedule {
    /// `(hour, minute)` parsed from `at`.
    pub fn time(&self) -> Result<(u8, u8)> {
        let parsed = self.at.split_once(':').and_then(|(hour, minute)| {
            Some((hour.parse::<u8>().ok()?, minute.parse::<u8>().ok()?))
        });
        match parsed {
            Some((hour, minute)) if hour < 24 && minute < 60 => Ok((hour, minute)),
            _ => bail!("schedule.at must be HH:MM, got {:?}", self.at),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        Config::parse(&text, base, home_dir().as_deref())
            .with_context(|| format!("invalid config {}", path.display()))
    }

    /// Parses and validates. `base` anchors relative paths; `home` expands `~/`.
    pub fn parse(text: &str, base: &Path, home: Option<&Path>) -> Result<Config> {
        let mut config: Config = toml::from_str(text)?;
        for (name, job) in &mut config.jobs {
            if !is_job_name(name) {
                bail!("job name {name:?} must be lowercase letters, digits and dashes");
            }
            job.schedule.time().with_context(|| format!("job {name}"))?;
            if job.schedule.days.is_empty() {
                bail!("job {name}: schedule.days cannot be empty");
            }
            job.prompt = resolve(&job.prompt, base, home);
            job.workdir = resolve(&job.workdir, base, home);
        }
        Ok(config)
    }

    pub fn job(&self, name: &str) -> Result<&Job> {
        self.jobs
            .get(name)
            .with_context(|| format!("no job named {name:?}"))
    }
}

/// Lowercase letters, digits and dashes: the name ends up in service labels and
/// file names.
pub fn is_job_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn resolve(path: &Path, base: &Path, home: Option<&Path>) -> PathBuf {
    if let (Ok(rest), Some(home)) = (path.strip_prefix("~"), home) {
        return home.join(rest);
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// `$XDG_CONFIG_HOME/otto/jobs.toml`, falling back to `~/.config/otto/jobs.toml`.
pub fn default_path() -> Result<PathBuf> {
    let dir = match env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => home_dir()
            .context("cannot find the home directory; pass --config")?
            .join(".config"),
    };
    Ok(dir.join("otto").join("jobs.toml"))
}

/// `<xdg_state>/otto`, falling back to `<home>/.local/state/otto`.
pub fn state_dir_from(xdg_state: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
    let state = match (xdg_state, home) {
        (Some(dir), _) => dir.to_path_buf(),
        (None, Some(home)) => home.join(".local").join("state"),
        (None, None) => bail!("cannot find the home directory to keep state in"),
    };
    Ok(state.join("otto"))
}

/// Where otto keeps what it remembers between runs.
pub fn state_dir() -> Result<PathBuf> {
    let xdg_state = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    state_dir_from(xdg_state.as_deref(), home_dir().as_deref())
}

/// The `logs` directory inside the state directory.
pub fn log_dir_from(xdg_state: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
    Ok(state_dir_from(xdg_state, home)?.join("logs"))
}

/// Where the output of scheduled runs is kept, one file per job.
pub fn log_dir() -> Result<PathBuf> {
    let xdg_state = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    log_dir_from(xdg_state.as_deref(), home_dir().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../examples/jobs.toml");

    fn parse(text: &str) -> Result<Config> {
        Config::parse(text, Path::new("/etc/otto"), Some(Path::new("/home/me")))
    }

    #[test]
    fn the_shipped_example_is_valid() {
        let config = parse(EXAMPLE).unwrap();
        let job = config.job("linear-updates").unwrap();
        assert_eq!(job.agent, Agent::Claude);
        assert_eq!(job.schedule.time().unwrap(), (16, 5));
        assert_eq!(job.schedule.days.len(), 5);
        assert_eq!(job.prompt, Path::new("/etc/otto/prompts/linear-updates.md"));
        assert_eq!(job.workdir, Path::new("/home/me/Workspace/app"));
    }

    #[test]
    fn days_default_to_the_whole_week() {
        let config = parse(
            "[jobs.nightly]\nagent = \"codex\"\nprompt = \"/p.md\"\nworkdir = \"/w\"\nschedule = { at = \"02:30\" }\n",
        )
        .unwrap();
        assert_eq!(config.job("nightly").unwrap().schedule.days.len(), 7);
    }

    #[test]
    fn a_bad_time_names_the_job() {
        let error = parse(
            "[jobs.broken]\nagent = \"claude\"\nprompt = \"/p.md\"\nworkdir = \"/w\"\nschedule = { at = \"25:00\" }\n",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("job broken"));
    }

    #[test]
    fn state_lives_under_the_xdg_state_directory() {
        assert_eq!(
            state_dir_from(Some(Path::new("/state")), Some(Path::new("/home/me"))).unwrap(),
            Path::new("/state/otto")
        );
        assert_eq!(
            state_dir_from(None, Some(Path::new("/home/me"))).unwrap(),
            Path::new("/home/me/.local/state/otto")
        );
        assert!(state_dir_from(None, None).is_err());
    }

    #[test]
    fn logs_live_under_the_state_directory() {
        assert_eq!(
            log_dir_from(Some(Path::new("/state")), Some(Path::new("/home/me"))).unwrap(),
            Path::new("/state/otto/logs")
        );
        assert_eq!(
            log_dir_from(None, Some(Path::new("/home/me"))).unwrap(),
            Path::new("/home/me/.local/state/otto/logs")
        );
        assert!(log_dir_from(None, None).is_err());
    }

    #[test]
    fn job_names_stay_safe_for_labels() {
        let error = parse(
            "[jobs.\"Bad Name\"]\nagent = \"claude\"\nprompt = \"/p.md\"\nworkdir = \"/w\"\nschedule = { at = \"09:00\" }\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("lowercase"));
    }
}
