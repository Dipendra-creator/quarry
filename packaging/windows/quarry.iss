; Inno Setup script for the Quarry Windows installer.
; Build with packaging\windows\build-installer.ps1, which passes AppVersion and BinDir.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\dist\release"
#endif
#ifndef AppArch
  #define AppArch "x64"
#endif
#if AppArch == "arm64"
  #define SetupArch "arm64"
  #define MinimumWindows "10.0.22000"
#else
  #define SetupArch "x64compatible"
  #define MinimumWindows "10.0.17763"
#endif

[Setup]
; Never change AppId: upgrades and uninstall find earlier installs by it.
AppId={{1A8FCE09-A187-4A6A-BADC-C82B9C8AC054}
AppName=Quarry
AppVersion={#AppVersion}
AppVerName=Quarry {#AppVersion}
AppPublisher=Quarry contributors
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\Quarry
DefaultGroupName=Quarry
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir=..\..\target\dist\installer
OutputBaseFilename=QuarrySetup-{#AppVersion}-{#AppArch}
SetupIconFile=..\..\crates\quarry-app\resources\quarry.ico
UninstallDisplayIcon={app}\quarry.exe
UninstallDisplayName=Quarry
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed={#SetupArch}
ArchitecturesInstallIn64BitMode={#SetupArch}
; Installs for the current user without admin rights; the dialog offers all users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; Windows 10 1809 or later (DirectComposition and system ICU used by GPUI).
MinVersion={#MinimumWindows}
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\quarry.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\quarry-mcp.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\..\README.md"; DestDir: "{app}"
Source: "..\..\docs\AI.md"; DestDir: "{app}\docs"
Source: "..\..\docs\ARCHITECTURE.md"; DestDir: "{app}\docs"

[Icons]
Name: "{autoprograms}\Quarry"; Filename: "{app}\quarry.exe"; AppUserModelID: "io.github.dipendra-creator.quarry"
Name: "{autodesktop}\Quarry"; Filename: "{app}\quarry.exe"; Tasks: desktopicon; AppUserModelID: "io.github.dipendra-creator.quarry"

[Run]
Filename: "{app}\quarry.exe"; Description: "{cm:LaunchProgram,Quarry}"; Flags: nowait postinstall skipifsilent

; Saved connections in %APPDATA%\Quarry and passwords in Credential Manager are
; kept on uninstall, like the original app keeps its data.
