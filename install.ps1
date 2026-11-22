[CmdletBinding()]
param(
    [string]$Version = $env:BATON_VERSION,
    [string]$InstallDir = $env:BATON_INSTALL_DIR
)

$ErrorActionPreference = "Stop"
$Repo = if ($env:BATON_REPO) { $env:BATON_REPO } else { "billy-enrizky/baton" }
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = "latest" }

$architecture = $env:PROCESSOR_ARCHITECTURE
try {
    $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
} catch {
}
if ($architecture -notin @("X64", "AMD64")) {
    throw "Baton currently publishes x64 Windows binaries; found $architecture."
}

$Target = "x86_64-pc-windows-msvc"
$Asset = "baton-$Target.zip"
if ($Version -eq "latest") {
    $BaseUrl = "https://github.com/$Repo/releases/latest/download"
} else {
    if (-not $Version.StartsWith("v")) { $Version = "v$Version" }
    $BaseUrl = "https://github.com/$Repo/releases/download/$Version"
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:LOCALAPPDATA "Baton\bin"
}

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("baton-install-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TempDir | Out-Null
try {
    $Archive = Join-Path $TempDir $Asset
    $Sums = Join-Path $TempDir "SHA256SUMS"
    $Downloaded = $false
    Write-Host "installing Baton for $Target"
    try {
        Invoke-WebRequest -Uri "$BaseUrl/$Asset" -OutFile $Archive -UseBasicParsing
        Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS" -OutFile $Sums -UseBasicParsing
        $Downloaded = $true
    } catch {
        Write-Warning "no prebuilt release for $Target; trying a source build"
        $Cargo = Get-Command cargo -ErrorAction SilentlyContinue
        if (-not $Cargo) {
            throw "Install Rust from https://rustup.rs, then run this script again."
        }
        $Prefix = Split-Path $InstallDir -Parent
        & cargo install --git "https://github.com/$Repo" --locked --root $Prefix
        if ($LASTEXITCODE -ne 0) { throw "cargo install failed" }
        $Binary = Join-Path $InstallDir "baton.exe"
        if (-not (Test-Path $Binary)) { throw "cargo finished without installing $Binary" }
    }

    if ($Downloaded) {
        $Expected = $null
        foreach ($Line in Get-Content $Sums) {
            $Fields = $Line.Trim() -split "\s+"
            $Name = if ($Fields.Count -ge 2) { $Fields[1] -replace "^\./", "" } else { "" }
            if ($Name -eq $Asset) {
                $Expected = $Fields[0].ToLowerInvariant()
                break
            }
        }
        if (-not $Expected) { throw "$Asset is missing from SHA256SUMS" }
        $Actual = (Get-FileHash -Algorithm SHA256 -Path $Archive).Hash.ToLowerInvariant()
        if ($Actual -ne $Expected) { throw "checksum mismatch for $Asset" }

        Expand-Archive -Path $Archive -DestinationPath $TempDir -Force
        $Source = Join-Path $TempDir "baton.exe"
        if (-not (Test-Path $Source)) { throw "baton.exe is missing from $Asset" }
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        Copy-Item -Force $Source (Join-Path $InstallDir "baton.exe")
    }

    $Installed = Join-Path $InstallDir "baton.exe"
    & $Installed --version
    if ($LASTEXITCODE -ne 0) { throw "installed baton did not run" }

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $Parts = @()
    if (-not [string]::IsNullOrWhiteSpace($UserPath)) {
        $Parts = $UserPath -split ";"
    }
    if ($Parts -notcontains $InstallDir) {
        $NewPath = if ([string]::IsNullOrWhiteSpace($UserPath)) { $InstallDir } else { "$UserPath;$InstallDir" }
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        Write-Host "Added $InstallDir to the user PATH. Open a new terminal to use it."
    }
    $env:Path = "$InstallDir;$env:Path"
    Write-Host ""
    Write-Host "Baton is installed at $Installed"
    Write-Host "Harness CLIs you want to drive still need to be on PATH. Run: baton doctor"
} finally {
    Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
}
