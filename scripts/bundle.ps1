# Builds lithe.exe, and optionally installs it.
#
#   .\scripts\bundle.ps1              -> target\release\lithe.exe
#   .\scripts\bundle.ps1 -Install     -> %LOCALAPPDATA%\Programs\lithe, with
#                                        a Start menu shortcut
#
# Windows has no bundle format: an application is an .exe, and what makes it
# look like one rather than like a loose binary is the icon and the version
# information compiled into it. That happens in crates/app/build.rs during
# the build, which is why the icon is drawn here first — it has to exist
# before cargo starts.
param([switch]$Install)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# The icon is generated rather than committed as a binary blob. A machine
# without Python still gets an editor, just one Explorer draws with the
# generic icon, so this is a warning rather than a failure.
if (-not (Test-Path "$root\assets\lithe.ico")) {
    try { python "$root\scripts\make-icon.py" --ico | Out-Null }
    catch { Write-Warning "could not draw the icon: $_" }
}

cargo build --release --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$exe = "$root\target\release\lithe.exe"

if (-not $Install) {
    $exe
    return
}

# Per-user, under Programs, which is where an application that was not
# installed by an administrator belongs — no elevation, nothing in
# Program Files, nothing to uninstall but a folder.
$home_dir = "$env:LOCALAPPDATA\Programs\lithe"
New-Item -ItemType Directory -Force -Path $home_dir | Out-Null
Copy-Item -Force $exe "$home_dir\lithe.exe"

$shortcut = [Environment]::GetFolderPath('Programs') + '\lithe.lnk'
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($shortcut)
$link.TargetPath = "$home_dir\lithe.exe"
$link.WorkingDirectory = $home_dir
$link.Description = 'A bare-bones code editor'
$link.Save()

"$home_dir\lithe.exe"
"$shortcut"
