# Security

otto starts coding agents unattended, on your machine and with your credentials. A flaw in it is worth reporting even when it looks small.

## Reporting a vulnerability

Report it in private, through [**Report a vulnerability**](https://github.com/tarcisiopgs/otto/security/advisories/new) on the Security tab of this repository. Please do not open a public issue for it.

Say what you did, what happened and what you expected, with the version (`otto --version`) and the operating system. A jobs file and a command that show the problem help more than anything else.

A fix goes out as a new release, and the advisory is published with it and names you, unless you would rather not be named.

## Which versions get a fix

The latest release. otto is one binary with no server side: the fix is to upgrade.

## What counts

- otto running something the jobs file did not ask for: a job name, a path or the words of a notification read as a command or as a script.
- otto giving an agent a permission it was not given. What an agent may do comes only from the `args` of its job, written by you; otto adds none.
- Another user of the machine reading or changing what otto keeps: the jobs file, the units it writes, the records and the output of the runs.
- The release itself: a binary, the npm package or the Homebrew formula not being what the workflows of this repository built.

## What does not

- What an agent does with the permissions its job gives it. That is between you and the agent CLI.
- Anything that needs someone who can already write to your jobs file, your prompts or your `PATH`: they can run what they please without otto.

## Checking a download

The files of the releases after v0.6.0 are attested by the workflow that built them:

```sh
gh attestation verify otto-aarch64-apple-darwin.tar.xz --repo tarcisiopgs/otto
```

The npm package is published with provenance (`npm audit signatures`), and each archive has its `.sha256` beside it.
