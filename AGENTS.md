# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, Cursor, Gemini CLI, OpenCode…) working in this repository. This is the single source of instructions.

## What this is

otto is a Rust CLI that runs coding agents (Claude Code, Codex) on a schedule. It has no daemon: it writes the unit the operating system scheduler understands (a launchd LaunchAgent on macOS, a systemd user timer on Linux) and that scheduler wakes `otto run <job>`.

It starts agents unattended on the user's machine, with the user's credentials. **Never add permission-bypassing flags to an agent command on otto's behalf.** What an agent may do comes from the job's `args`, written by the user.

The user-facing overview and the roadmap are in `README.md`.

## Commands

```sh
cargo test                                   # the whole suite
cargo clippy --all-targets -- -D warnings    # must be clean, CI enforces it
cargo fmt                                    # CI runs cargo fmt --check
cargo run -- --config examples/jobs.toml list
cargo run -- --config examples/jobs.toml plan linear-updates
```

CI (`.github/workflows/ci.yml`) runs Lint, Test (macOS and Linux) and Build for every pull request. The `check` job passes only when all of them pass.

Do not load a unit into launchd or systemd, and do not run a job for real, while testing on a real machine unless the user asks for it in that conversation. That rules out a bare `otto sync`. `otto plan`, `otto sync --dry-run` and `otto run --dry-run` exist for that.

## Layout

```
src/main.rs               CLI (clap): list, sync, plan, run, skip, pause, resume, runs, log
src/store.rs              Store: per-job state (paused, skip next) and run records under the state directory
src/run.rs                one run of a job: pause/skip/busy, start the agent, keep its output, close the record
src/config.rs             jobs.toml: Config, Job, Schedule, Weekday; parsing, validation, path resolution
src/agent.rs              Agent: the argv of a non-interactive run of each agent CLI
src/sync.rs               sync: compares the jobs file with the units on disk and applies the difference
src/scheduler/mod.rs      Scheduler trait, Unit, Context, native() picks the backend for this OS
src/scheduler/runner.rs   Runner: how a backend runs launchctl/systemctl; Recorder fakes it in tests
src/scheduler/launchd.rs  macOS LaunchAgent plist
src/scheduler/systemd.rs  Linux user service + timer
examples/                 a jobs.toml and a prompt to start from
npm/otto/                 the npm package: package.json and the launcher that starts the native binary
```

## Releases

The version lives in `Cargo.toml`; the tag is `vMAJOR.MINOR.PATCH` and must match it. A release is `gh release create vX.Y.Z --generate-notes`: never write the notes by hand.

Publishing the GitHub Release runs `.github/workflows/release.yml`, which builds the macOS and Linux binaries, attaches them to the release and publishes `@tarcisiopgs/otto` to npm through trusted publishing (OIDC). No npm token is stored anywhere. npm trusts that workflow by its file name, so renaming it breaks publishing.

One npm package carries every platform's binary, in `bin/<os>-<arch>/otto`. Do not split it into a package per platform: trusted publishing cannot create a package name, and each new name needs a manual bootstrap.

The same workflow attaches a `.deb` per Linux architecture, built by `scripts/build-deb.sh` around the static binary. The `Packaging` workflow installs it with apt when the packaging changes.

Homebrew lives in another repository, `tarcisiopgs/homebrew-tap` (`brew install tarcisiopgs/tap/otto`). It follows the releases on a schedule with its own token, so this repository stores no secret for it. The formula needs the four `.tar.xz` archives and their `.sha256` files under the names the release gives them: renaming an archive breaks the tap.

There is no Windows package (Scoop, winget) because there is no Windows scheduler backend.

Do not create a release or publish to npm unless the user asks for it in that conversation.

## Rules

- A new operating system is a new `Scheduler` backend. Nothing outside `src/scheduler/` knows which scheduler is in use.
- A new agent is a new `Agent` variant with its non-interactive argv. The prompt is always the last argument.
- Backends return file contents as data (`Unit`), so they are tested without touching the real scheduler.
- A backend runs `launchctl` or `systemctl` only through `Runner`, and takes its units directory as a field. Tests use `Recorder` and a temporary directory; no test loads a real unit.
- The units directory is the only record of what is scheduled. otto keeps no list of its own; a unit with otto's prefix is an otto job.
- Only `src/store.rs` knows the layout of the state directory. It takes its root as a parameter, so tests use a temporary directory.
- Code that needs the time takes it as a parameter (`jiff::Timestamp`); only `main.rs` reads the clock.
- Tests never start a real agent. `run::execute` takes the command ready to start, and tests give it `sh -c`.
- Job names are lowercase letters, digits and dashes: they end up in service labels and file names.
- No `unwrap` or `expect` outside tests; errors carry context with `anyhow`.
- Commit messages, branch names and pull requests are written in English. Never push to `main` directly: branch, push, open a pull request.
