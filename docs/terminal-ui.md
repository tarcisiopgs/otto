# The terminal UI

What `otto` with no subcommand shows, key by key. The short version is in the [README](../README.md).

`otto` with no subcommand opens a screen with every job: when it runs next, its state, how its last run ended and a strip of marks for its latest runs (`✓` ok, `✗` failed, `·` did not start the agent, `!` interrupted, `●` running). It updates by itself, so a scheduled run shows up when it starts.

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

| Key | What it does |
|---|---|
| `enter` | open the job, then the output of one of its runs |
| `esc` | go back |
| `r` | run the job now, when it is not running |
| `x` | stop the run in progress; `y` confirms |
| `p` `s` `u` | pause, skip the next scheduled run, resume |
| `e` | edit the prompt in `$VISUAL` or `$EDITOR`; the value may carry arguments (`code -w`) and quotes around a path with a space |
| `↑` `↓` `k` `j` `pgup` `pgdn` `g` `G` | move |
| `?` | every key |
| `n` | create a job |
| `E` `d` | edit the selected job, delete it; `y` confirms the delete |
| `S` | what a sync would do; there, `a` applies it and `y` confirms |
| `q` | quit |

A run started from the screen does not belong to it: it keeps going after you quit, and shows as running when you open otto again. The screen follows the output of a run in progress and loads the last 1 MiB of a long one; `otto log` prints all of it. Colours and other terminal escapes in the output are left out, and a progress line that rewrites itself shows where it ended.

Stopping a run ends the agent and everything it started. otto first checks that the process the record names is still that run, since a record left open by a crash can name a process id the system has given to something else, and refuses when it is not. It also refuses when it cannot end the run as a whole, which is the case for a process that does not lead its own process group. A stopped run is recorded as `interrupted`.

The next run is otto's own reading of the schedule, not something it asks the OS scheduler. On the two days a year the clocks change, a time that does not exist or happens twice may fire at a different moment than the screen says.

## Creating and editing a job

`n` opens a form for a new job and `E` opens it on the selected one: name, agent, time, days, working directory, prompt file, arguments (one to a line) and which runs the job [tells you about](how-it-works.md#notifications). `tab` moves between the fields, `space` marks an agent, a day or a level of notification, `ctrl-s` saves and `esc` leaves; every letter is text there, so the single-letter keys of the other screens do not apply. The line above the keys says what the field you are on takes.

Saving writes `jobs.toml`, and only what changed in it: your comments, the order of the jobs and the layout of the file stay as they were. A job that cannot be saved says why in the words the jobs file is read with, and the reason stays until you change the job. An empty working directory or prompt path is refused. A working directory that is not there, or an agent that is not on the `PATH`, is a note and does not stop the save, since `otto sync` checks both. If the file changed on disk while the form was open, the save is refused and the list is read again.

A new job gets `prompts/<name>.md` beside the jobs file as its prompt, unless you type another path. A prompt file that does not exist is created and opened in your editor. The name of a job cannot be changed afterwards: it is what its history and its scheduler unit are kept under.

No day marked means every day. Arguments the form cannot hold on a line each (one with a line break, with a space at an end, or empty) are kept exactly as the file has them and can only be changed there. The arguments start empty and otto suggests none: a scheduled run has nobody to answer a permission prompt, and what the agent may do is yours to write.

`d` removes the job from `jobs.toml` after asking. Its prompt file and its history stay on disk. A job with a run in progress is not deleted: stop the run first.

## Applying the sync

**A job created, edited or deleted here is not scheduled until the sync is applied**, the same as after editing the file by hand. The list says so: a job the scheduler does not have as the file has it reads `not applied` where its next run would be, and the line above the keys counts what is waiting. A job the sync cannot handle, such as one whose prompt file is gone, reads `sync error` there and is counted apart.

`S` shows what a sync would do, one job to a line: `add`, `update`, `remove`, or `error` with the reason under it. A job with a run in progress reads `busy` and is left for a later sync. Nothing is touched until `a`, which asks first, and `y`. The screen then says what was done in the words of `otto sync`: `added`, `updated`, `removed`.

It is the same sync as the command, with the jobs file and the `PATH` otto was opened with. What is applied is what was listed: if the jobs file changed or went away after the screen last read it, nothing is applied and the screen asks for another look. A jobs file that is missing removes nothing.

The screen needs a terminal of at least 60 columns by 12 rows. Where there is no terminal (a pipe, a script), `otto` alone prints the help and exits with 2. It uses your terminal's own colours and background.
