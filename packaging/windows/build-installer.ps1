# Builds target\dist\installer\QuarrySetup-<version>-x64.exe.
# Needs Rust (MSVC toolchain) and Inno Setup 6 (winget install JRSoftware.InnoSetup).
# Native tools write progress to stderr, so failures are checked by exit code
# rather than with $ErrorActionPreference = 'Stop' (which PowerShell 5.1 trips on).
$root = (Resolve-Path "$PSScriptRoot\..\.." -ErrorAction Stop).Path
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "(.+)"' -ErrorAction Stop).Matches[0].Groups[1].Value

# Static C runtime, so target machines need no Visual C++ Redistributable.
# A separate target dir keeps these flags from invalidating normal builds.
$env:RUSTFLAGS = '-C target-feature=+crt-static'
Push-Location $root
try {
    cargo build --release -p quarry-app -p quarry-ai --target-dir "$root\target\dist"
    if ($LASTEXITCODE) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
    Remove-Item Env:RUSTFLAGS
}

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup' }

& $iscc /Q "/DAppVersion=$version" "/DBinDir=$root\target\dist\release" "$PSScriptRoot\quarry.iss"
if ($LASTEXITCODE) { throw "ISCC failed ($LASTEXITCODE)" }
Get-Item "$root\target\dist\installer\QuarrySetup-$version-x64.exe"
