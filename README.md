# otto

**Scheduled runs for coding agents, on the scheduler your OS already has.**

otto runs Claude Code or Codex on a schedule, without a session, a desktop app or a daemon of its own left open. You describe a job (which agent, which prompt, where and when) and otto hands it to the native scheduler: launchd on macOS, systemd timers on Linux. The agent runs on your machine, with your local tools, credentials and memory.

The name comes from Otto, the school bus driver: he shows up on schedule. It also sounds like "auto".

> **Status: early.** Jobs are scheduled for real with `otto sync`, and every run is recorded. Notifications and the terminal UI are not built yet. See [Roadmap](#roadmap).

## Install

otto runs on macOS and Linux, on arm64 and x64, and [experimentally on Windows](#windows-experimental). Every way below installs the same native binary; the command is `otto`.

**Homebrew**, on macOS and Linux:

```sh
brew install tarcisiopgs/tap/otto
```

**npm:**

```sh
npm install -g @tarcisiopgs/otto
```

Node is only used to start the binary. A scheduled job calls it by its full path, inside the folder npm installed the package in. That path changes when you upgrade otto through a Node version manager or switch Node versions, so run `otto sync` after either.

**Debian and Ubuntu:** releases after 0.1.0 attach a `.deb` for amd64 and arm64. Download the one for your machine from the [latest release](https://github.com/tarcisiopgs/otto/releases/latest) and install it:

```sh
sudo apt install ./otto_*.deb
```

No apt repository is involved, so a new version is installed the same way.

**A binary by hand:** each release also attaches a `.tar.xz` per platform, with its checksum.

After an upgrade by any of these, run `otto sync`: it rewrites the units when what they should contain has changed.

## A job

`~/.config/otto/jobs.toml`:

```toml
[jobs.linear-updates]
agent = "claude"                       # claude | codex
prompt = "prompts/linear-updates.md"   # relative to this file
workdir = "~/Workspace/app"            # where the agent runs
schedule = { at = "16:05", days = ["mon", "tue", "wed", "thu", "fri"] }
args = ["--permission-mode", "auto"]   # extra arguments for the agent CLI
```

A scheduled run has nobody to answer permission prompts, so `args` is where the agent gets its permissions up front. otto adds none on its own.

## Commands

```sh
otto list                          # the configured jobs
otto sync                          # make the OS scheduler match the jobs file
otto sync --dry-run                # print what sync would change
otto run linear-updates            # run a job now, in the foreground
otto run linear-updates --dry-run  # print the agent command instead
otto plan linear-updates           # print the launchd/systemd unit sync would write
otto skip linear-updates           # skip the next scheduled run
otto pause linear-updates          # no scheduled runs until resumed
otto resume linear-updates         # clear pause and skip
otto runs linear-updates           # the runs of a job, newest first
otto log linear-updates            # the output of the latest run
otto log linear-updates <run-id>   # the output of one run
```

`--config <file>` points at a different jobs file. [`examples/jobs.toml`](examples/jobs.toml) is a starting point.

## Scheduling

`otto sync` reads the jobs file and makes the scheduler agree with it. A new job is added, a job whose unit would differ is reloaded, and an otto job the scheduler still has but the file no longer lists is removed. Run it again after every edit. otto only touches its own units.

The jobs file you sync is the whole list: syncing a different file with `--config` removes the jobs of the first one.

Each job runs with the `PATH` of the terminal you ran `otto sync` from, since the OS scheduler starts with a bare one and would not find the agent CLI. Run `otto sync` again if your `PATH` changes.

A job with a run in progress is never reloaded or removed, because that would stop the run. `otto sync` reports it as `busy` and leaves it alone; sync again once the run is over. The `PATH` is part of what otto compares, so a sync from a terminal with a different `PATH` reloads every job that is not running.

Before scheduling a job, otto checks that its prompt file and working directory exist and that the agent CLI is on the `PATH`. A job that fails the check is reported and left as it was.

## Runs

Every run is recorded: when it started, whether the schedule or you started it, how it ended and how long it took. `otto runs <job>` lists them and `otto log <job>` prints what the agent wrote. `otto list` shows each job's state and how its last run ended.

A run ends as `ok`, `failed` (with the agent's exit code), `skipped`, `paused` or `interrupted`. The last one is a run whose record was left open, because the machine went down or the process was killed.

`otto skip <job>` makes the next scheduled run not happen, once. `otto pause <job>` stops scheduled runs until `otto resume <job>`. Both act when the scheduler fires the job: otto records the run as skipped or paused and does not start the agent. `otto run <job>` is you asking, so it always runs, and it leaves pause and skip as they were.

A job runs once at a time. A scheduled run that finds the job running is recorded as `skipped`; a second `otto run` is refused.

A run that could not start (the prompt file is gone, the working directory is gone, the agent CLI is not found) is recorded as `failed`, and `otto log` shows the reason.

otto knows a job is running because its latest record is still open and the otto process that opened it is alive. If a job stays `running` when nothing is running, which can happen when another process got the same process id, delete that run's `<id>.toml` in the job's directory below.

Pause and skip need the unit `otto sync` writes today. After upgrading otto, run `otto sync` once: a unit written by an older version starts the agent regardless.

otto keeps the newest 50 runs of each job, in `~/.local/state/otto/jobs/<job>/` (`$XDG_STATE_HOME/otto/jobs` when that is set): a small file per run and its output beside it. Removing a job from the jobs file keeps its history. What otto itself prints during a scheduled run, such as a prompt file it could not read, goes to `~/.local/state/otto/logs/<job>.log`.

## Windows (experimental)

On Windows a job is a task in Task Scheduler, in a folder named `otto`. Install with `npm install -g @tarcisiopgs/otto`, or take the `.zip` of a release. The commands are the same.

**It has never run on a machine someone uses.** What stands behind it is a Windows runner of the CI, where every pull request runs a real `otto sync`: a task is registered, started, its record read back, its schedule changed and the job removed. Treat it as a first version, and please [open an issue](https://github.com/tarcisiopgs/otto/issues) with what you find.

What differs from macOS and Linux:

- **The agent CLI must be an `.exe`, or the prompt one line.** An agent installed through npm is a `.cmd`, and Windows cannot hand a `.cmd` an argument with a line break. Such a run is recorded as `failed`, and `otto log` says `batch file arguments are invalid`.
- **A run that was missed is not made up for.** If the machine is off at the scheduled time, that run does not happen.
- **A job runs only while you are logged on.**
- **The job gets your own environment**, the one Windows keeps for your user, and not the `PATH` of the terminal `otto sync` ran from. After installing an agent CLI, a new terminal has it; so does the next run.
- **Task Scheduler is not read back.** otto keeps the definition of each task it registered in its own folder and compares against that. A task deleted by hand in Task Scheduler still shows as `unchanged`; remove the job from the jobs file, sync, add it back and sync again.
- The jobs file is `%USERPROFILE%\.config\otto\jobs.toml`, and what otto remembers is under `%USERPROFILE%\.local\state\otto`.

## Build from source

```sh
cargo build --release
cargo test
```

Requires Rust 1.88 or newer.

## Roadmap

- Notifications when a run starts, finishes, fails or is skipped.
- Prechecks (a command that decides whether today's run happens) and a missed-run grace window.
- A terminal UI to see every job, run one now, pause it, read its log and edit its prompt.
- Windows out of experimental, with Scoop and winget packages.

## License

MIT
