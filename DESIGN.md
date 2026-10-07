---
name: otto
description: Scheduled runs for coding agents, on the scheduler your OS already has.
colors:
  accent: yellow
  failure: red
---

# Design System: otto

## Overview

**Creative North Star: "The Logbook"**

The surface is a ratatui screen on a grid of character cells (`src/tui/view.rs`). It has no typeface, no pixel, no raster asset. The two colours in the frontmatter are slots of the terminal's own theme, not fixed values: whatever the user's theme renders for yellow and red. There are no typography, radius, spacing or component tokens to declare; the rules below are counted in columns and rows.

Every job is an entry and every run leaves a mark in it. The user reads which routines are healthy from the marks, reads when each runs next in plain words, and reaches a run's output in two keystrokes, without opening a file.

The logbook refuses bordered panes. Nothing is boxed and nothing is split into list and detail. An entry is closed by one dim ruled line that grows out of a margin rule, and a screen is a single column read top to bottom.

**Key Characteristics:**
- lowercase everywhere, including weekdays, months and the name `otto`
- one margin rule (`│`, `├───`) and no other line drawing
- one accent, on two glyphs only
- every state has a glyph or a word of its own; colour only repeats it
- the terminal's own background, in a light or a dark theme, locally or over SSH

## Colors

Two theme colours and two text modifiers, on the terminal's default foreground and background.

