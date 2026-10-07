# otto on Windows (experimental)

What works, what has never been tried, and what differs from macOS and Linux.

On Windows a job is a task in Task Scheduler, in a folder named `otto`. Install with `npm install -g @tarcisiopgs/otto`, or take the `.zip` of a release. The commands are the same.

**It has never run on a machine someone uses.** What stands behind it is a Windows runner of the CI, where every pull request runs a real `otto sync`: a task is registered, started, its record read back, its schedule changed and the job removed. Treat it as a first version, and please [open an issue](https://github.com/tarcisiopgs/otto/issues) with what you find.

What differs from macOS and Linux:

- **The terminal UI has not been used on Windows.** It builds and its tests pass there; starting a run from it and stopping one have never run on a Windows machine. Stopping ends the whole process tree at once, since Windows has no gentler way to ask.
- **The agent CLI must be an `.exe`.** An agent installed through npm is a `.cmd`, and Windows cannot hand a `.cmd` an argument with a line break. A prompt file has at least the one at its end, so such a run is recorded as `failed`, and `otto log` says `batch file arguments are invalid`.
- **Expect a console window for every run**, open while the agent works; closing it should stop the run. A paused or skipped run flashes one too. This comes from how Task Scheduler starts a console program and has not been watched on a desktop.
- **A run that was missed is not made up for.** If the machine is off or asleep at the scheduled time, that run does not happen.
- **A job runs only while you are logged on.**
- **The job gets your own environment**, the one Windows keeps for your user, and not the `PATH` of the terminal `otto sync` ran from. `otto sync` checks for the agent CLI on the terminal's `PATH`, so a CLI that only a shell's version manager puts there passes the check and is then not found by the run.
- **An error before the run starts leaves no trace.** A jobs file that cannot be read, for one, is only seen as a failed task in Task Scheduler: there is no `logs\<job>.log` on Windows.
- **Task Scheduler is not read back.** otto keeps the definition of each task it registered in its own folder and compares against that. A task deleted by hand in Task Scheduler still shows as `unchanged`; remove the job from the jobs file, sync, add it back and sync again.
- **Write Windows paths in single quotes** in the jobs file: `workdir = 'C:\work\app'`. In double quotes TOML reads `\t` and `\n` as a tab and a line break.
- The jobs file is `%USERPROFILE%\.config\otto\jobs.toml`, and what otto remembers is under `%USERPROFILE%\.local\state\otto`, unless `HOME`, `XDG_CONFIG_HOME` or `XDG_STATE_HOME` say otherwise. Those have to be set for your user, not only in a shell, or the scheduled run and your terminal look in different places.
