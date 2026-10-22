<#
.SYNOPSIS
  Runs every check before a DVICode release on a Windows machine with WSL.

.DESCRIPTION
  Steps (skip any with -Skip):
    build       native Windows engine (cargo), static Linux engine (WSL, scripts/package.sh
                --server) and the extension bundle
    test        server tests natively on Windows and in WSL
    unit        the extension's unit tests (npm run unit), when the script exists
    corpus      the real-world corpus regression (scripts/corpus), when it exists
    corpus-private
                the same with a private manifest outside the repository (-PrivateCorpus or
                DVICODE_CORPUS_MANIFEST), when one is given; its summary shows only counts
    e2e-native  end-to-end tests with the engine running on Windows
    e2e-wsl     end-to-end tests in WSL: Windows drive, \\wsl.localhost folder, WSL-only folder
    e2e-ssh     end-to-end tests over SSH against a second WSL distribution
    e2e-remote-wsl  end-to-end tests in a VS Code Remote - WSL window (extension and engine in WSL)
    e2e-remote-ssh  end-to-end tests in a VS Code Remote - SSH window to the second distribution
    e2e-editors the native, WSL, dvicode:// and SSH runs again in VSCodium, Cursor and Windsurf,
                for those installed (VSCodium's portable zip may be unpacked into
                %USERPROFILE%\.dvicode-test\vscodium); a SKIP when none is

  The corpus steps run in WSL in the background while the end-to-end steps run (their output is
  shown after them); -Sequential runs them before the end-to-end steps instead. The summary lists
  the steps in the order above either way.

  Without -Vsix the end-to-end tests run the extension from client/ with the engines built here.
  With -Vsix they run a packaged extension (the universal package of a Release workflow run).

  Prerequisites (see docs/development.md, "Before a release"): Rust and the MSVC build tools on
  Windows, Rust and Python in the WSL distribution, Node.js, VS Code, and for e2e-ssh a WSL
  distribution with sshd configured on -SshPort for key login with -SshKey. The remote-window
  steps install Microsoft's Remote - WSL and Remote - SSH extensions into client/.vscode-test on
  first use, and VS Code downloads its server into WSL and onto the SSH host.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\prerelease.ps1
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\prerelease.ps1 -Vsix C:\Downloads\dvicode-0.1.1.vsix -Skip build,test
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\prerelease.ps1 -PrivateCorpus $env:USERPROFILE\dvicode-private\manifest.json
#>
param(
  [string]$Vsix,
  [string]$PrivateCorpus = $env:DVICODE_CORPUS_MANIFEST,
  [string[]]$Skip = @(),
  [string]$Distro = 'Ubuntu',
  [string]$SshDistro = 'dvicode-remote',
  [string]$SshUser = 'dv',
  [int]$SshPort = 2222,
  # In the user profile, not AppData: Windows redirects what packaged apps (such as the Claude
  # desktop app) write under AppData, so a terminal outside them would not see the key there.
  [string]$SshKey = "$env:USERPROFILE\.dvicode-test\e2e_key",
  # Runs the corpus steps before the e2e steps instead of at the same time.
  [switch]$Sequential
)
# Where earlier versions of this script kept it.
if (-not (Test-Path $SshKey) -and (Test-Path "$env:LOCALAPPDATA\dvicode-test\e2e_key")) {
  $SshKey = "$env:LOCALAPPDATA\dvicode-test\e2e_key"
}

# Native tools write progress to stderr, which Windows PowerShell turns into errors under
# 'Stop'; failures are detected from exit codes instead, and cmdlets stop explicitly.
$ErrorActionPreference = 'Continue'
# `powershell -File` passes `-Skip a,b` as one string.
$Skip = @($Skip | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() })
$Root = Split-Path -Parent $PSScriptRoot
$Client = Join-Path $Root 'client'
$Demo = Join-Path $Root 'examples\demo-project'
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$results = New-Object System.Collections.ArrayList

# A step's line in the summary, in the order the steps are listed.
function NewRow([string]$Name) {
  $row = [pscustomobject]@{ Step = $Name; Result = 'SKIP'; Seconds = 0; Detail = '' }
  [void]$results.Add($row)
  $row
}

