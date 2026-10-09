# Product

<!-- impeccable:product-schema 1 -->

## Platform

terminal

## Users

Developers who already run coding agents (Claude Code, Codex) in the terminal every day and want some of that work to happen on a schedule, without leaving a desktop app or a session open. The maintainer is the first such user: his routines ran on Orca's automations, then on a hand-written launchd wrapper that could not be managed and reported nothing.

## Product Purpose

otto is a terminal manager for scheduled agent automations. The user creates an automation (which agent, which prompt, which folder, when), and otto hands it to the scheduler the operating system already has. From the same terminal the user creates new automations, writes and edits their prompt, picks the agent, and skips a run.

Success is the maintainer's real routines running on otto instead of the hand-written wrapper, and him knowing what each one did without opening a log file.

## Positioning

- No daemon and no desktop app. launchd on macOS and systemd timers on Linux do the waking; otto writes the unit and gets out of the way.
- It understands an agent routine, which a generic scheduler front end does not. The maintainer surveyed the open-source launchd TUIs and GUIs on 2026-10-05 (launchdeck, launchk, launchtui, launchdui, launchd-ui): they show launchd jobs, and none of them knows about an agent, a prompt or a run history.
- Agent-agnostic. Claude Code and Codex are both first-class.

## Operating Context

The user lives in a terminal. A scheduled run happens with nobody watching and nobody to answer a permission prompt, often while the user is in another app or away from the machine. What the user sees of a run is whatever otto kept of it.

The agent runs on the user's machine with the user's credentials, tools and memory.

## Capabilities and Constraints

Built today (v0.6.1):

- `~/.config/otto/jobs.toml` describes jobs: agent, prompt file, working directory, schedule (`at` and optional `days`), extra agent arguments.
- `otto sync` makes the scheduler match the jobs file: it adds, reloads and removes otto's units, and `--dry-run` only reports. Each unit carries the `PATH` of the terminal `sync` ran from. A job with a run in progress is reported as `busy` and left alone.
- Run records: every run of a job is kept with its start, trigger (scheduled or manual), outcome, duration and output. `otto runs <job>` lists them and `otto log <job>` prints the output. The newest 50 per job are kept.
- Skip and pause are separate. `otto skip <job>` drops the next scheduled run only; `otto pause <job>` stops scheduled runs until `otto resume <job>`. Both are recorded as runs that did not start the agent. A manual `otto run <job>` always runs and leaves them as they were.
- `otto list` (with each job's state and last outcome) and `otto run <job>` (with `--dry-run`).
- Scheduler backends for launchd and systemd behind one trait, and an experimental one for Windows Task Scheduler.
- A terminal UI, `otto` with no subcommand: every job with its state, its next run, how its last run ended and a strip of marks for its latest runs; one job with its run history; the output of a run, followed while it lasts. From it the user runs a job now, stops a run, pauses, skips, resumes and opens the prompt in their editor. A run started there outlives the screen.
- A form in the terminal UI that creates and edits a job, and a key that deletes one. It writes `jobs.toml` in place: only the values that changed are touched, so the user's comments and layout stay. The prompt file of a new job is created and opened in the user's editor.
- The sync in the terminal UI: the list marks each job the scheduler does not have as the jobs file has it, a screen shows what a sync would do, and applying it is a question answered with `y`. It is the sync of the command, run with the `PATH` the screen was opened with.
- Notifications on the machine the job ran on, through what the system already has. A job tells of failures unless its `notify` says otherwise: `off`, `failures`, `finish` or `all`. An interrupted run tells nothing, and nothing reaches a phone.
- When a job runs next, computed by otto from the schedule and the job's state. `otto list` shows it too.

Constraints:

- otto manages only the jobs it created. Other LaunchAgents and timers on the machine are out of scope and stay invisible.
- otto never adds a permission-bypassing flag to an agent command. What an agent may do comes from the job, written by the user.
- macOS and Linux. Windows is experimental: it has only ever run on a CI runner, where a real sync is exercised against Task Scheduler, and never on a machine someone uses. No daemon of otto's own.

Not built, and not yet decided in detail:

- Prechecks and a grace window for a missed run.

## Brand Commitments

The name is otto, lowercase, after Otto the school bus driver, who shows up on schedule; it also sounds like "auto". The README line is "Scheduled runs for coding agents, on the scheduler your OS already has."

otto has its own identity. Lisa, the maintainer's other terminal tool, is a reference for what worked there and is not a design system otto inherits.

## Evidence on Hand

- The terminal UI was driven end to end on 2026-10-06 in a throwaway setup: its own state directory, a jobs file of two made-up jobs and a fake `claude` on the `PATH`. Running, following the output, stopping, pausing, a jobs file broken from outside and a run that outlives the screen all behaved as described. It has not been used against the maintainer's real jobs, and there is no screenshot or recording of it yet: do not present one as if it existed.
- `examples/jobs.toml` and `examples/prompts/linear-updates.md` are the shipped example.
- An end-to-end test on 2026-10-05 ran one Claude Code job and one Codex job through launchd; both exited 0 and wrote the expected file. That test used hand-installed units, before `otto sync` existed; `otto sync` itself has not been run for real against launchd or systemd.
- Memory use of otto has not been measured.

## Product Principles

- The operating system keeps the schedule. otto describes the job and reads back what happened; it never becomes a process that must stay alive.
- A run nobody watched must leave a record. If the user cannot tell whether it ran and how it ended, the run did not do its job.
- Permissions belong to the user. otto passes them through and never widens them.
- An automation is an agent, a prompt and a time. Everything on screen serves one of those three.
