# otto

**Scheduled runs for coding agents, on the scheduler your OS already has.**

otto runs Claude Code or Codex on a schedule, without a session, a desktop app or a daemon of its own left open. You describe a job (which agent, which prompt, where and when) and otto hands it to the native scheduler: launchd on macOS, systemd timers on Linux. The agent runs on your machine, with your local tools, credentials and memory.

The name comes from Otto, the school bus driver: he shows up on schedule. It also sounds like "auto".

> **Status: early.** Jobs are scheduled for real with `otto sync`. Run history, notifications and the terminal UI are not built yet. See [Roadmap](#roadmap).

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
```

`--config <file>` points at a different jobs file. [`examples/jobs.toml`](examples/jobs.toml) is a starting point.

## Scheduling

`otto sync` reads the jobs file and makes the scheduler agree with it. A new job is added, a job whose unit would differ is reloaded, and an otto job the scheduler still has but the file no longer lists is removed. Run it again after every edit. otto only touches its own units.

The jobs file you sync is the whole list: syncing a different file with `--config` removes the jobs of the first one.

Each job runs with the `PATH` of the terminal you ran `otto sync` from, since the OS scheduler starts with a bare one and would not find the agent CLI. Run `otto sync` again if your `PATH` changes.

Before scheduling a job, otto checks that its prompt file and working directory exist and that the agent CLI is on the `PATH`. A job that fails the check is reported and left as it was.

The output of every scheduled run is appended to `~/.local/state/otto/logs/<job>.log` (`$XDG_STATE_HOME/otto/logs` when that is set).

## Build

```sh
cargo build --release
cargo test
```

Requires Rust 1.88 or newer.

## Roadmap

- Run records: when each job last ran, how it ended, the log of each run.
- Notifications when a run starts, finishes, fails or is skipped.
- Prechecks (a command that decides whether today's run happens) and a missed-run grace window.
- A terminal UI to see every job, run one now, pause it, read its log and edit its prompt.
- Windows Task Scheduler backend.

## License

MIT