# Runs a step's body and records PASS / FAIL / SKIP with its time and a short detail in its row.
function Finish($Row, [scriptblock]$Body) {
  $t0 = Get-Date
  $detail = ''
  $ok = $true
  try {
    $detail = & $Body
    if ($detail -is [array]) { $detail = $detail -join '; ' }
  } catch {
    $ok = $false
    $detail = $_.Exception.Message
    Write-Host "FAILED: $detail" -ForegroundColor Red
  }
  # A step with nothing to run yet returns 'SKIP: why'.
  $Row.Result = if (-not $ok) { 'FAIL' } elseif ("$detail" -like 'SKIP:*') { 'SKIP' } else { 'PASS' }
  $Row.Seconds = [int]((Get-Date) - $t0).TotalSeconds
  $Row.Detail = "$detail" -replace '^SKIP:\s*', ''
}

# Runs one step and carries on whatever happens.
function Step([string]$Name, [scriptblock]$Body) {
  $row = NewRow $Name
  if ($Skip -contains $Name) { return }
  Write-Host "`n=== $Name" -ForegroundColor Cyan
  Finish $row $Body
}

# Runs a native command, echoing its output; throws when it fails.
function Run([string]$What, [scriptblock]$Cmd) {
  & $Cmd 2>&1 | ForEach-Object { "$_" } | Out-Host
  if ($LASTEXITCODE -ne 0) { throw "$What failed (exit $LASTEXITCODE)" }
}

function WslPath([string]$WinPath, [string]$D = $Distro) {
  (wsl.exe -d $D -e wslpath -u ($WinPath -replace '\\', '/')).Trim()
}

# One end-to-end run: folder or URI, label, settings; returns the "N/N passed" line. With
# -Remote (wsl+<distro>, ssh-remote+<host>) it runs in a VS Code remote window and the target is
# a path on that machine; -SshConfig is the ssh config file that defines <host>.
function E2e([string]$Target, [string]$Label, [hashtable]$Settings = @{}, [string]$Remote = '', [string]$SshConfig = '') {
  $env:DVICODE_E2E_SETTINGS = ($Settings | ConvertTo-Json -Compress)
  # An empty value removes the variable.
  $env:DVICODE_E2E_REMOTE = $Remote
  $env:DVICODE_E2E_SSH_CONFIG = $SshConfig
  Push-Location $Client
  try {
    $out = node test/runTest.js $Target $Label 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    if ($code -eq 3) {
      # VS Code reported no result (it sometimes fails to start the first time): once more.
      Write-Host "[$Label] no result from VS Code; retrying once" -ForegroundColor Yellow
      $out = node test/runTest.js $Target $Label 2>&1 | ForEach-Object { "$_" }
      $code = $LASTEXITCODE
    }
  } finally {
    Pop-Location
  }
  $out | Where-Object { $_ -match '\] (FAIL|PASS)|passed;|extension host:|no result' } | Out-Host
  $summary = ($out | Where-Object { $_ -match 'passed;' } | Select-Object -Last 1)
  if ($code -ne 0) { throw "$Label failed: $summary" }
  return ($summary -replace '^\[[^\]]+\]\s*', '')
}

$RootWsl = WslPath $Root

# Copies the demo project to ~/dvicode-demo in WSL; returns that path.
function DemoToWsl {
  $wslDemo = (wsl.exe -d $Distro -e sh -c 'echo $HOME/dvicode-demo').Trim()
  $demoWsl = WslPath $Demo
  Run 'copy the demo into WSL' { wsl.exe -d $Distro -e bash -c "rm -rf '$wslDemo' && mkdir -p '$wslDemo' && cp -r '$demoWsl/.' '$wslDemo/'" }
  return $wslDemo
}

