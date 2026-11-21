# Installs or updates the Xedoc CLI standalone package on Windows.
# Usage: irm https://raw.githubusercontent.com/apohl79/xedoc/main/scripts/install/install.ps1 | iex
# Or:    powershell -ExecutionPolicy Bypass -File install.ps1 [-Release TAG] [-Target TARGET]
#            [-Repo OWNER/REPO] [-LocalZip PATH] [-Experimental] [-Check]
#
# Environment:
#   XEDOC_RELEASE_TAG      Same as -Release. Defaults to the checkout version or the latest release.
#   XEDOC_RELEASE_TARGET   Same as -Target (x86_64-pc-windows-msvc).
#   XEDOC_RELEASE_REPO     Same as -Repo (XEDOC_REPO is accepted too). Defaults to apohl79/xedoc.
#   XEDOC_LOCAL_ZIP        Same as -LocalZip.
#   XEDOC_INSTALL_DIR      Directory for the visible xedoc command shims. Defaults to %XEDOC_HOME%\bin.
#   XEDOC_HOME             Xedoc home directory. Defaults to %USERPROFILE%\.xedoc.
#   XEDOC_NON_INTERACTIVE  Set to 1, true, or yes to skip prompts.
#   GH_TOKEN/GITHUB_TOKEN  Optional GitHub token for API requests.
[CmdletBinding()]
param(
    [Alias("Tag")][string]$Release,
    [string]$Target,
    [string]$Repo,
    [string]$LocalZip,
    [switch]$Experimental,
    [switch]$Check
)

