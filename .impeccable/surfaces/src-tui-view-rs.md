---
version: 1
slug: "src-tui-view-rs"
primary_target: "src/tui/view.rs"
related_targets: ["src/tui/keys.rs"]
---

# Terminal UI (`otto` with no subcommand)

Scope: the whole terminal UI, drawn by `src/tui/view.rs` with ratatui. Mode: Operate.

Audience and job: a developer who runs scheduled coding agents and wants to know what each routine did, and when it runs again, without opening a log file. Screens: jobs, one job with its runs, the output of one run, the job form, the sync preview.

Constraints: a grid of character cells, no typeface of its own; the terminal's own background and its 16 theme colours, so it works in light and dark themes and over SSH; every state readable without colour; screen text in English; labels otto already uses (`active`, `paused`, `skip next`, `running`, `ok`, `failed`, `skipped`, `interrupted`) stay as they are.

## Direction contract

THESIS: otto is a logbook. Every job is an entry and every run leaves a mark in it; the user reads the history without opening anything. It refuses the bordered two-pane list-and-detail layout this category always ships.

OWN-WORLD: lowercase throughout. No boxes: entries are separated by one dim ruled line that grows out of a margin rule (`│`, `├───`). One accent, school-bus yellow (the terminal's yellow), on two glyphs only, the selection marker `▸` and the mark of a run in progress `●`, while what waits on the user is bold in the terminal's own foreground; red only for a failure or an error; dim for rules and secondary facts. Run marks are distinct glyphs, never colour alone: `✓` ok, `✗` failed, `·` did not start the agent (skipped, paused), `!` interrupted, `●` running.

STORY: the user sees which routines are healthy at a glance from the strip of marks, sees when each runs next in plain words (`today 16:05`), and reaches a run's output in two keystrokes.

FIRST VIEWPORT: a one-line header (`otto · jobs` left, date and time right), a blank line, then one two-line entry per job. Line one: selection marker, name, agent, schedule, state right-aligned. Line two, behind the margin rule: next run, the strip of the last eight runs oldest to newest, the last outcome with its duration right-aligned. A rule closes each entry. A notice line and a shortcut bar sit at the bottom; a question replaces the shortcut bar while it waits.

SIGNATURE: the run strip. It is the same strip everywhere: per job on the list, as the first column of the run list on the job screen, and as the single mark in the log header.

FORM: logbook ledger, candidate 4 of the ordered list; seed key e303f5b1. Chosen by the user over the split-flap departure board, the week timetable and the category standard.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance.

## Unresolved

- Impeccable does not recognise `terminal` as a platform and treats the project as web; its detector, comps and screenshots do not apply to a ratatui screen. The review of this surface is done on the text the screen renders.
- The job form is drawn and described in DESIGN.md. The sync preview is drawn in a later pull request and inherits this world.