# Starts sshd in the SSH test distribution and copies the demo project to ~/demo there.
function DemoToSsh {
  if (-not (Test-Path $SshKey)) { throw "no SSH test key at $SshKey (see docs/development.md)" }
  # The pattern is anchored: unanchored, it also matches this very `sh -c` command line, so sshd
  # would never be started.
  Run 'start sshd' { wsl.exe -d $SshDistro -u root -e sh -c "mkdir -p /run/sshd; pgrep -f '^/usr/sbin/sshd -f /etc/ssh/sshd_dvicode.conf' >/dev/null || /usr/sbin/sshd -f /etc/ssh/sshd_dvicode.conf" }
  $sshOpts = @('-p', "$SshPort", '-i', $SshKey, '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=accept-new')
  Push-Location (Split-Path $Demo)
  try {
    # Quoted for cmd: the key's path may hold spaces.
    $cmdOpts = ($sshOpts | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }) -join ' '
    cmd /c "tar -cf - demo-project | ssh $cmdOpts $SshUser@127.0.0.1 ""rm -rf ~/demo && mkdir -p ~/demo && tar -C ~/demo --strip-components=1 -xf -"""
    if ($LASTEXITCODE -ne 0) { throw "copying the demo to the SSH host failed (exit $LASTEXITCODE)" }
  } finally { Pop-Location }
}

