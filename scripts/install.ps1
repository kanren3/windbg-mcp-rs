<#
.SYNOPSIS
    Installs the windbg-mcp-rs extension into all discovered WinDbg installations.

.DESCRIPTION
    By default, downloads the latest release DLL and manifest from GitHub Releases.
    Searches for WinDbg (SDK Debuggers x64/arm64 and WinDbg Store), copies
    windbg_mcp_rs.dll into winext\, and places windbg_mcp_rs_GalleryManifest.xml
    into OptionalExtensions\ for automatic loading when WinDbg starts.

.PARAMETER LocalPath
    Install from a local build (e.g. "..\target\release\") instead of downloading.
    The directory must contain windbg_mcp_rs.dll. The manifest is auto-located
    from the project root if not found alongside the DLL.

.PARAMETER Version
    Specific release version to download (e.g. "0.1.2"). Defaults to latest.

.PARAMETER DryRun
    Preview mode — only reports what would be installed without making changes.

.EXAMPLE
    .\install.ps1
    Downloads the latest release from GitHub and installs to all WinDbg locations.

.EXAMPLE
    .\install.ps1 -LocalPath ..\target\x86_64-pc-windows-msvc\release
    Installs from a local release build (for development).

.EXAMPLE
    .\install.ps1 -Version 0.1.0
    Installs a specific release version from GitHub.

.EXAMPLE
    .\install.ps1 -DryRun
    Preview what would happen without making changes.
#>

[CmdletBinding()]
param(
    [string]$LocalPath,
    [string]$Version,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoOwner = "kanren3"
$RepoName  = "windbg-mcp-rs"
$TempDir   = Join-Path $env:TEMP "windbg-mcp-rs-install"

# ── Step 1: Discover WinDbg installations ──────────────────────────

function Find-WinDbgInstallations {
    $found = [System.Collections.Generic.List[hashtable]]::new()
    $seen  = [System.Collections.Generic.HashSet[string]]::new()

    # SDK Debuggers: C:\Program Files (x86)\Windows Kits\10\Debuggers\<arch>
    $sdkRoot = "${env:ProgramFiles(x86)}\Windows Kits\10\Debuggers"
    if (Test-Path $sdkRoot) {
        Get-ChildItem $sdkRoot -Directory | ForEach-Object {
            if ($_.Name -eq "x86") { return }
            if (Test-Path (Join-Path $_.FullName "dbgeng.dll")) {
                $normalized = (Resolve-Path $_.FullName).Path
                if ($seen.Add($normalized)) {
                    $found.Add(@{ Path = $normalized; Arch = $_.Name; Label = "SDK Debuggers ($($_.Name))"; Type = "SDK" })
                }
            }
        }
    }

    # Registry
    $regPaths = @(
        "HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots"
    )
    foreach ($regPath in $regPaths) {
        if (Test-Path $regPath) {
            $kitsRoot = (Get-ItemProperty -Path $regPath -Name "KitsRoot10" -ErrorAction SilentlyContinue).KitsRoot10
            if ($kitsRoot) {
                $dbgDir = Join-Path $kitsRoot "Debuggers"
                if (Test-Path $dbgDir) {
                    Get-ChildItem $dbgDir -Directory | ForEach-Object {
                        if ($_.Name -eq "x86") { return }
                        if (Test-Path (Join-Path $_.FullName "dbgeng.dll")) {
                            $normalized = (Resolve-Path $_.FullName).Path
                            if ($seen.Add($normalized)) {
                                $found.Add(@{ Path = $normalized; Arch = $_.Name; Label = "SDK Debuggers [registry] ($($_.Name))"; Type = "SDK" })
                            }
                        }
                    }
                }
            }
        }
    }

    # WinDbg (Store) user extensions
    $previewBase = Join-Path $env:LOCALAPPDATA "DBG"
    $normalized = $previewBase
    if ($seen.Add($normalized)) {
        # Detect arch from Store app directory name (read-only)
        $previewArch = "x64"  # default
        $winApps = "${env:ProgramFiles}\WindowsApps"
        if (Test-Path $winApps) {
            $pkg = Get-ChildItem $winApps -Directory -Filter "Microsoft.WinDbg_*" -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($pkg) {
                if ($pkg.Name -match "_arm64")     { $previewArch = "arm64" }
                elseif ($pkg.Name -match "_x64")   { $previewArch = "x64" }
            }
        }
        $found.Add(@{ Path = $normalized; Arch = $previewArch; Label = "WinDbg (Store, $previewArch)"; Type = "Store" })
    }

    return $found

}

$installations = Find-WinDbgInstallations

if ($installations.Count -eq 0) {
    Write-Host "No WinDbg installations found." -ForegroundColor Yellow
    Write-Host ""
    Write-Host "Manually install with:" -ForegroundColor Gray
    Write-Host '  copy windbg_mcp_rs.dll <windbg>\winext\' -ForegroundColor Gray
    Write-Host '  copy windbg_mcp_rs_GalleryManifest.xml <windbg>\OptionalExtensions\' -ForegroundColor Gray
    exit 1
}

Write-Host "Found $($installations.Count) WinDbg installation(s):" -ForegroundColor Green
foreach ($inst in $installations) {
    Write-Host "  $($inst.Label)" -ForegroundColor Gray
}
Write-Host ""

# ── Step 2: Acquire DLLs per architecture ──────────────────────────

# Collect unique architectures across all installations
$archs = $installations | ForEach-Object { $_.Arch } | Select-Object -Unique
$dllByArch = @{}

if ($LocalPath) {
    Write-Host "=== windbg-mcp-rs Installer (local) ===" -ForegroundColor Cyan
    $LocalPath = Resolve-Path $LocalPath -ErrorAction Stop

    # Manifest is in the project root (one level above scripts/)
    $manifestPath = Join-Path $ScriptDir "..\windbg_mcp_rs_GalleryManifest.xml"
    if (-not (Test-Path $manifestPath)) {
        $manifestPath = Join-Path $LocalPath "windbg_mcp_rs_GalleryManifest.xml"
    }
    if (-not (Test-Path $manifestPath)) { throw "Manifest not found" }
    $manifestPath = Resolve-Path $manifestPath -ErrorAction Stop

    # Auto-discover: scan target/<triple>/release/windbg_mcp_rs.dll
    # Map Rust target triples to WinDbg arch names
    $archMap = @{
        "x86_64-pc-windows-msvc"   = "x64"
        "aarch64-pc-windows-msvc"  = "arm64"
        "i686-pc-windows-msvc"     = "x86"
    }

    $foundArchs = @{}
    if (Test-Path (Join-Path $LocalPath "windbg_mcp_rs.dll")) {
        # Flat layout (e.g. target/release/) — assume x64
        $foundArchs["x64"] = Join-Path $LocalPath "windbg_mcp_rs.dll"
    } else {
        # Matrix layout: target/<triple>/release/
        foreach ($triple in $archMap.Keys) {
            $candidate = Join-Path $LocalPath "$triple\release\windbg_mcp_rs.dll"
            if (Test-Path $candidate) {
                $foundArchs[$archMap[$triple]] = $candidate
            }
        }
    }

    if ($foundArchs.Count -eq 0) {
        $searched = Join-Path $LocalPath "*\release\windbg_mcp_rs.dll"
        throw "No DLL found. Expected at target/<triple>/release/windbg_mcp_rs.dll (e.g. target/x86_64-pc-windows-msvc/release/windbg_mcp_rs.dll)"
    }

    Write-Host "  Manifest: $manifestPath"
    foreach ($a in $foundArchs.Keys | Sort-Object) {
        Write-Host ("  {0,-5} DLL: {1}" -f $a, $foundArchs[$a]) -ForegroundColor Gray
        $dllByArch[$a] = @{ Dll = $foundArchs[$a]; Manifest = $manifestPath }
    }
} else {
    # Download from GitHub Releases
    Write-Host "=== windbg-mcp-rs Installer ===" -ForegroundColor Cyan
    Write-Host "Fetching release info..." -ForegroundColor Gray

    $ReleaseUrl = if ($Version) {
        "https://api.github.com/repos/$RepoOwner/$RepoName/releases/tags/v$Version"
    } else {
        "https://api.github.com/repos/$RepoOwner/$RepoName/releases/latest"
    }

    try {
        $Release = Invoke-RestMethod -Uri $ReleaseUrl -ErrorAction Stop
    } catch {
        if ($_.Exception.Response.StatusCode -eq 404) {
            throw "Release not found. Check the version tag (v$Version) or try without -Version for latest."
        }
        throw "Failed to fetch release: $_"
    }

    if (-not $Version) { $Version = $Release.tag_name -replace '^v', '' }
    Write-Host "  Version: v$Version" -ForegroundColor Green

    New-Item -ItemType Directory -Path $TempDir -Force | Out-Null

    foreach ($arch in $archs) {
        Write-Host "  Acquiring $arch DLL..." -ForegroundColor Gray

        # Find matching zip: windbg-mcp-rs-v0.1.2-windows-x64.zip or *-windows-arm64.zip
        $zipPattern = "*-windows-$arch.zip"
        $ZipAsset = $Release.assets | Where-Object {
            $_.name -like $zipPattern
        } | Select-Object -First 1

        if (-not $ZipAsset) {
            Write-Host "    [!] No $arch zip found in release — skipping $arch" -ForegroundColor Yellow
            continue
        }

        $archTemp = Join-Path $TempDir $arch
        New-Item -ItemType Directory -Path $archTemp -Force | Out-Null

        $zipPath = Join-Path $archTemp "release.zip"
        Invoke-WebRequest -Uri $ZipAsset.browser_download_url -OutFile $zipPath
        Expand-Archive -Path $zipPath -DestinationPath $archTemp -Force

        $archDll = Get-ChildItem $archTemp -Recurse -Filter "windbg_mcp_rs.dll" | Select-Object -First 1 -ExpandProperty FullName
        if (-not $archDll) {
            Write-Host "    [!] No DLL in $arch zip — skipping $arch" -ForegroundColor Yellow
            continue
        }

        $archManifest = Get-ChildItem $archTemp -Recurse -Filter "windbg_mcp_rs_GalleryManifest.xml" | Select-Object -First 1 -ExpandProperty FullName
        if (-not $archManifest) {
            Write-Host "    Manifest not in zip — downloading from main branch..." -ForegroundColor Yellow
            $archManifest = Join-Path $archTemp "windbg_mcp_rs_GalleryManifest.xml"
            Invoke-WebRequest -Uri "https://raw.githubusercontent.com/$RepoOwner/$RepoName/main/windbg_mcp_rs_GalleryManifest.xml" -OutFile $archManifest
        }

        $dllByArch[$arch] = @{ Dll = $archDll; Manifest = $archManifest }
        Write-Host "    [+] $arch ready" -ForegroundColor Green
    }
}

if ($dllByArch.Count -eq 0) {
    throw "No compatible DLLs acquired. Checked archs: $($archs -join ', ')"
}

Write-Host ""

if ($DryRun) {
    Write-Host "[DRY RUN] No changes made. Remove -DryRun to install." -ForegroundColor Yellow
    exit 0
}

# ── Step 3: Install to each discovered location ─────────────────────

$installed = 0
$failed    = 0

$needsAdmin = $installations | Where-Object {
    $_.Path.StartsWith(${env:ProgramFiles}, [StringComparison]::OrdinalIgnoreCase) -or
    $_.Path.StartsWith(${env:ProgramFiles(x86)}, [StringComparison]::OrdinalIgnoreCase)
}
if ($needsAdmin) {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Write-Host "Some paths need Administrator. Re-run as admin." -ForegroundColor Yellow
        Write-Host ""
    }
}

foreach ($inst in $installations) {
    $arch    = $inst.Arch
    $files   = $dllByArch[$arch]
    if (-not $files) {
        Write-Host "  [!] $($inst.Label): no DLL for arch '$arch'" -ForegroundColor Yellow
        $failed++
        continue
    }

    $targetPath = $inst.Path
    $isStore = ($inst.Type -eq "Store")

    if ($isStore) {
        # WinDbg Store: DLL → EngineExtensions\, Manifest → ExtRepository\
        $dllDir = Join-Path $targetPath "EngineExtensions"
        $manifestDir = Join-Path $targetPath "ExtRepository"
    } else {
        # SDK Debuggers: DLL → winext\, Manifest → OptionalExtensions\
        $dllDir = Join-Path $targetPath "winext"
        $manifestDir = Join-Path $targetPath "OptionalExtensions"
    }

    try {
        New-Item -ItemType Directory -Path $dllDir -Force | Out-Null
        New-Item -ItemType Directory -Path $manifestDir -Force | Out-Null
    } catch {
        Write-Host "  [!] $($inst.Label): cannot create dirs" -ForegroundColor Red
        $failed++
        continue
    }

    try {
        Copy-Item -Path $files.Dll -Destination (Join-Path $dllDir "windbg_mcp_rs.dll") -Force
    } catch {
        Write-Host "  [!] $($inst.Label): DLL failed — $($_.Exception.Message)" -ForegroundColor Red
        $failed++
        continue
    }

    # Copy manifest (SDK format, only for SDK Debuggers)
    if (-not $isStore) {
        try {
            Copy-Item -Path $files.Manifest -Destination (Join-Path $manifestDir "windbg_mcp_rs_GalleryManifest.xml") -Force
        } catch {
            Write-Host "  [!] $($inst.Label): Manifest failed — $($_.Exception.Message)" -ForegroundColor Red
            $failed++
            continue
        }
    }

    # Store: install gallery files into isolated subdirectory
    if ($isStore) {
        $galleryDir = Join-Path $manifestDir "windbg-mcp-rs"
        try {
            New-Item -ItemType Directory -Path $galleryDir -Force | Out-Null

            # Read manifest and rewrite <File> to absolute DLL path
            $manifestContent = Get-Content -Path $files.Manifest -Raw
            $dllAbsPath = Join-Path $dllDir "windbg_mcp_rs.dll"
            $manifestContent = $manifestContent -replace `
                '<File Architecture="Any" Module="winext\\windbg_mcp_rs.dll" FilePathKind="RepositoryRelative" />',
                "<File Architecture=`"Any`" Module=`"$dllAbsPath`" FilePathKind=`"Absolute`" />"

            Set-Content -Path (Join-Path $galleryDir "manifest.1.xml") -Value $manifestContent

            # ManifestVersion.txt
            $verFile = Join-Path $galleryDir "ManifestVersion.txt"
            Set-Content -Path $verFile -Value "1`r`n1.0.0.0`r`n1"

            # config.xml
            $configPath = Join-Path $galleryDir "config.xml"
            $configGUID = [Guid]::NewGuid().ToString("B")
            $configXml = @"
<?xml version="1.0" encoding="utf-8"?>
<Settings Version="1">
  <Namespace Name="Extensions">
    <Setting Name="ExtensionRepository" Type="VT_BSTR" Value="Implicit"></Setting>
    <Namespace Name="ExtensionRepositories">
      <Namespace Name="windbg-mcp-rs">
        <Setting Name="Id" Type="VT_BSTR" Value="$configGUID"></Setting>
        <Setting Name="LocalCacheRootFolder" Type="VT_BSTR" Value="$galleryDir"></Setting>
        <Setting Name="IsEnabled" Type="VT_BOOL" Value="true"></Setting>
      </Namespace>
    </Namespace>
  </Namespace>
</Settings>
"@
            Set-Content -Path $configPath -Value $configXml
        } catch {
            Write-Host "  [!] $($inst.Label): gallery config failed — $($_.Exception.Message)" -ForegroundColor Red
            $failed++
            continue
        }
    }

    Write-Host "  [+] $($inst.Label): installed" -ForegroundColor Green
    $installed++
    if ($isStore) { $storeInstalled = $true; $storeConfigPath = $configPath }
}

# Cleanup
Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "=== Installation Summary ===" -ForegroundColor Cyan
Write-Host "  Installed: $installed" -ForegroundColor Green
if ($failed -gt 0) { Write-Host "  Failed:    $failed" -ForegroundColor Red }
Write-Host ""
if ($storeInstalled) {
    Write-Host "WinDbg (Store): run the following once to enable auto-load:" -ForegroundColor Yellow
    Write-Host "  .settings load $storeConfigPath" -ForegroundColor White
    Write-Host "  .settings save" -ForegroundColor White
}
Write-Host "SDK Debuggers: Gallery manifest auto-loads on restart. Run '!mcp status'." -ForegroundColor Yellow
Write-Host "Server auto-starts at http://127.0.0.1:50051/mcp" -ForegroundColor Gray
