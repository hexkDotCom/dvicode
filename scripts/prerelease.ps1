<#
.SYNOPSIS
  Runs every check before a DVICode release on a Windows machine with WSL.

.DESCRIPTION
  Steps, in order (skip any with -Skip):
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

  Without -Vsix the end-to-end tests run the extension from client/ with the engines built here.
  With -Vsix they run a packaged extension (the universal package of a Release workflow run).

  Prerequisites (see docs/development.md, "Before a release"): Rust and the MSVC build tools on
  Windows, Rust and Python in the WSL distribution, Node.js, VS Code, and for e2e-ssh a WSL
  distribution with sshd configured on -SshPort for key login with -SshKey.

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
  [string]$SshKey = "$env:LOCALAPPDATA\dvicode-test\e2e_key"
)

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

# Runs one step, records PASS / FAIL / SKIP with its time and a short detail, and carries on.
function Step([string]$Name, [scriptblock]$Body) {
  if ($Skip -contains $Name) {
    [void]$results.Add([pscustomobject]@{ Step = $Name; Result = 'SKIP'; Seconds = 0; Detail = '' })
    return
  }
  Write-Host "`n=== $Name" -ForegroundColor Cyan
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
  $result = if (-not $ok) { 'FAIL' } elseif ("$detail" -like 'SKIP:*') { 'SKIP' } else { 'PASS' }
  [void]$results.Add([pscustomobject]@{
      Step = $Name; Result = $result
      Seconds = [int]((Get-Date) - $t0).TotalSeconds; Detail = ("$detail" -replace '^SKIP:\s*', '')
    })
}

# Runs a native command, echoing its output; throws when it fails.
function Run([string]$What, [scriptblock]$Cmd) {
  & $Cmd 2>&1 | ForEach-Object { "$_" } | Out-Host
  if ($LASTEXITCODE -ne 0) { throw "$What failed (exit $LASTEXITCODE)" }
}

function WslPath([string]$WinPath, [string]$D = $Distro) {
  (wsl.exe -d $D -e wslpath -u ($WinPath -replace '\\', '/')).Trim()
}

# One end-to-end run: folder or URI, label, settings; returns the "N/N passed" line.
function E2e([string]$Target, [string]$Label, [hashtable]$Settings = @{}) {
  $env:DVICODE_E2E_SETTINGS = ($Settings | ConvertTo-Json -Compress)
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
  $out | Where-Object { $_ -match '\] (FAIL|PASS)|passed;' } | Out-Host
  $summary = ($out | Where-Object { $_ -match 'passed;' } | Select-Object -Last 1)
  if ($code -ne 0) { throw "$Label failed: $summary" }
  return ($summary -replace '^\[[^\]]+\]\s*', '')
}

$RootWsl = WslPath $Root

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

Step 'corpus' {
  $runner = Join-Path $Root 'scripts\corpus\run.sh'
  if (-not (Test-Path $runner)) { return 'SKIP: no corpus harness yet' }
  # Fetches the pinned projects, builds the engine, checks, sweeps the IDE queries, compares.
  # The manifest is explicit so that a DVICODE_CORPUS_MANIFEST set in WSL does not replace it.
  Run 'corpus regression' { wsl.exe -d $Distro -e bash -lc "cd '$RootWsl' && bash scripts/corpus/run.sh all --manifest scripts/corpus/manifest.json" }
  'no regression against scripts/corpus/baseline.json'
}

Step 'corpus-private' {
  if (-not $PrivateCorpus) { return 'SKIP: no private manifest (-PrivateCorpus or DVICODE_CORPUS_MANIFEST)' }
  if (-not (Test-Path (Join-Path $Root 'scripts\corpus\run.sh'))) { return 'SKIP: no corpus harness yet' }
  # A Windows path is converted; anything else is taken as a path in WSL.
  $manifest = if (Test-Path -LiteralPath $PrivateCorpus) { WslPath (Resolve-Path -LiteralPath $PrivateCorpus).Path } else { $PrivateCorpus }
  # The output names private projects and files: it goes to the console only, and the summary
  # keeps the counts.
  $out = wsl.exe -d $Distro -e bash -lc "cd '$RootWsl' && bash scripts/corpus/run.sh all --manifest '$manifest'" 2>&1 |
    ForEach-Object { $s = "$_"; Write-Host $s; $s }
  $code = $LASTEXITCODE
  $cmp = $out | Where-Object { $_ -match '^compare: (\d+) results, (\d+) regressions' } | Select-Object -Last 1
  if (-not $cmp) { throw "private corpus failed before compare (exit $code)" }
  $null = $cmp -match '^compare: (\d+) results, (\d+) regressions'
  $notes = @($out | Where-Object { $_ -like 'note: *' }).Count
  $counts = "$($Matches[1]) configurations, $($Matches[2]) regressions, $notes notes"
  if ($code -ne 0) { throw "private corpus: $counts" }
  $counts
}

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
  $wslDemo = (wsl.exe -d $Distro -e sh -c 'echo $HOME/dvicode-demo').Trim()
  $demoWsl = WslPath $Demo
  Run 'copy the demo into WSL' { wsl.exe -d $Distro -e bash -c "rm -rf '$wslDemo' && mkdir -p '$wslDemo' && cp -r '$demoWsl/.' '$wslDemo/'" }
  $a = E2e $Demo 'wsl-drive' @{ 'dvicode.server.mode' = 'wsl' }
  $b = E2e "file://wsl.localhost/$Distro$wslDemo" 'wsl-unc'
  $c = E2e "dvicode://wsl+$Distro$wslDemo" 'remote-fs'
  "drive: $a; unc: $b; remote-fs: $c"
}

Step 'e2e-ssh' {
  if (-not (Test-Path $SshKey)) { throw "no SSH test key at $SshKey (see docs/development.md)" }
  Run 'start sshd' { wsl.exe -d $SshDistro -u root -e sh -c 'mkdir -p /run/sshd; pgrep -f sshd_dvicode.conf >/dev/null || /usr/sbin/sshd -f /etc/ssh/sshd_dvicode.conf' }
  $sshOpts = @('-p', "$SshPort", '-i', $SshKey, '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=accept-new')
  Push-Location (Split-Path $Demo)
  try {
    cmd /c "tar -cf - demo-project | ssh $($sshOpts -join ' ') $SshUser@127.0.0.1 ""rm -rf ~/demo && mkdir -p ~/demo && tar -C ~/demo --strip-components=1 -xf -"""
    if ($LASTEXITCODE -ne 0) { throw "copying the demo to the SSH host failed (exit $LASTEXITCODE)" }
  } finally { Pop-Location }
  $key = $SshKey -replace '\\', '/'
  E2e "dvicode://ssh+$SshUser%40127.0.0.1/home/$SshUser/demo" 'ssh' @{
    'dvicode.server.sshArgs' = @('-p', "$SshPort", '-i', $key, '-o', 'StrictHostKeyChecking=accept-new')
  }
}

Write-Host "`n=== Summary" -ForegroundColor Cyan
$results | Format-Table -AutoSize -Wrap | Out-String -Width 200 | Write-Host
$failed = @($results | Where-Object { $_.Result -eq 'FAIL' }).Count
if ($failed) {
  Write-Host "$failed step(s) failed" -ForegroundColor Red
  exit 1
}
Write-Host 'All steps passed' -ForegroundColor Green