Step 'build' {
  $env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\dvicode-target"
  Push-Location (Join-Path $Root 'server')
  try { Run 'Windows engine build' { cargo build --release --locked } } finally { Pop-Location }
  Remove-Item Env:CARGO_TARGET_DIR
  New-Item -ItemType Directory -Force (Join-Path $Client 'server\win32-x64') | Out-Null
  Copy-Item "$env:LOCALAPPDATA\dvicode-target\release\dvicode-server.exe" (Join-Path $Client 'server\win32-x64\') -Force -ErrorAction Stop
  Run 'Linux engine build' { wsl.exe -d $Distro -e bash -lc "cd '$RootWsl' && bash scripts/package.sh --server" }
  Push-Location $Client
  try {
    Run 'npm ci' { npm ci --no-audit --no-fund }
    Run 'bundle' { node esbuild.mjs --production }
  } finally { Pop-Location }
  'Windows and Linux engines, bundle'
}

Step 'test' {
  # Both platforms run even when the first fails.
  $failures = @()
  $env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\dvicode-target"
  Push-Location (Join-Path $Root 'server')
  try { Run 'server tests (Windows)' { cargo test --locked } } catch { $failures += $_.Exception.Message } finally { Pop-Location }
  Remove-Item Env:CARGO_TARGET_DIR
  try { Run 'server tests (WSL)' { wsl.exe -d $Distro -e bash -lc "cd '$RootWsl' && bash scripts/wsl-cargo.sh test --locked" } } catch { $failures += $_.Exception.Message }
  if ($failures) { throw ($failures -join '; ') }
  'Windows and WSL'
}

Step 'unit' {
  $pkg = Get-Content (Join-Path $Client 'package.json') -Raw | ConvertFrom-Json
  if (-not $pkg.scripts.unit) { return 'SKIP: no unit script yet' }
  Push-Location $Client
  try { Run 'unit tests' { npm run unit --silent } } finally { Pop-Location }
  'npm run unit'
}

# The corpus steps: `run.sh all` in WSL (fetch, build the engine, check, sweep the IDE queries,
# compare), and a check of its output lines and exit code. They need WSL's CPUs and the e2e steps
# need VS Code windows on Windows, so the corpus steps run one after the other in the background
# while the e2e steps run (with -Sequential, here and before them).
$runner = Join-Path $Root 'scripts\corpus\run.sh'
# A Windows path is converted; anything else is taken as a path in WSL.
$privateManifest = if (-not $PrivateCorpus) { '' } elseif (Test-Path -LiteralPath $PrivateCorpus) { WslPath (Resolve-Path -LiteralPath $PrivateCorpus).Path } else { $PrivateCorpus }
$corpusSteps = @(
  [pscustomobject]@{
    Name = 'corpus'
    Skip = $(if (-not (Test-Path $runner)) { 'no corpus harness yet' })
    # The manifest is explicit so that a DVICODE_CORPUS_MANIFEST set in WSL does not replace it.
    Command = "cd '$RootWsl' && bash scripts/corpus/run.sh all --manifest scripts/corpus/manifest.json"
    Check = {
      param($Out, $Code)
      if ($Code -ne 0) { throw "corpus regression failed (exit $Code)" }
      'no regression against scripts/corpus/baseline.json'
    }
  }
  [pscustomobject]@{
    Name = 'corpus-private'
    Skip = $(if (-not $PrivateCorpus) { 'no private manifest (-PrivateCorpus or DVICODE_CORPUS_MANIFEST)' } elseif (-not (Test-Path $runner)) { 'no corpus harness yet' })
    Command = "cd '$RootWsl' && bash scripts/corpus/run.sh all --manifest '$privateManifest'"
    # The output names private projects and files: it goes to the console only, and the summary
    # keeps the counts.
    Check = {
      param($Out, $Code)
      $cmp = $Out | Where-Object { $_ -match '^compare: (\d+) results, (\d+) regressions' } | Select-Object -Last 1
      if (-not $cmp) { throw "private corpus failed before compare (exit $Code)" }
      $null = $cmp -match '^compare: (\d+) results, (\d+) regressions'
      $notes = @($Out | Where-Object { $_ -like 'note: *' }).Count
      $counts = "$($Matches[1]) configurations, $($Matches[2]) regressions, $notes notes"
      if ($Code -ne 0) { throw "private corpus: $counts" }
      $counts
    }
  }
)
$background = @()
foreach ($s in $corpusSteps) {
  if ($Sequential) {
    Step $s.Name {
      if ($s.Skip) { return "SKIP: $($s.Skip)" }
      $out = wsl.exe -d $Distro -e bash -lc $s.Command 2>&1 | ForEach-Object { $line = "$_"; Write-Host $line; $line }
      & $s.Check $out $LASTEXITCODE
    }
    continue
  }
  $row = NewRow $s.Name
  if ($Skip -contains $s.Name) { continue }
  if ($s.Skip) { $row.Detail = $s.Skip; continue }
  $background += [pscustomobject]@{
    Name = $s.Name; Command = $s.Command; Check = $s.Check; Row = $row
    Log = Join-Path $env:TEMP "dvicode-prerelease-$PID-$($s.Name).log"
    # The session of the step's processes in WSL, while it runs (see StopCorpus).
    Sid = "/tmp/dvicode-prerelease-$PID-$($s.Name).sid"
  }
}
$corpusJob = $null
if ($background) {
  Write-Host "`n=== $(($background | ForEach-Object Name) -join ', ') (in the background)" -ForegroundColor Cyan
  $background | ForEach-Object { Write-Host "$($_.Name): output in $($_.Log), shown after the e2e steps" }
  $corpusJob = Start-Job -ArgumentList $Distro, @($background | Select-Object Name, Command, Log, Sid) -ScriptBlock {
    param($Distro, $Steps)
    foreach ($s in $Steps) {
      $t0 = Get-Date
      wsl.exe -d $Distro -e setsid -w bash -lc "echo `$`$ >'$($s.Sid)'; $($s.Command) </dev/null; rc=`$?; rm -f '$($s.Sid)'; exit `$rc" 2>&1 |
        ForEach-Object { "$_" } | Set-Content -Encoding utf8 $s.Log
      [pscustomobject]@{ Name = $s.Name; Code = $LASTEXITCODE; Seconds = [int]((Get-Date) - $t0).TotalSeconds }
    }
  }
}

# Stops the background job and, in WSL, every process of the corpus step it runs (`timeout`
# gives each engine a process group of its own, but they all stay in the step's session).
function StopCorpus {
  $running = $corpusJob.State -eq 'Running'
  Stop-Job $corpusJob
  Remove-Job $corpusJob -Force
  if ($running) {
    # The step that was running writes its file once its login shell has started: wait for it
    # a little.
    $files = ($background | ForEach-Object { "'$($_.Sid)'" }) -join ' '
    wsl.exe -d $Distro -e sh -c "for i in 1 2 3 4 5 6 7 8 9 10; do ls $files 2>/dev/null | grep -q . && break; sleep 0.5; done; for f in $files; do [ -f `$f ] && pkill -TERM -s `$(cat `$f); rm -f `$f; done"
  }
  $background | ForEach-Object { Remove-Item $_.Log -ErrorAction SilentlyContinue }
  Write-Host 'corpus steps stopped' -ForegroundColor Yellow
}

# Interrupted (Ctrl+C), the corpus steps stop too.
try {
  # The extension under test: the sources (default) or an unpacked VSIX.
  if ($Vsix) {
    $unpacked = Join-Path $env:TEMP 'dvc-prerelease'
    if (Test-Path $unpacked) { Remove-Item -Recurse -Force $unpacked -Confirm:$false }
    New-Item -ItemType Directory $unpacked | Out-Null
    Copy-Item $Vsix (Join-Path $unpacked 'p.zip') -ErrorAction Stop
    Expand-Archive (Join-Path $unpacked 'p.zip') -DestinationPath $unpacked -ErrorAction Stop
    $env:DVICODE_E2E_EXTENSION = Join-Path $unpacked 'extension'
  } else {
    Remove-Item Env:DVICODE_E2E_EXTENSION -ErrorAction SilentlyContinue
  }

  Step 'e2e-native' {
    E2e $Demo 'native'
  }

  Step 'e2e-wsl' {
    $wslDemo = DemoToWsl
    $a = E2e $Demo 'wsl-drive' @{ 'dvicode.server.mode' = 'wsl' }
    $b = E2e "file://wsl.localhost/$Distro$wslDemo" 'wsl-unc'
    $c = E2e "dvicode://wsl+$Distro$wslDemo" 'remote-fs'
    "drive: $a; unc: $b; remote-fs: $c"
  }

  Step 'e2e-ssh' {
    DemoToSsh
    $key = $SshKey -replace '\\', '/'
    E2e "dvicode://ssh+$SshUser%40127.0.0.1/home/$SshUser/demo" 'ssh' @{
      'dvicode.server.sshArgs' = @('-p', "$SshPort", '-i', $key, '-o', 'StrictHostKeyChecking=accept-new')
    }
  }

  # VS Code's own remote windows: the extension is installed on the remote side, runs in the remote
  # extension host and starts the Linux engine there (mode local). Each run also has a local user
  # setting meant for Windows, which VS Code must keep out of the remote window.
  Step 'e2e-remote-wsl' {
    $wslDemo = DemoToWsl
    E2e $wslDemo 'remote-wsl' @{ 'dvicode.server.mode' = 'wsl' } -Remote "wsl+$Distro"
  }

  Step 'e2e-remote-ssh' {
    DemoToSsh
    # Remote - SSH and the test runner reach the host through this alias.
    $sshConfig = Join-Path $env:TEMP 'dvicode-e2e-ssh_config'
    @(
      'Host dvicode-e2e'
      '  HostName 127.0.0.1'
      "  Port $SshPort"
      "  User $SshUser"
      "  IdentityFile $($SshKey -replace '\\', '/')"
      '  IdentitiesOnly yes'
      '  StrictHostKeyChecking accept-new'
    ) | Set-Content -Encoding ascii $sshConfig
    E2e "/home/$SshUser/demo" 'remote-ssh' @{ 'dvicode.server.mode' = 'ssh'; 'dvicode.server.sshHost' = 'not-this-host.invalid' } -Remote 'ssh-remote+dvicode-e2e' -SshConfig $sshConfig
  }

  # Other editors built on VS Code (installed from Open VSX by their users): the setups that use
  # DVICode's own engine modes (this machine, WSL, its dvicode:// folders, SSH), not VS Code's
  # remote windows. Each editor found at its usual place (or in $env:DVICODE_E2E_EDITORS, a
  # ';'-separated list of executables) runs them; none found is a SKIP.
  Step 'e2e-editors' {
    $candidates = @(
      "$env:USERPROFILE\.dvicode-test\vscodium\VSCodium.exe",
      "$env:LOCALAPPDATA\Programs\VSCodium\VSCodium.exe",
      "$env:LOCALAPPDATA\Programs\cursor\Cursor.exe",
      "$env:LOCALAPPDATA\Programs\Windsurf\Windsurf.exe"
    )
    if ($env:DVICODE_E2E_EDITORS) { $candidates = $env:DVICODE_E2E_EDITORS -split ';' }
    $editors = @($candidates | Where-Object { $_ -and (Test-Path $_) })
    if (-not $editors) { return 'SKIP: no VSCodium, Cursor or Windsurf installed' }
    $wslDemo = DemoToWsl
    DemoToSsh
    $key = $SshKey -replace '\\', '/'
    $out = @()
    $failures = @()
    try {
      foreach ($exe in $editors) {
        $name = [IO.Path]::GetFileNameWithoutExtension($exe).ToLower()
        $version = (Get-Item $exe).VersionInfo.ProductVersion
        $env:VSCODE_EXECUTABLE = $exe
        # One profile per editor, kept: sign in to Cursor once in it and later runs stay signed in.
        $env:DVICODE_E2E_USER_DATA = "$env:USERPROFILE\.dvicode-test\profile-$name"
        # An editor still open on that profile (after signing in, say) would take the test window
        # over and give no result: close it. Only processes on this test profile are stopped.
        Get-CimInstance Win32_Process -Filter "Name='$([IO.Path]::GetFileName($exe))'" |
          Where-Object { $_.CommandLine -like "*$env:DVICODE_E2E_USER_DATA*" } |
          ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
        $runs = @()
        foreach ($run in @(
            @{ Target = $Demo; Label = "$name-native"; Settings = @{} },
            @{ Target = $Demo; Label = "$name-wsl"; Settings = @{ 'dvicode.server.mode' = 'wsl' } },
            @{ Target = "dvicode://wsl+$Distro$wslDemo"; Label = "$name-remote-fs"; Settings = @{} },
            @{ Target = "dvicode://ssh+$SshUser%40127.0.0.1/home/$SshUser/demo"; Label = "$name-ssh"; Settings = @{ 'dvicode.server.sshArgs' = @('-p', "$SshPort", '-i', $key, '-o', 'StrictHostKeyChecking=accept-new') } }
          )) {
          try {
            $runs += "$($run.Label): $(E2e $run.Target $run.Label $run.Settings)"
          } catch {
            $failures += $_.Exception.Message
          }
        }
        $out += "$name $version ($($runs.Count)/4 setups)"
      }
    } finally {
      Remove-Item Env:VSCODE_EXECUTABLE -ErrorAction SilentlyContinue
      Remove-Item Env:DVICODE_E2E_USER_DATA -ErrorAction SilentlyContinue
    }
    if ($failures) { throw (($out + $failures) -join '; ') }
    $out -join '; '
  }

  if ($corpusJob) {
    Write-Host "`n=== waiting for $(($background | ForEach-Object Name) -join ', ')" -ForegroundColor Cyan
    $ended = @(Receive-Job $corpusJob -Wait -AutoRemoveJob)
    $corpusJob = $null
    foreach ($s in $background) {
      Write-Host "`n=== $($s.Name) (ran in the background)" -ForegroundColor Cyan
      $out = @()
      if (Test-Path $s.Log) {
        $out = @(Get-Content -Encoding utf8 $s.Log)
        Remove-Item $s.Log
      }
      $out | ForEach-Object { Write-Host $_ }
      $end = $ended | Where-Object { $_.Name -eq $s.Name } | Select-Object -First 1
      Finish $s.Row {
        if (-not $end) { throw 'the background job did not run it' }
        & $s.Check $out $end.Code
      }
      if ($end) { $s.Row.Seconds = $end.Seconds }
    }
  }
} finally {
  if ($corpusJob) { StopCorpus }
}

Write-Host "`n=== Summary" -ForegroundColor Cyan
$results | Format-Table -AutoSize -Wrap | Out-String -Width 200 | Write-Host
$failed = @($results | Where-Object { $_.Result -eq 'FAIL' }).Count
if ($failed) {
  Write-Host "$failed step(s) failed" -ForegroundColor Red
  exit 1
}
Write-Host 'All steps passed' -ForegroundColor Green
