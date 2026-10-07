# otto

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/otto-banner-dark.webp">
  <img alt="otto: scheduled runs for coding agents, on the scheduler your OS already has" src="assets/otto-banner-light.webp">
</picture>

**Scheduled runs for coding agents, on the scheduler your OS already has.**

otto runs Claude Code or Codex on a schedule, with no session, desktop app or daemon left open. You say which agent, which prompt, where and when; launchd on macOS or systemd on Linux does the waking. The agent runs on your machine, with your tools, credentials and memory.

```
 otto · jobs                                        tue 6 oct  09:00

▸ linear-updates        claude  16:05 mon–fri              ● running
 │ next today 16:05               ✓ ✓ ✗ ✓ ●      last ok · 2m 14s
 ├──────────────────────────────────────────────────────────────────
  morning-triage        codex   07:00 daily                   paused
 │ no next run                        ✓ ✓ ·      last ok · 2m 14s
 ├──────────────────────────────────────────────────────────────────

 enter open  x stop  p pause  s skip  e prompt  n new  ? help  q quit
```

- **No daemon and no desktop app.** The operating system does the waking. Between two runs, nothing of otto is running.
- **It knows what an agent routine is.** A job is an agent, a prompt and a time, and every run leaves a record: when it started, how it ended, how long it took, what the agent wrote.
- **One screen for all of it.** Create a job, write its prompt, see when it runs next, read the output of a run, skip a day.
- **A failure tells you.** A run that fails shows a notification; a job can also tell you of every run, or of none.
- **Claude Code and Codex**, side by side.
- **Permissions are yours.** What an agent may do is what you write in the job. otto never adds to it.

## Quick start

```sh
brew install tarcisiopgs/tap/otto    # or: npm install -g @tarcisiopgs/otto
otto
```

1. **`n` creates a job.** The form asks for its name, the agent, the time and the days, the directory the agent runs in and the prompt file. `ctrl-s` saves, and a prompt file that is not there yet is created and opened in your editor.
2. **`S` schedules it.** The new job reads `not applied`: it is in your jobs file and the scheduler does not know it yet. `S` shows what the scheduler would be told, and `a` then `y` tells it.
3. **The OS runs it.** At its time, whether or not otto or any terminal is open. Open `otto` again to see how it went and read the output, or wait for the notification if it failed.

A scheduled run has nobody to answer a permission prompt, so the agent needs what it may do granted up front, in the job's arguments: `--permission-mode auto` for Claude Code, for one. The form starts them empty and otto suggests none.

## The screen

| Key | What it does |
|---|---|
| `enter` `esc` | open a job, then the output of one of its runs; go back |
| `n` `E` `d` | create a job, edit it, delete it |
| `e` | edit the prompt in your editor |
| `r` `x` | run the job now; stop the run in progress |
| `p` `s` `u` | pause, skip the next run, resume |
| `S` | what the scheduler would be told; there, `a` applies it |
| `?` `q` | every key; quit |

A run started from the screen keeps going after you quit. The screen follows the output of a run in progress, uses your terminal's own colours, and needs 60 columns by 12 rows. More in [the terminal UI](docs/terminal-ui.md).

## Without the screen

Every job is a few lines of `~/.config/otto/jobs.toml`, which the screen writes for you and you can edit by hand:

```toml
[jobs.linear-updates]
agent = "claude"                       # claude | codex
prompt = "prompts/linear-updates.md"   # relative to this file
workdir = "~/Workspace/app"            # where the agent runs
schedule = { at = "16:05", days = ["mon", "tue", "wed", "thu", "fri"] }
args = ["--permission-mode", "auto"]   # what the agent may do
notify = "finish"                      # off | failures | finish | all
```

And there is a command for what a script or another agent needs:

```sh
otto list                  # the jobs, their state, last and next run
otto sync                  # make the OS scheduler match the jobs file
otto run linear-updates    # run a job now
otto skip linear-updates   # skip the next scheduled run; also pause, resume
otto runs linear-updates   # the runs of a job, newest first
otto log linear-updates    # what the agent wrote in the latest run
```

## Good to know

- **Sync after you change something.** After editing the jobs file by hand, upgrading otto or changing your `PATH`, run `otto sync` (or `S` on the screen). A job runs with the `PATH` of the terminal that synced it.
- **A job runs once at a time**, and a job that is running is never reloaded under it.
- **otto keeps the last 50 runs of each job**, with their output, under `~/.local/state/otto`.
- **`ok` means the agent exited with 0**, not that it did what the prompt asked.
- **Windows is experimental**: it has only ever run in CI. [What that means](docs/windows.md).

The rest is in [how otto works](docs/how-it-works.md): the jobs file, every command, what a sync does, runs and notifications.

## Install

```sh
brew install tarcisiopgs/tap/otto     # macOS and Linux
npm install -g @tarcisiopgs/otto      # anywhere Node runs
```

Releases also attach a `.deb` and a binary per platform. [Every way to install](docs/install.md).

## Roadmap

- Prechecks (a command that decides whether today's run happens) and a missed-run grace window.
- Windows out of experimental, with Scoop and winget packages.

## The name

Otto is the school bus driver: he shows up on schedule. It also sounds like "auto".

## License

MIT
