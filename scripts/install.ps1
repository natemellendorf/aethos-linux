param(
  [string]$Repo = "natemellendorf/aethos-linux",
  [string]$Ref = "",
  [string]$InstallDir = "$env:LOCALAPPDATA\Programs\Aethos\bin"
)

$ErrorActionPreference = "Stop"

function Write-Log([string]$Message) {
  Write-Host "[aethos-install] $Message"
}

function Resolve-TargetTriple {
  $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
  switch ($arch) {
    "X64" { return "x86_64-pc-windows-gnu" }
    default { throw "Unsupported Windows architecture: $arch" }
  }
}

function Resolve-Ref {
  param([string]$Repository, [string]$RequestedRef)

  if ($RequestedRef -and $RequestedRef.Trim().Length -gt 0) {
    Write-Log "Using explicit release tag: $RequestedRef"
    return $RequestedRef
  }

  $api = "https://api.github.com/repos/$Repository/releases/latest"
  $latest = Invoke-RestMethod -Uri $api -Headers @{ Accept = "application/vnd.github+json" }
  if (-not $latest.tag_name) {
    throw "Unable to resolve latest official release tag"
  }

  Write-Log "Resolved latest release tag: $($latest.tag_name)"
  return $latest.tag_name
}

function Resolve-AssetUrl {
  param([string]$Repository, [string]$Tag, [string]$TargetTriple)

  $api = "https://api.github.com/repos/$Repository/releases/tags/$Tag"
  $release = Invoke-RestMethod -Uri $api -Headers @{ Accept = "application/vnd.github+json" }
  $assetName = "aethos-$Tag-$TargetTriple.zip"

  $asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
  if (-not $asset) {
    throw "Release '$Tag' does not contain asset '$assetName'"
  }

  return $asset.browser_download_url
}

$target = Resolve-TargetTriple
$tag = Resolve-Ref -Repository $Repo -RequestedRef $Ref
$assetUrl = Resolve-AssetUrl -Repository $Repo -Tag $tag -TargetTriple $target

$tempRoot = Join-Path $env:TEMP ("aethos-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

try {
  $zipPath = Join-Path $tempRoot "aethos.zip"
  $extractPath = Join-Path $tempRoot "extract"

  Write-Log "Downloading release asset"
  Invoke-WebRequest -Uri $assetUrl -OutFile $zipPath

  Expand-Archive -Path $zipPath -DestinationPath $extractPath -Force

  $exePath = Join-Path $extractPath "aethos.exe"
  if (-not (Test-Path $exePath)) {
    throw "Archive missing expected binary: aethos.exe"
  }

  New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
  $targetExe = Join-Path $InstallDir "aethos.exe"
  Copy-Item $exePath $targetExe -Force

  # Backward-compatible alias executable name.
  Copy-Item $targetExe (Join-Path $InstallDir "aethos-linux.exe") -Force

  Write-Log "Installed: $targetExe"
  Write-Log "Alias:     $(Join-Path $InstallDir "aethos-linux.exe")"

  $pathParts = ($env:PATH -split ';' | ForEach-Object { $_.Trim() })
  if ($pathParts -notcontains $InstallDir) {
    Write-Log "Install directory is not in PATH. Add it with:"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';$InstallDir', 'User')"
  }

  Write-Log "Done. Run 'aethos'"
}
finally {
  Remove-Item -Recurse -Force $tempRoot -ErrorAction SilentlyContinue
}
