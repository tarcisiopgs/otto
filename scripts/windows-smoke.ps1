# Tries the Windows backend for real, on a CI runner: `otto sync` registers a
# task with Task Scheduler, the task is run, its record is read back, the
# schedule is changed and the job is removed. A program compiled here stands in
# for the agent. Expects target\release\otto.exe to be built.
#
# Not for a machine someone uses: it adds a folder to the user's PATH.
$ErrorActionPreference = 'Stop'

$otto = Join-Path $PWD 'target\release\otto.exe'
$work = Join-Path $env:RUNNER_TEMP 'otto-smoke'
New-Item -ItemType Directory -Force "$work\bin", "$work\job" | Out-Null

Set-Content "$work\fake.rs" 'fn main() { println!("fake agent got {} arguments", std::env::args().count() - 1); }'
rustc "$work\fake.rs" -o "$work\bin\claude.exe"
if ($LASTEXITCODE -ne 0) { throw 'could not build the fake agent' }

# Two lines: a prompt is rarely one.
Set-Content "$work\prompt.md" "line one`nline two"
$config = "$work\jobs.toml"
Set-Content $config @"
[jobs.demo]
agent = "claude"
prompt = "prompt.md"
workdir = "job"
schedule = { at = "03:00" }
"@

# Task Scheduler starts a task with the environment the user has in the
# registry, not with this shell's, so the fake agent goes on both.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
[Environment]::SetEnvironmentVariable('Path', "$work\bin;$userPath", 'User')
$env:Path = "$work\bin;$env:Path"

function Otto {
    $output = & $otto --config $config @args | Out-String
    if ($LASTEXITCODE -ne 0) { throw "otto $args exited with $LASTEXITCODE`n$output" }
    Write-Host "> otto $args`n$output"
    $output
}

function Expect($text, $pattern) {
    if ($text -notmatch $pattern) { throw "expected /$pattern/ in:`n$text" }
}

Expect (Otto sync) 'demo\s+added'
schtasks /Query /TN 'otto\demo' /V /FO LIST
if ($LASTEXITCODE -ne 0) { throw 'the task is not registered' }
Expect (Otto sync) 'demo\s+unchanged'

schtasks /Run /TN 'otto\demo'
if ($LASTEXITCODE -ne 0) { throw 'the task could not be started' }
$deadline = (Get-Date).AddSeconds(90)
do {
    Start-Sleep -Seconds 3
    $runs = & $otto --config $config runs demo | Out-String
} until ($runs -match 'scheduled\s+(ok|failed|interrupted)' -or (Get-Date) -gt $deadline)
Write-Host "> otto runs demo`n$runs"
schtasks /Query /TN 'otto\demo' /V /FO LIST | Select-String 'Last Run|Last Result|Status'
Expect $runs 'scheduled\s+ok'
Expect (Otto log demo) 'fake agent got 2 arguments'
Expect (Otto list) 'demo.*active\s+ok'

(Get-Content $config) -replace '03:00', '04:30' | Set-Content $config
Expect (Otto sync) 'demo\s+updated'
$xml = schtasks /Query /TN 'otto\demo' /XML | Out-String
Expect $xml 'T04:30:00'

Set-Content $config ''
Expect (Otto sync) 'demo\s+removed'
schtasks /Query /TN 'otto\demo' 2>$null | Out-Null
if ($LASTEXITCODE -eq 0) { throw 'the task is still registered' }
Write-Host 'The Windows backend registered, ran, updated and removed a job.'

# An agent CLI installed through npm is a `.cmd`, which Windows runs through
# cmd.exe. Shown, not required: how such an agent takes a prompt of two lines.
Set-Content "$work\bin\codex.cmd" "@echo off`r`necho fake codex got: %*"
Set-Content $config @"
[jobs.batch]
agent = "codex"
prompt = "prompt.md"
workdir = "job"
schedule = { at = "03:00" }
"@
& $otto --config $config run batch
Write-Host "an agent that is a .cmd, with a prompt of two lines: exit $LASTEXITCODE"
& $otto --config $config runs batch
& $otto --config $config log batch

# The step ends with the last native exit code, and the last ones here are
# allowed to fail.
exit 0
