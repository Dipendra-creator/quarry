# Builds target\dist\installer\QuarrySetup-<version>-<architecture>.exe.
# Needs Rust (MSVC toolchain) and Inno Setup 6 (winget install JRSoftware.InnoSetup).
# Native tools write progress to stderr, so failures are checked by exit code
# rather than with $ErrorActionPreference = 'Stop' (which PowerShell 5.1 trips on).
param([ValidateSet('x64', 'arm64')][string]$Architecture)

$rustHost = ((rustc -vV | Select-String '^host: ').Line -replace '^host: ', '').Trim()
if ($LASTEXITCODE) { throw 'Could not determine the Rust host architecture' }
$nativeArchitecture = if ($rustHost -eq 'aarch64-pc-windows-msvc') { 'arm64' } elseif ($rustHost -eq 'x86_64-pc-windows-msvc') { 'x64' } else { throw "Unsupported Windows Rust host: $rustHost" }
if (-not $Architecture) { $Architecture = $nativeArchitecture }
if ($Architecture -ne $nativeArchitecture) { throw "Requested $Architecture, but the active Rust toolchain builds $nativeArchitecture" }
$root = (Resolve-Path "$PSScriptRoot\..\.." -ErrorAction Stop).Path
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "(.+)"' -ErrorAction Stop).Matches[0].Groups[1].Value

# Static C runtime, so target machines need no Visual C++ Redistributable.
# A separate target dir keeps these flags from invalidating normal builds.
$previousRustFlags = $env:RUSTFLAGS
$env:RUSTFLAGS = '-C target-feature=+crt-static'
Push-Location $root
try {
    cargo build --release --locked -p quarry-app -p quarry-ai --target-dir "$root\target\dist"
    if ($LASTEXITCODE) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
    if ($null -eq $previousRustFlags) { Remove-Item Env:RUSTFLAGS } else { $env:RUSTFLAGS = $previousRustFlags }
}

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup' }

& $iscc /Q "/DAppVersion=$version" "/DAppArch=$Architecture" "/DBinDir=$root\target\dist\release" "$PSScriptRoot\quarry.iss"
if ($LASTEXITCODE) { throw "ISCC failed ($LASTEXITCODE)" }
Get-Item "$root\target\dist\installer\QuarrySetup-$version-$Architecture.exe"