# Run in a child scope so `irm | iex` does not leak preferences or helpers into the caller's session.
& {
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression.FileSystem

$ReleaseRepo = if ($Repo) { $Repo } elseif ($env:XEDOC_RELEASE_REPO) { $env:XEDOC_RELEASE_REPO } elseif ($env:XEDOC_REPO) { $env:XEDOC_REPO } else { "apohl79/xedoc" }
$ReleaseTag = if ($Release) { $Release } else { $env:XEDOC_RELEASE_TAG }
$ReleaseTarget = if ($Target) { $Target } else { $env:XEDOC_RELEASE_TARGET }
$LocalZipPath = if ($LocalZip) { $LocalZip } else { $env:XEDOC_LOCAL_ZIP }
$IsExperimental = [bool]$Experimental
$XedocHome = if ($env:XEDOC_HOME) { $env:XEDOC_HOME } else { Join-Path $env:USERPROFILE ".xedoc" }
$BinDir = if ($env:XEDOC_INSTALL_DIR) { $env:XEDOC_INSTALL_DIR } else { Join-Path $XedocHome "bin" }
$StandaloneRoot = Join-Path $XedocHome "packages\standalone"
$ReleasesDir = Join-Path $StandaloneRoot "releases"
$CurrentLink = Join-Path $StandaloneRoot "current"
$ExperimentalLink = Join-Path $StandaloneRoot "experimental"
$ModelRouterRuntimeRoot = Join-Path $XedocHome "packages\model-router-runtime"
$AutostartKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$AutostartName = "Xedoc app-server"
$WindowsTargets = @("x86_64-pc-windows-msvc")
$ScriptDir = $PSScriptRoot

# Mutable installer state; functions run in child scopes, so they update this table instead of variables.
$Installer = @{ AppServerWasRunning = $false; SkipModelRouterWarm = $false; AutostartAction = "skipped"; ReleaseJsonCache = @{} }

function Step([string]$Message) { Write-Host "==> $Message" }
function Warn([string]$Message) { Write-Warning $Message }
function Die([string]$Message) { throw "Error: $Message" }

function Test-PromptAvailable {
    if ($env:XEDOC_NON_INTERACTIVE -match '^(1|true|yes)$') { return $false }
    return [Environment]::UserInteractive -and -not [Console]::IsInputRedirected
}

function Read-YesNo([string]$Prompt) {
    if (-not (Test-PromptAvailable)) { return $false }
    $answer = Read-Host "$Prompt [y/N]"
    return $answer -match '^(y|yes)$'
}

# Windows PowerShell 5.1 turns redirected native stderr into terminating errors under "Stop".
function Invoke-Native([string]$Exe, [string[]]$Arguments) {
    $ErrorActionPreference = "Continue"
    $output = & $Exe @Arguments 2>&1 | Out-String
    return [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = $output }
}

function Get-RequestHeaders([string]$Accept) {
    $headers = @{ "User-Agent" = "xedoc-installer"; "Accept" = $Accept }
    $token = if ($env:GH_TOKEN) { $env:GH_TOKEN } else { $env:GITHUB_TOKEN }
    if ($token) { $headers["Authorization"] = "Bearer $token" }
    return $headers
}

function Get-ReleaseJson([string]$Tag) {
    if (-not $Installer.ReleaseJsonCache.ContainsKey($Tag)) {
        $url = "https://api.github.com/repos/$ReleaseRepo/releases/tags/$Tag"
        $Installer.ReleaseJsonCache[$Tag] = Invoke-RestMethod -Uri $url -Headers (Get-RequestHeaders "application/vnd.github+json")
    }
    return $Installer.ReleaseJsonCache[$Tag]
}

function Get-ReleaseAsset([string]$Tag, [string]$Name) {
    $asset = (Get-ReleaseJson $Tag).assets | Where-Object { $_.name -eq $Name } | Select-Object -First 1
    if (-not $asset) { Die "Could not find release asset $Name for tag $Tag in $ReleaseRepo." }
    $prefix = "https://api.github.com/repos/$ReleaseRepo/releases/assets/"
    if (-not $asset.url.StartsWith($prefix) -or $asset.url.Substring($prefix.Length) -notmatch '^[0-9]+$') {
        Die "Could not find release asset $Name for tag $Tag in $ReleaseRepo."
    }
    if ($asset.digest -notmatch '^sha256:([0-9a-f]{64})$') {
        Die "Could not find release asset digest for $Name in tag $Tag of $ReleaseRepo."
    }
    return [pscustomobject]@{ Url = $asset.url; Digest = $Matches[1] }
}

function Test-ReleaseAsset([string]$Tag, [string]$Name) {
    return [bool]((Get-ReleaseJson $Tag).assets | Where-Object { $_.name -eq $Name })
}

function Save-Download([string]$Url, [string]$Output) {
    Invoke-WebRequest -Uri $Url -Headers (Get-RequestHeaders "application/octet-stream") -OutFile $Output -UseBasicParsing
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Assert-ArchiveDigest([string]$Path, [string]$Expected) {
    $actual = Get-Sha256 $Path
    if ($actual -ne $Expected) {
        [Console]::Error.WriteLine("Expected: $Expected")
        [Console]::Error.WriteLine("Actual:   $actual")
        Die "Downloaded archive checksum did not match."
    }
}

function Assert-Repo([string]$Value) {
    if ($Value -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { Die "Invalid GitHub repository: $Value" }
}

function Assert-Tag([string]$Value) {
    if ($Value -cnotmatch '^v[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta)(\.[0-9]+)?)?$') { Die "Invalid Xedoc release tag: $Value" }
}

function Assert-Target([string]$Value) {
    if ($WindowsTargets -cnotcontains $Value) { Die "Unsupported target: $Value" }
}

function Get-LatestReleaseTag {
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$ReleaseRepo/releases/latest" -Headers (Get-RequestHeaders "application/vnd.github+json")
    } catch {
        Die "Could not fetch the latest Xedoc release metadata. GitHub API may be unavailable or rate limited."
    }
    if (-not $release.tag_name) { Die "Could not resolve the latest Xedoc release tag." }
    Assert-Tag $release.tag_name
    return $release.tag_name
}

function Get-CheckoutReleaseTag {
    if (-not $ScriptDir) { return $null }
    $repoRoot = [IO.Path]::GetFullPath([IO.Path]::Combine($ScriptDir, "..", ".."))
    if (-not (Test-Path -LiteralPath ([IO.Path]::Combine($repoRoot, "xedoc-rs")))) { return $null }
    if ((Test-Path -LiteralPath (Join-Path $repoRoot ".git")) -and (Get-Command git -ErrorAction SilentlyContinue)) {
        $tag = (Invoke-Native "git" @("-C", $repoRoot, "tag", "--points-at", "HEAD")).Output -split "\r?\n" | Where-Object { $_ -match '^v[0-9]+\.[0-9]+\.[0-9]+' } | Select-Object -Last 1
        if ($tag) { return $tag }
    }
    $cargoToml = Join-Path $repoRoot "xedoc-rs\Cargo.toml"
    if (-not (Test-Path -LiteralPath $cargoToml)) { return $null }
    $inWorkspacePackage = $false
    foreach ($line in Get-Content -LiteralPath $cargoToml) {
        if ($line -eq "[workspace.package]") { $inWorkspacePackage = $true; continue }
        if ($inWorkspacePackage -and $line -match '^\[') { break }
        if ($inWorkspacePackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return "v$($Matches[1])" }
    }
    return $null
}

function Get-CurrentReleaseTag {
    if ($ReleaseTag) { Assert-Tag $ReleaseTag; return $ReleaseTag }
    $tag = Get-CheckoutReleaseTag
    if (-not $tag) { $tag = Get-LatestReleaseTag }
    Assert-Tag $tag
    return $tag
}

function Get-ReleaseTarget([string]$Tag, [string]$Version) {
    if ($ReleaseTarget) { Assert-Target $ReleaseTarget; return $ReleaseTarget }
    return "x86_64-pc-windows-msvc"
}

function Read-ZipJson([string]$ZipPath, [string]$EntryName) {
    $zip = [IO.Compression.ZipFile]::OpenRead($ZipPath)
    try {
        $entry = $zip.GetEntry($EntryName)
        if (-not $entry) { return $null }
        $reader = New-Object IO.StreamReader($entry.Open())
        try { return $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
    } finally {
        $zip.Dispose()
    }
}

function Initialize-LocalPackage {
    if (-not (Test-Path -LiteralPath $LocalZipPath -PathType Leaf)) { Die "Local package ZIP does not exist: $LocalZipPath" }
    if ($LocalZipPath -notmatch '\.zip$') { Die "Local package must be a ZIP file: $LocalZipPath" }
    $zipPath = (Resolve-Path -LiteralPath $LocalZipPath).ProviderPath
    $metadata = Read-ZipJson $zipPath "xedoc-package.json"
    if (-not $metadata) { Die "Local package ZIP is missing xedoc-package.json: $zipPath" }
    if (-not $metadata.version) { Die "Local package metadata is missing version: $zipPath" }
    if (-not $metadata.target) { Die "Local package metadata is missing target: $zipPath" }

    $localTag = "v$($metadata.version)"
    Assert-Tag $localTag
    if ($ReleaseTag) {
        Assert-Tag $ReleaseTag
        if ($ReleaseTag -cne $localTag) { Die "Local package version does not match tag $ReleaseTag." }
    }

    Assert-Target $metadata.target
    if ($ReleaseTarget) {
        Assert-Target $ReleaseTarget
        if ($ReleaseTarget -cne $metadata.target) { Die "Local package target does not match target $ReleaseTarget." }
    }
    return [pscustomobject]@{ Zip = $zipPath; Tag = $localTag; Target = $metadata.target }
}

function Get-LocalRuntimeArchive([string]$AssetName) {
    $localZipDir = Split-Path -Parent $LocalZipPath
    $sibling = Join-Path $localZipDir $AssetName
    if (Test-Path -LiteralPath $sibling -PathType Leaf) { return $sibling }
    $runtimeRoot = Join-Path (Split-Path -Parent $localZipDir) "model-router-runtime"
    if (Test-Path -LiteralPath $runtimeRoot) {
        $found = Get-ChildItem -LiteralPath $runtimeRoot -Recurse -File -Filter $AssetName -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $sibling
}

function Test-ReparsePoint([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    return $item -and ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)
}

# Deletes a file, junction, or directory tree without following junctions into their targets.
function Remove-PathSafely([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        if (Test-ReparsePoint $Path) { [IO.Directory]::Delete($Path) }
        return
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        if ($item.PSIsContainer) { [IO.Directory]::Delete($Path) } else { [IO.File]::Delete($Path) }
        return
    }
    if (-not $item.PSIsContainer) { [IO.File]::Delete($Path); return }
    foreach ($child in Get-ChildItem -LiteralPath $Path -Force) {
        if (-not ($child.Attributes -band [IO.FileAttributes]::ReparsePoint) -and -not $child.PSIsContainer) {
            $child.Attributes = $child.Attributes -band -bnot [IO.FileAttributes]::ReadOnly
        }
        Remove-PathSafely $child.FullName
    }
    [IO.Directory]::Delete($Path)
}

# Points LinkPath at TargetDir with a directory junction (no admin or developer mode needed).
function Set-Junction([string]$LinkPath, [string]$TargetDir) {
    if ((Test-Path -LiteralPath $LinkPath) -and -not (Test-ReparsePoint $LinkPath)) {
        Die "$LinkPath exists and is not a junction; move it away and rerun the installer."
    }
    $tmpLink = Join-Path (Split-Path -Parent $LinkPath) ".$(Split-Path -Leaf $LinkPath).$PID"
    Remove-PathSafely $tmpLink
    New-Item -ItemType Junction -Path $tmpLink -Target $TargetDir | Out-Null
    Remove-PathSafely $LinkPath
    [IO.Directory]::Move($tmpLink, $LinkPath)
}

function Test-ReleaseDirComplete([string]$Dir) {
    foreach ($relative in @("xedoc-package.json", "bin\xedoc.exe", "xedoc-path\rg.exe")) {
        if (-not (Test-Path -LiteralPath (Join-Path $Dir $relative) -PathType Leaf)) { return $false }
    }
    return $true
}

function Install-ZipRelease([string]$ReleaseDir, [string]$ArchivePath) {
    $stage = Join-Path $ReleasesDir ".staging.$(Split-Path -Leaf $ReleaseDir).$PID"
    New-Item -ItemType Directory -Force -Path $ReleasesDir | Out-Null
    Remove-PathSafely $stage
    [IO.Compression.ZipFile]::ExtractToDirectory($ArchivePath, $stage)
    foreach ($relative in @("bin\xedoc.exe", "xedoc-path\rg.exe")) {
        if (-not (Test-Path -LiteralPath (Join-Path $stage $relative) -PathType Leaf)) {
            Remove-PathSafely $stage
            Die "Archive is missing $relative."
        }
    }
    foreach ($relative in @("bin\xedoc-session.cmd", "bin\xedoc-remote-agentd.cmd")) {
        if (-not (Test-Path -LiteralPath (Join-Path $stage $relative) -PathType Leaf)) {
            Warn "Package has no $relative; the matching command is unavailable."
        }
    }

    if (Test-Path -LiteralPath $ReleaseDir) {
        # Move first: a running release cannot be deleted, and a failed move leaves it intact.
        $trash = Join-Path $ReleasesDir ".trash.$(Split-Path -Leaf $ReleaseDir).$PID"
        try {
            [IO.Directory]::Move($ReleaseDir, $trash)
        } catch {
            Remove-PathSafely $stage
            Die "Could not replace $ReleaseDir because it is in use. Stop Xedoc (xedoc app-server daemon stop) and rerun the installer."
        }
        try { Remove-PathSafely $trash } catch { Warn "Could not remove the previous release copy $trash." }
    }
    [IO.Directory]::Move($stage, $ReleaseDir)
}

function Get-RuntimeManifest([string]$RuntimeDir) {
    $path = Join-Path $RuntimeDir "model-router-runtime.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    try { return Get-Content -Raw -LiteralPath $path | ConvertFrom-Json } catch { return $null }
}

function Test-RuntimeComplete([string]$RuntimeDir, [string]$ExpectedTarget, [string]$ExpectedRuntimeId) {
    $manifest = Get-RuntimeManifest $RuntimeDir
    if (-not $manifest -or $manifest.target -cne $ExpectedTarget -or $manifest.runtimeId -cne $ExpectedRuntimeId -or -not $manifest.files) {
        return $false
    }
    $fileCount = 0
    foreach ($file in $manifest.files.PSObject.Properties) {
        $relative = $file.Name
        if ($relative -match '(^|/)__pycache__(/|$)' -or $relative -match '\.pyc$') { continue }
        if (-not $relative -or -not $file.Value -or $relative.StartsWith("/") -or "/$relative/".Contains("/../") -or $relative.Contains("\")) {
            return $false
        }
        $path = Join-Path $RuntimeDir ($relative -replace '/', '\')
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
        if ((Get-Sha256 $path) -ne $file.Value) { return $false }
        $fileCount++
    }
    return $fileCount -gt 0
}

function Assert-RuntimeMetadata($Runtime, [string]$RuntimeTarget) {
    $id = [string]$Runtime.runtimeId
    if ($id -cnotmatch '^r[0-9][0-9a-z-]*-sha256-([0-9a-f]{64})$' -or $id.Contains("..")) {
        Die "Package metadata has an invalid model-router runtime ID."
    }
    if ($Runtime.assetName -cne "xedoc-model-router-runtime-$RuntimeTarget-$id.zip") {
        Die "Package metadata has an invalid model-router runtime asset."
    }
    if ($Runtime.sourceReleaseTag -cne "model-router-runtime-$id") {
        Die "Package metadata has an invalid model-router runtime source release."
    }
    if ([string]$Runtime.sha256 -cnotmatch '^[0-9a-f]{64}$') {
        Die "Package metadata has an invalid model-router runtime SHA256."
    }
}

function Get-RuntimePython([string]$ReleaseDir, [string]$RuntimeDir) {
    $bundled = Join-Path $ReleaseDir "xedoc-resources\remote-agent\runtime\python\python.exe"
    if (Test-Path -LiteralPath $bundled -PathType Leaf) { return $bundled }
    return Join-Path $RuntimeDir "python\python.exe"
}

function Get-EmbedderState([string]$ReleaseDir) {
    $runtimeLink = Join-Path $ReleaseDir "xedoc-resources\model-router\runtime"
    $runtimeItem = Get-Item -LiteralPath $runtimeLink -Force -ErrorAction SilentlyContinue
    $runtimeReal = $runtimeLink
    if ($runtimeItem -and $runtimeItem.Target) { $runtimeReal = @($runtimeItem.Target)[0] }
    $stateDir = Split-Path -Parent $runtimeReal
    return [pscustomobject]@{
        RuntimeDir = $runtimeLink
        StatePath = Join-Path $stateDir "embedder-daemon.json"
        LegacyStatePath = Join-Path $runtimeLink "embedder-daemon.json"
        Embedder = Join-Path $ReleaseDir "xedoc-resources\model-router\reference-router-embedder.py"
        Python = Get-RuntimePython $ReleaseDir $runtimeLink
    }
}

function Test-EmbedderDaemonRunning([string]$StatePath) {
    if (-not (Test-Path -LiteralPath $StatePath -PathType Leaf)) { return $false }
    try { $embedder = Get-Content -Raw -LiteralPath $StatePath | ConvertFrom-Json } catch { return $false }
    return $embedder.pid -and [bool](Get-Process -Id ([int]$embedder.pid) -ErrorAction SilentlyContinue)
}

function Restart-ModelRouterDaemon([string]$ReleaseDir) {
    $embedder = Get-EmbedderState $ReleaseDir
    $statePath = $embedder.StatePath
    if (-not (Test-Path -LiteralPath $statePath) -and (Test-Path -LiteralPath $embedder.LegacyStatePath)) {
        $statePath = $embedder.LegacyStatePath
    }
    if (-not (Test-EmbedderDaemonRunning $statePath)) {
        if (Test-Path -LiteralPath $statePath) { Remove-Item -Force -LiteralPath $statePath }
        return
    }
    $keepRunning = {
        if ($statePath -ne $embedder.StatePath) { Copy-Item -Force -LiteralPath $statePath -Destination $embedder.StatePath }
        $Installer.SkipModelRouterWarm = $true
    }
    if (-not (Test-PromptAvailable)) {
        Step "Model-router semantic runtime is running; leaving it unchanged in non-interactive mode"
        & $keepRunning
        return
    }
    if (-not (Read-YesNo "The model-router semantic runtime is running. Gracefully restart it for the upgraded runtime?")) {
        Step "Leaving the running model-router semantic runtime unchanged"
        & $keepRunning
        return
    }
    Step "Gracefully restarting model-router semantic runtime"
    & $embedder.Python -B $embedder.Embedder $embedder.RuntimeDir --stop-daemon $statePath
    if ($LASTEXITCODE -eq 0) {
        Step "Model-router semantic runtime stopped"
    } else {
        Warn "Could not stop the running model-router semantic runtime. Leaving it unchanged."
        & $keepRunning
    }
}

function Install-RouterRuntime([string]$ReleaseDir, [string]$RuntimeTarget, [string]$RuntimeId, [string]$ArchivePath) {
    $runtimeDir = Join-Path $ModelRouterRuntimeRoot "$RuntimeTarget\$RuntimeId"
    if (Test-RuntimeComplete $runtimeDir $RuntimeTarget $RuntimeId) {
        Step "Reusing cached model-router runtime $RuntimeId"
    } else {
        $stage = Join-Path $ModelRouterRuntimeRoot "$RuntimeTarget\.$RuntimeId.$PID.staging"
        Remove-PathSafely $stage
        [IO.Compression.ZipFile]::ExtractToDirectory($ArchivePath, $stage)
        if (-not (Test-RuntimeComplete $stage $RuntimeTarget $RuntimeId)) {
            Remove-PathSafely $stage
            Die "Model-router runtime archive is incomplete, invalid, or targets another platform."
        }
        Remove-PathSafely $runtimeDir
        [IO.Directory]::Move($stage, $runtimeDir)
    }
    $runtimeLink = Join-Path $ReleaseDir "xedoc-resources\model-router\runtime"
    if ((Test-Path -LiteralPath $runtimeLink) -and -not (Test-ReparsePoint $runtimeLink)) { Remove-PathSafely $runtimeLink }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $runtimeLink) | Out-Null
    Set-Junction $runtimeLink $runtimeDir
}

function Start-RouterRuntime([string]$ReleaseDir) {
    $embedder = Get-EmbedderState $ReleaseDir
    if (-not (Test-Path -LiteralPath $embedder.Python -PathType Leaf) -or -not (Test-Path -LiteralPath $embedder.Embedder -PathType Leaf)) { return }
    Step "Starting model-router semantic runtime"
    & $embedder.Python -B $embedder.Embedder $embedder.RuntimeDir --start-daemon $embedder.StatePath
    if ($LASTEXITCODE -ne 0) {
        Warn "Model-router semantic runtime did not start. Router diagnostics will report the failure."
    }
}

function Get-ShimTarget([string]$TargetPath) {
    # cmd.exe reads batch files in the OEM code page and expands %VAR% in literals, so avoid
    # embedding the (possibly non-ASCII) profile path when the target sits beside the bin dir.
    $parent = (Split-Path -Parent $BinDir.TrimEnd("\")) + "\"
    if ($TargetPath.StartsWith($parent, [StringComparison]::OrdinalIgnoreCase)) {
        return "%~dp0..\" + $TargetPath.Substring($parent.Length)
    }
    return $TargetPath.Replace("%", "%%")
}

function Set-CommandShim([string]$Name, [string]$TargetPath, [string]$Arguments = "") {
    $shim = Join-Path $BinDir "$Name.cmd"
    $content = "@echo off`r`n`"$(Get-ShimTarget $TargetPath)`" $Arguments%*`r`n"
    $tmp = Join-Path $BinDir ".$Name.cmd.$PID"
    [IO.File]::WriteAllText($tmp, $content, (New-Object Text.UTF8Encoding($false)))
    Move-Item -Force -LiteralPath $tmp -Destination $shim
}

function Update-CommandShims([string]$LinkPath, [string]$Suffix) {
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    Set-CommandShim "xedoc$Suffix" (Join-Path $LinkPath "bin\xedoc.exe")
    foreach ($command in @("xedoc-session", "xedoc-remote-agentd")) {
        if ($Suffix -and $command -eq "xedoc-remote-agentd") { continue }
        $target = Join-Path $LinkPath "bin\$command.cmd"
        $shim = Join-Path $BinDir "$command$Suffix.cmd"
        if (Test-Path -LiteralPath $target -PathType Leaf) {
            # xedoc-session detects the experimental target from its own name on Unix; a
            # .cmd shim cannot rename the Python script, so pass the flag explicitly.
            $arguments = if ($Suffix -and $command -eq "xedoc-session") { "--experimental " } else { "" }
            Set-CommandShim "$command$Suffix" $target $arguments
        } elseif ((Test-Path -LiteralPath $shim) -and (Get-Content -Raw -LiteralPath $shim).Contains((Get-ShimTarget $target))) {
            Remove-Item -Force -LiteralPath $shim
        }
    }
}

function Add-BinDirToUserPath {
    $normalized = $BinDir.TrimEnd("\")
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("Environment")
    try {
        $userPath = [string]$key.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $entries = @($userPath.Split(";") | Where-Object { $_ })
        $present = $entries | Where-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd("\") -eq $normalized }
        if (-not $present) {
            $key.SetValue("Path", (($entries + $BinDir) -join ";"), [Microsoft.Win32.RegistryValueKind]::ExpandString)
            # Setting a user variable through .NET broadcasts WM_SETTINGCHANGE so new terminals see the PATH.
            [Environment]::SetEnvironmentVariable("XEDOC_INSTALLER_PATH_REFRESH", "1", "User")
            [Environment]::SetEnvironmentVariable("XEDOC_INSTALLER_PATH_REFRESH", $null, "User")
            Step "Added $BinDir to your user PATH; open a new terminal to use xedoc"
        } else {
            Step "$BinDir is already on your user PATH"
        }
    } finally {
        $key.Close()
    }
    if (-not (@($env:Path.Split(";")) | Where-Object { $_.TrimEnd("\") -eq $normalized })) {
        $env:Path = "$BinDir;$env:Path"
    }
}

function Get-AutostartCommand {
    $exe = Join-Path $CurrentLink "bin\xedoc.exe"
    return "powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -Command `"& '$($exe.Replace("'", "''"))' app-server daemon start`""
}

function Set-AppServerAutostart {
    $existing = (Get-ItemProperty -Path $AutostartKey -Name $AutostartName -ErrorAction SilentlyContinue).$AutostartName
    $command = Get-AutostartCommand
    if ($null -ne $existing) {
        if ($existing -eq $command) {
            $Installer.AutostartAction = "configured"
        } else {
            Set-ItemProperty -Path $AutostartKey -Name $AutostartName -Value $command
            $Installer.AutostartAction = "updated"
        }
        return
    }
    if (-not (Test-PromptAvailable)) { return }
    if (-not (Read-YesNo "Start the Xedoc app-server automatically when you sign in to Windows?")) {
        $Installer.AutostartAction = "disabled"
        return
    }
    New-Item -Force -Path $AutostartKey | Out-Null
    Set-ItemProperty -Path $AutostartKey -Name $AutostartName -Value $command
    $Installer.AutostartAction = "added"
}

function Test-AppServerRunning {
    $exe = Join-Path $CurrentLink "bin\xedoc.exe"
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { return $false }
    return (Invoke-Native $exe @("app-server", "daemon", "version")).ExitCode -eq 0
}

function Invoke-Xedoc([string[]]$Arguments) {
    # Do not pipe output: the app-server daemon this starts may inherit the pipe and block the read.
    $startInfo = New-Object Diagnostics.ProcessStartInfo
    $startInfo.FileName = Join-Path $CurrentLink "bin\xedoc.exe"
    $startInfo.Arguments = $Arguments -join " "
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($startInfo)
    $process.WaitForExit()
    return $process.ExitCode -eq 0
}

function Start-AppServer {
    if ($Installer.AppServerWasRunning) {
        if (-not (Test-PromptAvailable)) {
            Step "App-server was running before the upgrade; leaving it running in non-interactive mode"
            return
        }
        if (-not (Read-YesNo "The Xedoc app-server is running. Restart it to use the upgraded version?")) {
            Step "Leaving the running Xedoc app-server unchanged"
            return
        }
        Step "Restarting Xedoc app-server"
        if (Invoke-Xedoc @("app-server", "daemon", "restart")) {
            Step "Xedoc app-server restarted"
        } else {
            Warn "Could not restart the running Xedoc app-server. Run: xedoc app-server daemon restart"
        }
        return
    }
    if ($Installer.AutostartAction -notin @("added", "updated", "configured")) { return }
    Step "Starting Xedoc app-server"
    if (-not (Invoke-Xedoc @("app-server", "daemon", "start"))) {
        Warn "Could not start the Xedoc app-server. Run: xedoc app-server daemon start"
    }
}

function Get-RemoteAgentCommand {
    $link = if ($IsExperimental) { $ExperimentalLink } else { $CurrentLink }
    $command = Join-Path $link "bin\xedoc-remote-agentd.cmd"
    if (Test-Path -LiteralPath $command -PathType Leaf) { return $command }
    return $null
}

function Initialize-HostPush {
    $command = Get-RemoteAgentCommand
    if (-not $command) { return }
    # Older/local packages may predate automatic APNs setup.
    if ((Invoke-Native $command @("apns", "--help")).ExitCode -ne 0) { return }
    $status = (Invoke-Native $command @("apns", "check", "--xedoc-home", $XedocHome)).ExitCode
    if ($status -eq 0) { Step "Host push credentials are ready"; return }
    if ($status -ne 2) {
        Warn "Existing host push credentials could not be validated; leaving them unchanged without contacting 1Password."
        return
    }
    Step "Provisioning missing host push credentials"
    if (-not (Get-Command op -ErrorAction SilentlyContinue)) {
        Warn "Host push is unavailable: 1Password CLI was not found in PATH; signing keys cannot be retrieved."
        return
    }
    if (-not (Test-PromptAvailable)) {
        Warn "Host push is unavailable: 1Password sign-in approval requires an interactive terminal."
        return
    }
    & $command apns setup --xedoc-home $XedocHome
    if ($LASTEXITCODE -ne 0) {
        Warn "Host push credential provisioning failed; no manual APNs configuration is needed, but signing-key access is required."
    }
}

function Initialize-RemoteAgent {
    if (-not (Get-RemoteAgentCommand)) { return }
    $suffix = if ($IsExperimental) { "-experimental" } else { "" }
    $sessionCommand = Join-Path $BinDir "xedoc-session$suffix.cmd"
    if (-not (Test-Path -LiteralPath $sessionCommand -PathType Leaf)) { return }
    if (-not (Test-PromptAvailable)) {
        Step "Remote-agent onboarding requires an interactive terminal"
        Write-Host "Run after installation:`n  xedoc-session$suffix remote-agent-configure"
        return
    }
    & $sessionCommand remote-agent-configure
    if ($LASTEXITCODE -eq 0) {
        Step "Remote-agent configuration is ready"
    } else {
        Warn "Remote-agent onboarding was not completed. Re-run:`n  xedoc-session$suffix remote-agent-configure"
    }
}

function Install-DesktopApp([string]$Tag) {
    if ($LocalZipPath -or $IsExperimental -or $Tag -match '-(alpha|beta)') { return }
    if (-not (Read-YesNo "Install or update the Xedoc desktop app?")) {
        if (Test-PromptAvailable) { Step "Skipping Xedoc desktop app installation" }
        return
    }
    $localScript = if ($ScriptDir) { Join-Path $ScriptDir "install_windows_app.ps1" } else { $null }
    try {
        if ($localScript -and (Test-Path -LiteralPath $localScript -PathType Leaf)) {
            $source = Get-Content -Raw -LiteralPath $localScript
        } else {
            $url = "https://raw.githubusercontent.com/$ReleaseRepo/$Tag/scripts/install/install_windows_app.ps1"
            $source = (Invoke-WebRequest -Uri $url -Headers (Get-RequestHeaders "text/plain") -UseBasicParsing).Content
        }
        $previousTag = $env:XEDOC_RELEASE_TAG
        $previousRepo = $env:XEDOC_RELEASE_REPO
        $env:XEDOC_RELEASE_TAG = $Tag
        $env:XEDOC_RELEASE_REPO = $ReleaseRepo
        try {
            # A script block bypasses the execution policy that would block a downloaded .ps1 file.
            & ([scriptblock]::Create($source))
        } finally {
            $env:XEDOC_RELEASE_TAG = $previousTag
            $env:XEDOC_RELEASE_REPO = $previousRepo
        }
    } catch {
        Warn "Could not install or update the Xedoc desktop app; the Xedoc CLI remains installed. $($_.Exception.Message)"
    }
}

function Write-AutostartSummary {
    switch ($Installer.AutostartAction) {
        "added" { Step "App-server startup was added to $AutostartKey\$AutostartName" }
        "updated" { Step "App-server startup was updated in $AutostartKey\$AutostartName" }
        "configured" { Step "App-server startup is already configured in $AutostartKey\$AutostartName" }
        "disabled" { Step "App-server startup was not configured" }
        default { Step "App-server startup was not configured; run the installer interactively to enable it, or start it with: xedoc app-server daemon start" }
    }
}

Assert-Repo $ReleaseRepo
if ($IsExperimental -and -not $LocalZipPath) { Die "-Experimental requires -LocalZip." }
if ($LocalZipPath) {
    $localPackage = Initialize-LocalPackage
    $LocalZipPath = $localPackage.Zip
    $ReleaseTag = $localPackage.Tag
    $ReleaseTarget = $localPackage.Target
}

$tag = Get-CurrentReleaseTag
$releaseVersion = $tag.Substring(1)
$target = Get-ReleaseTarget $tag $releaseVersion
$asset = "xedoc-$target-$releaseVersion.zip"
$releaseName = "$releaseVersion-$target"
if ($IsExperimental) { $releaseName = "$releaseName-experimental" }
$releaseDir = Join-Path $ReleasesDir $releaseName

if ($LocalZipPath) {
    Step "Found local Xedoc package"
    Write-Host "Package:    $LocalZipPath"
    Write-Host "SHA256:     $(Get-Sha256 $LocalZipPath)"
} else {
    $releaseAsset = Get-ReleaseAsset $tag $asset
    Step "Found Xedoc release asset"
    Write-Host "Repository: $ReleaseRepo"
    Write-Host "Asset:      $asset"
    Write-Host "SHA256:     $($releaseAsset.Digest)"
    Write-Host "URL:        $($releaseAsset.Url)"
}
Write-Host "Tag:        $tag"
Write-Host "Target:     $target"
if ($Check) { return }

$Installer.AppServerWasRunning = Test-AppServerRunning
$tmpDir = Join-Path ([IO.Path]::GetTempPath()) "xedoc-install-$PID"
New-Item -ItemType Directory -Force -Path $tmpDir | Out-Null
try {
    if ($LocalZipPath -or -not (Test-ReleaseDirComplete $releaseDir)) {
        if ($LocalZipPath) {
            $archivePath = $LocalZipPath
            Step "Installing local package ZIP"
        } else {
            $archivePath = Join-Path $tmpDir $asset
            Step "Downloading $asset"
            Save-Download $releaseAsset.Url $archivePath
            Assert-ArchiveDigest $archivePath $releaseAsset.Digest
        }
        Step "Installing standalone package to $releaseDir"
        Install-ZipRelease $releaseDir $archivePath
    }

    $packageMetadata = Get-Content -Raw -LiteralPath (Join-Path $releaseDir "xedoc-package.json") | ConvertFrom-Json
    $runtime = $packageMetadata.modelRouterRuntime
    if ($runtime -and ($runtime.runtimeId -or $runtime.assetName -or $runtime.sha256 -or $runtime.sourceReleaseTag)) {
        if (-not ($runtime.runtimeId -and $runtime.assetName -and $runtime.sha256 -and $runtime.sourceReleaseTag)) {
            Die "Package metadata has an incomplete model-router runtime reference."
        }
        Assert-RuntimeMetadata $runtime $target

        if (-not $IsExperimental) {
            $currentManifest = Get-RuntimeManifest (Join-Path $CurrentLink "xedoc-resources\model-router\runtime")
            if ($currentManifest -and $currentManifest.runtimeId -and $currentManifest.runtimeId -ne $runtime.runtimeId) {
                Restart-ModelRouterDaemon $CurrentLink
            }
        }

        $runtimeArchive = $null
        if (-not (Test-RuntimeComplete (Join-Path $ModelRouterRuntimeRoot "$target\$($runtime.runtimeId)") $target $runtime.runtimeId)) {
            if ($LocalZipPath) {
                $runtimeArchive = Get-LocalRuntimeArchive $runtime.assetName
                if (-not (Test-Path -LiteralPath $runtimeArchive -PathType Leaf)) {
                    Die "Local model-router runtime archive does not exist: $runtimeArchive"
                }
                Assert-ArchiveDigest $runtimeArchive $runtime.sha256
                Step "Installing local model-router runtime"
            } else {
                $runtimeArchive = Join-Path $tmpDir $runtime.assetName
                $runtimeAsset = Get-ReleaseAsset $runtime.sourceReleaseTag $runtime.assetName
                Step "Downloading $($runtime.assetName)"
                Save-Download $runtimeAsset.Url $runtimeArchive
                Assert-ArchiveDigest $runtimeArchive $runtime.sha256
            }
        }
        Install-RouterRuntime $releaseDir $target $runtime.runtimeId $runtimeArchive
        if ($Installer.SkipModelRouterWarm) {
            Step "Skipping model-router semantic runtime warmup because the existing daemon was left running"
        } else {
            Start-RouterRuntime $releaseDir
        }
    } else {
        Step "Package has no model-router runtime reference; semantic routing is unavailable"
    }

    if ($IsExperimental) {
        Set-Junction $ExperimentalLink $releaseDir
        Update-CommandShims $ExperimentalLink "-experimental"
        if ((Invoke-Native (Join-Path $ExperimentalLink "bin\xedoc.exe") @("--version")).ExitCode -ne 0) { Die "The installed xedoc-experimental command failed to run." }
    } else {
        Set-Junction $CurrentLink $releaseDir
        Update-CommandShims $CurrentLink ""
        if ((Invoke-Native (Join-Path $CurrentLink "bin\xedoc.exe") @("--version")).ExitCode -ne 0) { Die "The installed xedoc command failed to run." }
        Set-AppServerAutostart
        Start-AppServer
    }
    Add-BinDirToUserPath

    Initialize-HostPush
    Initialize-RemoteAgent
    Step "Skipping NAT bridge connector setup: automatic setup supports macOS and Linux only. Direct host login is unchanged."

    if (-not $IsExperimental) { Write-AutostartSummary }
    Write-Host "Xedoc CLI $releaseVersion installed successfully."
    Install-DesktopApp $tag
} finally {
    Remove-PathSafely $tmpDir
}
}
