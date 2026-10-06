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
- **DIM** (`Modifier::DIM`): the margin rule and the ruled line, the ` · ` separators, the clock, labels of facts (`workdir`, `prompt`, `args`), `last`, durations, the trigger, `active`, `never ran`, ` no runs yet`, the second line of an empty state, the words of the shortcut bar, the `·` mark.
- **BOLD** (`Modifier::BOLD`): what is selected or waits on the user. The name of the selected job, `paused`, the question, every key the user can press (shortcut bar, help), and `otto` in the header.

### Named Rules
**The Own Ground Rule.** Never set a background. No cell in `src/tui` carries one; the terminal's background is the page.

**The Two Glyphs Rule.** Yellow is spent on `▸` and `●`. A new screen that needs emphasis uses BOLD.

**The Colourless Rule.** Remove every colour and the screen must still say the same thing. A state that only a colour distinguishes is not drawn yet.

## Typography

There is no typeface: the terminal's font is the user's. Type is case and weight.

- **Case:** lowercase throughout. The only capitals on screen are data (a path, an agent's output, `HH:MM` in an error from the parser), the key `G`, and `MiB`.
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

Three line glyphs: `│` (margin rule), `├` and `─` (the rule that closes an entry). No corners, no boxes, no vertical rule anywhere but column 1. The remaining glyphs are the marks and `▸`.

### The marks

| Mark | Meaning | Style |
| --- | --- | --- |
| `✓` | ok | default |
| `✗` | failed | red |
| `·` | did not start the agent (skipped, paused) | DIM |
| `!` | interrupted | red |
| `●` | running | yellow |

## Components

### Header
` otto` (BOLD) ` · ` (DIM) then the place: `jobs`, the job's name, `{job} · {start of the run}`, `help`. Right: the clock (DIM). On the log screen the right side is the run instead: its mark, how it ended, ` · {duration}`.

### Entry
- **First line:** `▸ ` or two spaces; the name in 20 columns (BOLD when selected, cut with `…`); two spaces; the agent in 8 columns; the schedule. The state is right-aligned.
- **Second line:** ` │ `, the coming run, the run strip, and how the last run ended, right-aligned.
- **Rule:** ` ├───…`, DIM.
- **Opened** (job screen): the same two lines without the marker, then the facts behind the rule (` │ ` and a DIM label in 9 columns: `workdir`, `prompt`, `args`; `none` for no arguments), then the rule, then the runs. On a terminal too short to show three runs under them, the facts give way and the rule follows the second line.

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
| jobs | `enter open` `r run` or `x stop` `p pause` `s skip` `u resume` `e prompt` | `? help` `q quit` |
| job | `enter log` `r run` or `x stop` `p pause` `s skip` `u resume` `e prompt` | `esc back` `? help` |
| log | `↑↓ scroll` `g top` `G end` | `esc back` `? help` |
| help | | `esc back` `q quit` |

The way back and the way to the help or the way out never leave the bar. An action that does not fit is left out whole; it still works and the help lists it.

The bar offers what would do something: `x stop` for a job that is running and `r run` for one that is not, never both.

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
| `?` | help | everywhere |
| `q` `ctrl-c` | quit | everywhere |
| `r` | run the job now | jobs, job |
| `x` | stop the run in progress (asks first) | jobs, job |
| `p` | pause the schedule | jobs, job |
| `s` | skip the next run | jobs, job |
| `u` | resume the schedule (clears pause and skip) | jobs, job |
| `e` | edit the prompt in the user's editor | jobs, job |
| `y` | yes | question |
| `n` `esc` `ctrl-c` | no | question |

### Screens not drawn yet

The job form and the sync preview are not built. Nothing below is observed; it is what the rules above already decide for them.

- **Job form:** one opened entry. Each field is a fact behind ` │ `: DIM label in a fixed column, value in the default foreground. The field being edited is the selection: `▸` in column 0 and its label BOLD. A validation problem is red and names the problem and the recovery, in the words the jobs file error already uses (`schedule.at must be HH:MM, got "25:00"`). The built system has no treatment for a warning: it is default foreground, never yellow, never red, never led by `!`, and it needs a word of its own to be told from a problem. The key map has no text entry: while a field takes text, the single-letter keys are text, and that screen's shortcut bar must say so with the keys that remain.
- **Sync preview:** one entry per job, the action where the state sits, right-aligned on the first line; the reason for an error behind ` │ `. `otto sync` already reports `added`, `updated`, `removed`, `unchanged`, `busy` and `error`; the preview uses the same stems. `error` is red; the others are told apart by the word. Applying it is a question in the shortcut bar, named after the action, answered with `y` and `n`.

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
- **Do** keep the way back and the way to the help in the shortcut bar at every width.

### Don't:
- **Don't** draw a box, a border or a pane, or split a screen into a list and its detail.
- **Don't** set a background colour or reverse video, for selection or anything else.
- **Don't** use yellow on a word, or red on anything that is not a failure or an error.
- **Don't** let Enter confirm a question.
- **Don't** open a popup or an overlay; a question takes the shortcut bar.
- **Don't** capitalise a label, a weekday or a month.
- **Don't** wrap an entry line; give content up in a stated order instead.
