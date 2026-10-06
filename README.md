# otto

**Scheduled runs for coding agents, on the scheduler your OS already has.**

otto runs Claude Code or Codex on a schedule, without a session, a desktop app or a daemon of its own left open. You describe a job (which agent, which prompt, where and when) and otto hands it to the native scheduler: launchd on macOS, systemd timers on Linux. The agent runs on your machine, with your local tools, credentials and memory.

The name comes from Otto, the school bus driver: he shows up on schedule. It also sounds like "auto".

> **Status: early.** Jobs are scheduled for real with `otto sync`, and every run is recorded. Notifications and the terminal UI are not built yet. See [Roadmap](#roadmap).

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

otto keeps the newest 50 runs of each job, in `~/.local/state/otto/jobs/<job>/` (`$XDG_STATE_HOME/otto/jobs` when that is set): a small file per run and its output beside it. Removing a job from the jobs file keeps its history. What otto itself prints during a scheduled run, such as a prompt file it could not read, goes to `~/.local/state/otto/logs/<job>.log`.

## Build

```sh
cargo build --release
cargo test
```

Requires Rust 1.88 or newer.

## Roadmap

- Notifications when a run starts, finishes, fails or is skipped.
- Prechecks (a command that decides whether today's run happens) and a missed-run grace window.
- A terminal UI to see every job, run one now, pause it, read its log and edit its prompt.
- Windows Task Scheduler backend.

## License

MIT
