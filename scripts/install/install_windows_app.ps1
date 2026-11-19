# Installs or updates the Xedoc desktop app on Windows from a GitHub release.
# Usage: irm https://raw.githubusercontent.com/apohl79/xedoc/main/scripts/install/install_windows_app.ps1 | iex
# Set XEDOC_RELEASE_TAG (for example v1.96.0) to pin a release; the default is the latest release.

$ErrorActionPreference = "Stop"
$repo = if ($env:XEDOC_RELEASE_REPO) { $env:XEDOC_RELEASE_REPO } else { "apohl79/xedoc" }
$headers = @{ "User-Agent" = "xedoc-installer"; "Accept" = "application/vnd.github+json" }
$token = if ($env:GH_TOKEN) { $env:GH_TOKEN } else { $env:GITHUB_TOKEN }
if ($token) { $headers["Authorization"] = "Bearer $token" }

$releaseUrl = if ($env:XEDOC_RELEASE_TAG) {
    "https://api.github.com/repos/$repo/releases/tags/$($env:XEDOC_RELEASE_TAG)"
} else {
    "https://api.github.com/repos/$repo/releases/latest"
}
$release = Invoke-RestMethod -Uri $releaseUrl -Headers $headers
$version = $release.tag_name -replace '^v', ''
$assetName = "xedoc-desktop-$version-windows-x86_64-setup.exe"
$asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
if (-not $asset) { throw "Release $($release.tag_name) in $repo has no asset $assetName." }
if ($asset.digest -notmatch '^sha256:([0-9a-f]{64})$') { throw "Release asset $assetName has no SHA-256 digest." }
$expected = $Matches[1]

$installer = Join-Path ([IO.Path]::GetTempPath()) $assetName
Write-Host "==> Downloading $assetName"
$downloadHeaders = $headers.Clone()
$downloadHeaders["Accept"] = "application/octet-stream"
Invoke-WebRequest -Uri $asset.url -Headers $downloadHeaders -OutFile $installer -UseBasicParsing
$actual = (Get-FileHash -Algorithm SHA256 -Path $installer).Hash.ToLowerInvariant()
if ($actual -ne $expected) {
    Remove-Item -Force $installer
    throw "Downloaded installer checksum did not match. Expected $expected, got $actual."
}

Write-Host "==> Installing Xedoc desktop app $version for the current user"
$process = Start-Process -FilePath $installer -ArgumentList "/S" -Wait -PassThru
Remove-Item -Force $installer
if ($process.ExitCode -ne 0) { throw "The Xedoc installer exited with code $($process.ExitCode)." }
Write-Host "==> Xedoc desktop app installed; start it from the Start menu"