### Primary
- **Accent** (the theme's yellow, `Color::Yellow`): only the glyphs `▸` (the selection, "here") and `●` (a run in progress, "now"). Never a word, never a rule.

### Secondary
- **Failure** (the theme's red, `Color::Red`): the marks `✗` and `!`, the words `failed`, `failed (3)` and `interrupted`, the error block, and a notice that reports an error. Nothing else.

### Neutral
- **Default foreground**: every word that is neither secondary nor a failure. `✓`, `skip next`, `running`, values of facts, the output of a run.
- **DIM** (`Modifier::DIM`): the margin rule and the ruled line, the ` · ` separators, the clock, labels of facts (`workdir`, `prompt`, `args`, `notify`), `last`, durations, the trigger, `active`, `never ran`, ` no runs yet`, the second line of an empty state, the words of the shortcut bar, the `·` mark.
- **BOLD** (`Modifier::BOLD`): what is selected or waits on the user. The name of the selected job, `paused`, the question, every key the user can press (shortcut bar, help), and `otto` in the header.

### Named Rules
**The Own Ground Rule.** Never set a background. No cell in `src/tui` carries one; the terminal's background is the page.

**The Two Glyphs Rule.** Yellow is spent on `▸` and `●`. A new screen that needs emphasis uses BOLD.

**The Colourless Rule.** Remove every colour and the screen must still say the same thing. A state that only a colour distinguishes is not drawn yet.

## Typography

There is no typeface: the terminal's font is the user's. Type is case and weight.

- **Case:** lowercase throughout. The only capitals on screen are data (a path, an agent's output, `HH:MM` in an error from the parser), the keys `G`, `E` and `S`, and `MiB`.
- **Weight:** default, DIM and BOLD, with the roles given under Colors. No italic, no underline, no reverse video.

### The words

| Meaning | Exact text |
| --- | --- |
| state of a job | `active`, `paused`, `skip next`, `● running` |
| how a run ended | `ok`, `failed`, `failed (3)` (with the exit code), `skipped`, `paused`, `interrupted`, `running` |
| what started a run | `scheduled`, `manual` |
| coming run | `next today 16:05`, `next tomorrow 07:00`, `next mon 16:05` (within six days), `next 2026-10-20 16:05` (after that) |
| skip pending | `skips today 16:05, then tomorrow 16:05` |
| no coming run | `no next run` |
| schedule | `16:05 mon–fri` (en dash), `07:00 daily`, `09:00 mon,thu` |
| last run | `last ok · 2m 14s`, then `last ok`, then `ok`; or `running since 16:16`; or `never ran` |
| duration | `9s`, `2m 14s`, `1h 02m` |
| start of a run | `2026-10-06 18:14` |
| clock | `tue 6 oct  18:14` (two spaces before the time) |
| place in a long list | ` · 3 of 12` |

Time of day is always 24-hour `HH:MM`, on the clock of the terminal.

## Layout

One column of rows, top to bottom:

1. header (1 row)
2. blank (1 row)
3. body (height − 4 rows)
4. notice (1 row, blank when there is nothing to tell)
5. shortcut bar (1 row)

- **Margins:** every row starts one column in, and text set to the right stops one column short of the edge. Column 0 holds only the `▸` of a selected row.
- **Gaps:** two columns between fields of a row.
- **The entry** takes three rows: its first line, its second line behind `│`, and the rule ` ├───…` that closes it, which runs to one column short of the edge.
- **Behind the rule:** the first line of an entry is its heading and sits at the margin. Everything else that belongs to the entry sits behind ` │ `: its second line, its facts, the output of its run. Rows of a list of their own (the runs of a job) sit at the margin under the closing rule, with column 0 kept for `▸`.
- **Narrow lines:** a line gives up content in a stated order and never wraps, except the error block (wrapped between words) and the output of a run (broken at width − 4 columns, never cut). Text cut at its end takes `…`; a path keeps its end and takes `…` at its start.
- **Long lists:** the selection stays in view; the header says where it is (` · 3 of 12`) only when the list does not fit.
- **Minimum:** 60 columns × 12 rows. Below it the screen draws only `terminal too small: otto needs 60 × 12`. At the minimum the body is eight rows: two entries, or the whole help.

## Elevation & Depth

None. There is no layer, overlay, popup or shadow; a question replaces the shortcut bar instead of opening over the screen. The only depth is the margin rule.

## Shapes

Three line glyphs: `│` (margin rule), `├` and `─` (the rule that closes an entry). No corners, no boxes, no vertical rule anywhere but column 1. The remaining glyphs are the marks, `▸`, and on the form `⏎` between two arguments, DIM.

### The marks

| Mark | Meaning | Style |
| --- | --- | --- |
| `✓` | ok | default |
| `✗` | failed | red |
| `·` | did not start the agent (skipped, paused) | DIM |
| `!` | interrupted | red |
| `●` | running | yellow |

On the form `✓` and `·` say the same of a choice: this one is taken, this one is not.

## Components

### Header
` otto` (BOLD) ` · ` (DIM) then the place: `jobs`, the job's name, `{job} · {start of the run}`, `help`. Right: the clock (DIM). On the log screen the right side is the run instead: its mark, how it ended, ` · {duration}`.

### Entry
- **First line:** `▸ ` or two spaces; the name in 20 columns (BOLD when selected, cut with `…`); two spaces; the agent in 8 columns; the schedule. The state is right-aligned.
- **Second line:** ` │ `, the coming run, the run strip, and how the last run ended, right-aligned.
- **Rule:** ` ├───…`, DIM.
- **Opened** (job screen): the same two lines without the marker, then the facts behind the rule (` │ ` and a DIM label in 9 columns: `workdir`, `prompt`, `args`, `notify`; `none` for no arguments, and for `notify` the level the job has), then the rule, then the runs. On a terminal too short to show three runs under them, the facts give way and the rule follows the second line.

### Run strip (signature)
The latest eight runs as marks, one space apart, oldest to newest, so the newest is at the right. It is the same strip in three places: on the second line of every entry, as the first column of the run list on the job screen, and as the single mark in the header of the log.

On an entry the strip ends in a fixed column, 31 columns short of the right edge (28 kept for the ending), so the newest run of every job reads down one column. A line too narrow for that still stacks its strip with the others: the strip starts after a coming run given 22 columns, the room of an ordinary one (`next tomorrow 16:05`), and the ending gives up its duration, then the word `last`. Only when that does not fit either does the strip follow the coming run after two spaces, and last of all the oldest marks go. A coming run longer than an ordinary one (`skips today 16:05, then tomorrow 16:05`) keeps its marks but not the shared column. The newest mark and the outcome are the last to go.

### Run row
`▸ ` or two spaces, the mark, the start, the trigger (DIM, 11 columns), how it ended (14 columns), the duration (DIM).

### Output of a run
Each row behind ` │ `, in the default foreground, verbatim. `reading the output…` (DIM) until it is read.

### Empty states
One or two rows at the margin: ` no jobs yet` then ` otto reads them from {path}` (DIM); ` no runs yet` (DIM).

### Notice line
One row above the shortcut bar, one column in, cut with `…`. Default foreground for what happened or could not be done (`report is already running`), red for an error. The next action clears it. On the log screen, with nothing to tell, it says where the screen is in the output, DIM: `81–100 of 100`, ` · following`, ` · showing the last 1 MiB of the output`, or `no output yet`.

### Error block
At the top of the body, above the entries, when the jobs file cannot be used:

```
 ! invalid config jobs.toml: job x: schedule.at must be
   HH:MM, got "25:00"
   showing the last valid read; fix the file to reload
```

Red, led by ` ! `, continuation indented three columns, at most two lines (then `…`), wrapped between words, the file named without its directory. Under it, DIM, what the user is looking at and how to recover. A blank row follows.

### Shortcut bar
The last row: key (BOLD), a space, one word (DIM), two spaces.

| Screen | Yields, rightmost first | Never leaves |
| --- | --- | --- |
| jobs | `S sync` when the scheduler is behind the file, then `enter open` `r run` or `x stop` `p pause` `s skip` `u resume` `e prompt` `n new` `E edit` `d delete`; with no job yet, only `n new` | `? help` `q quit` |
| job | `enter log` `r run` or `x stop` `p pause` `s skip` `u resume` `e prompt` `E edit` `d delete` | `esc back` `? help` |
| log | `↑↓ scroll` `g top` `G end` | `esc back` `? help` |
| sync | `a apply` `↑↓ scroll` | `esc back` `? help` |
| help | | `esc back` `q quit` |
| form | `tab next`, then what the field takes: `←→ move` `space mark` on a choice, `enter line` on `args`, one argument to a line | `ctrl-s save` `esc cancel` |

The way back and the way to the help or the way out never leave the bar. An action that does not fit is left out whole; it still works and the help lists it.

The bar offers what would do something: `x stop` for a job that is running and `r run` for one that is not, never both; `p pause` and `s skip` for a job they would change, `u resume` for one that is paused or skipping.

Width is counted in columns, not characters (`src/tui/text.rs`): a CJK character takes two. A name too long for its place is cut with `…`, in the header and in the question too, so what sits at the right edge stays there.

### Question
A question replaces the shortcut bar while the app waits:

```
 stop linear-updates?  y stop  n keep running
```

The question (BOLD) names the action and what it acts on. `y` is labelled with the action, `n` with what stays as it is. Only `y` confirms. `n`, `esc` and `ctrl-c` answer no. Enter and every other key do nothing.

### Help
Two columns that fit the eight body rows of the smallest terminal: the action keys on the left, the movement keys and the legend of the marks on the right. Keys BOLD, meanings default. A new key must still fit those eight rows.

### Key map

| Key | Action | Where |
| --- | --- | --- |
| `↑` `k` | up one | jobs, job (selection); log (scroll) |
| `↓` `j` | down one | same |
| `pgup` `pgdn` | a page: on the list, as many jobs as fit | same |
| `g` `home` | first | same |
| `G` `end` | last (on the log, follow the end again) | same |
| `enter` | open the job; then open the output of the selected run | jobs, job |
| `esc` | back | job, log, help |
| `?` | help | everywhere but the form, where it is text |
| `n` | new job | jobs |
| `E` | edit the job | jobs, job |
| `d` | delete the job, after a question | jobs, job |
| `q` `ctrl-c` | quit | everywhere but the form: `q` is text there and `ctrl-c` leaves it |
| `tab` `↓` `enter` | next field (`enter` breaks a line in `args`) | form |
| `shift-tab` `↑` | previous field | form |
| `space` | mark the agent, the day or the level | form, on a choice |
| `ctrl-s` | save | form |
| `esc` `ctrl-c` | leave the form | form |
| `r` | run the job now | jobs, job |
| `x` | stop the run in progress (asks first) | jobs, job |
| `p` | pause the schedule | jobs, job |
| `s` | skip the next run | jobs, job |
| `u` | resume the schedule (clears pause and skip) | jobs, job |
| `e` | edit the prompt in the user's editor | jobs, job |
| `S` | what a sync would do | jobs |
| `a` | apply the sync | sync |
| `y` | yes | question |
| `n` `esc` `ctrl-c` | no | question |

### Job form
One opened entry, with a fact to a field, in the order an entry reads: `name`, `agent`, `at`, `days`, then `workdir`, `prompt`, `args`, `notify` as on the job screen, then the rule. On the smallest terminal the eight fields take every row and the rule gives way to them. The header reads `new job` or `edit <name>`.

- **A field** is ` │ `, a DIM label in 9 columns, and the value in the default foreground from column 12. The field in focus is the selection: `▸` in column 0, in place of the space before the rule, and its label BOLD.
- **The cursor** is the terminal's own, on the field in focus: where the next character goes, on the day `space` would mark, or on the agent that is chosen (`space` takes the next one). It is not shown while a question waits.
- **A choice** (`agent`, `days`, `notify`) lists every option: the chosen ones `✓name` with the name BOLD, the others `·name` DIM. No day chosen means every day, and the row says so in the list's word: it ends in a DIM `daily`.
- **A text wider than its room** shows the part the cursor is in, with `…` at each end that was cut. The arguments are one to a line and are shown on one row, a DIM ` ⏎ ` between them. Arguments the field cannot hold (one with a line break, with a space at an end, or empty) are not shown as if it could: the row reads `written by hand in the jobs file`, DIM, and takes no typing.
- **The levels of `notify`** go from less to more, `off` `failures` `finish` `all`: `←` and `→` stop at each end, and `space` takes the next one and goes round. A job that does not say has `failures`, which is not written to the file.
- **The name of a job that exists** is DIM: it is shown, not offered.
- **The notice row** says, in this order: why the job could not be saved, red, in the words of the jobs file, and it stays until the job changes while the focus goes to the field it names; on `args`, `a scheduled run has nobody to answer a permission prompt`, which is never out of sight there; what saving would run into, led by `note:` and never by `!`, since a note does not stop the save, and without the path the row above already shows (`note: workdir not found; codex not found in PATH`); and what the field in focus calls for (`24-hour time, as in 16:05`). On `notify` it is what the level that is chosen does: `never tells`, `tells when a run fails`, `tells when a run ends, well or not`, `tells when a run starts, ends or does not happen`.
- **Every letter is text** on this screen. Its shortcut bar is `tab next`, the keys the field in focus takes besides text, and always `ctrl-s save` `esc cancel`. Text pasted into the terminal goes into the field as text: a line break in it is not `enter`.
- **After a save or a delete** the list says what happened, in the default foreground: `nightly saved`. What is still to do is on the entry and in the reminder of the sync, below.
- **Leaving a form that changed** is a question: `discard the changes?  y discard  n keep editing`. Deleting a job is one too: `delete <name>?  y delete  n keep`.

### Sync
The scheduler runs what its units say, and the jobs file can be ahead of them. The list says so, and one screen shows by how much. The header reads `sync`.

- **On the list**, a job the scheduler does not have as the file has it reads `not applied`, BOLD in the default foreground, where its coming run would be: that run is the file's word, not yet the scheduler's, and the file's time is the schedule on the line above. A job the sync cannot handle reads `sync error` there, red. Both are on the job screen too, and neither moves the strip off its column.
- **The reminder** takes the notice row of the list when nothing else does, in the default foreground, the key BOLD: `2 changes not applied: S to review`. It counts a unit whose job is gone, which has no entry to carry the word, and a job that is `busy`. What the sync cannot do is not a change and is named apart: `2 changes not applied, 1 error: S to review`, or `1 error in the sync: S to review`. `S sync` is then the first key of the bar, so a narrow bar keeps it.
- **A change** is the first line of the job's entry without the marker: name, agent, schedule, and what the sync does where the state sits. A unit whose job is gone has only its name. There is no selection on this screen, and one rule closes the list: a list that scrolls ends in it, and one that fills the screen exactly is closed by the edge instead of scrolling for a rule. A line too long for the terminal loses the end of its schedule, never the word at the edge; the same holds for the entries of the list.
- **The words** are those of `otto sync` in the stem before and whole after: `add`, `update`, `remove` become `added`, `updated`, `removed`. `busy` is a job with a run in progress, left for a later sync. `error` is red; the others are told apart by the word.
- **The reason** of an error is under its job, behind ` │ `, in the default foreground, in two rows at most. It ends in the file or the directory it is about, so one that does not fit keeps its end: the second row opens with `…`.
- **The notice row** says, DIM, where the screen is in a list that does not fit (`1–8 of 12`) and what `busy` leaves unsaid: `busy: sync again after its run ends`. After applying it counts first, in the default foreground because it is what happened: `3 applied, 1 not · busy: sync again after its run ends`, `0 applied, 1 not`. A part that does not fit is left out whole, the last first.
- **Applying** is a question, and it counts only what the sync would do now, not what waits or cannot be done: `apply 2 changes?  y apply  n not now`. The bar offers `a apply` only while there is something to apply and nothing was applied yet; what was applied stays on screen until `esc`.
- **Nothing to show** is said on the list, where `S` was pressed: `nothing to apply`, `no scheduler on this system`, `fix the jobs file before syncing`. `a` with only errors and busy jobs says `nothing can be applied now`. A preview emptied by a sync made elsewhere closes with `nothing left to apply`, and an apply that found nothing to do with `nothing was applied`.
- **A jobs file that changed** between the look and the `y` is not applied: the screen shows one `error`, named `*` as an error of the whole sync is, with `the jobs file changed: review the sync again`.

## Do's and Don'ts

### Do:
- **Do** draw a new screen as header, blank row, body, notice, shortcut bar, and test it at 60 × 12.
- **Do** put what belongs to an entry behind ` │ ` and close the entry with ` ├───`.
- **Do** give every new state a word or a glyph before giving it a colour.
- **Do** reuse the exact words above; a new label is lowercase and as short.
- **Do** say time in words relative to today (`today 16:05`) before falling back to a date.
- **Do** name the action or the place on every control (`y stop`, `enter log`), never `ok`, `yes` or `confirm`.
- **Do** make an error name the problem and the way out (`showing the last valid read; fix the file to reload`).
- **Do** write screen text in English.
- **Do** keep the way back and the way to the help in the shortcut bar at every width. The form is the exception: `?` is text there, so its bar keeps `ctrl-s save` and `esc cancel`, and says what each field takes.

### Don't:
- **Don't** draw a box, a border or a pane, or split a screen into a list and its detail.
- **Don't** set a background colour or reverse video, for selection or anything else.
- **Don't** use yellow on a word, or red on anything that is not a failure or an error.
- **Don't** let Enter confirm a question.
- **Don't** open a popup or an overlay; a question takes the shortcut bar.
- **Don't** capitalise a label, a weekday or a month.
- **Don't** wrap an entry line; give content up in a stated order instead.
