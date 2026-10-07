# How otto works

The jobs file, the commands, what a sync does, what is kept of a run and how a run tells you. The short version is in the [README](../README.md).

## The jobs file

The jobs live in `~/.config/otto/jobs.toml`. The terminal UI writes it for you and keeps your comments and your layout; it is also yours to edit by hand, followed by an `otto sync`.

```toml
[jobs.linear-updates]
agent = "claude"                       # claude | codex
prompt = "prompts/linear-updates.md"   # relative to this file
workdir = "~/Workspace/app"            # where the agent runs
schedule = { at = "16:05", days = ["mon", "tue", "wed", "thu", "fri"] }
args = ["--permission-mode", "auto"]   # extra arguments for the agent CLI
notify = "finish"                      # off | failures | finish | all; failures when left out
```

A scheduled run has nobody to answer permission prompts, so `args` is where the agent gets its permissions up front. otto adds none on its own.

Leave `days` out for every day, and `notify` out to hear only of [failures](#notifications). [`examples/jobs.toml`](../examples/jobs.toml) is a starting point.

## Commands

```sh
otto                               # the terminal UI
otto list                          # the jobs, their state, last and next run
otto sync                          # make the OS scheduler match the jobs file
otto sync --dry-run                # print what sync would change
otto run linear-updates            # run a job now, in the foreground
otto run linear-updates --dry-run  # print the agent command instead
otto skip linear-updates           # skip the next scheduled run
otto pause linear-updates          # no scheduled runs until resumed
otto resume linear-updates         # clear pause and skip
otto runs linear-updates           # the runs of a job, newest first
otto log linear-updates            # the output of the latest run
otto log linear-updates <run-id>   # the output of one run
```

`otto` alone opens the [terminal UI](terminal-ui.md). `--config <file>` points at a different jobs file.

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

On macOS, at login launchd ties each job to the program its unit names, and kills a run when that binary was replaced since: nothing is recorded and the job shows as never run. So the unit names `/usr/bin/env`, which starts otto, and an upgrade of otto leaves the job running. A unit written by otto 0.5.0 or older names otto itself: one `otto sync` with the new version rewrites it.

Pause and skip need the unit `otto sync` writes today. After upgrading otto, run `otto sync` once, or answer the sync the terminal UI shows as not applied: a unit written by an older version starts the agent regardless. The unit carries the version of the otto that wrote it, so after an upgrade every job is one to reload.

otto keeps the newest 50 runs of each job, in `~/.local/state/otto/jobs/<job>/` (`$XDG_STATE_HOME/otto/jobs` when that is set): a small file per run and its output beside it. Removing a job from the jobs file keeps its history. What otto itself prints during a scheduled run, such as a prompt file it could not read, goes to `~/.local/state/otto/logs/<job>.log`.

## Notifications

A run that needs your attention shows a notification on the machine it ran on. How much a job tells is its `notify`:

| `notify` | Tells when |
| --- | --- |
| `off` | never |
| `failures` | a run failed; this is what a job without `notify` does |
| `finish` | a run ended, as `ok` or `failed` |
| `all` | also when a run started, was skipped, or the job was paused |

The notification names the job and what happened, `linear-updates failed`, and under it the exit code and how long the run took, or why it never started.

otto uses what the system already has: `osascript` on macOS, `notify-send` on Linux, a PowerShell toast on Windows. On macOS the notification therefore comes in the name of Script Editor, and the first one may ask you to allow it. A notification that cannot be shown never changes a run: the reason is a line starting with `otto:` in the output of the run. otto waits five seconds for the system to take one and then goes on without it.

Two things a notification does not know. A run that was interrupted, because the machine went down or the process was killed, tells nothing: no otto was left to tell. And an agent that exits with 0 without having done its job is an `ok` run, as it is everywhere else in otto.

`notify` is read when the job runs. Changing it needs no `otto sync`, and the terminal UI has it as the last field of a job.
