$ErrorActionPreference = 'Stop'
$toolsDir = Split-Path -Parent $MyInvocation.MyCommand.Definition

# Update $version / $checksum per release (matches GitHub release asset).
$version = '0.1.2'
$checksum64 = 'REPLACE_WITH_SHA256_OF_ytdlp-tui-windows-x86_64.exe'
$url64 = "https://github.com/johnhalz/ytdlp-tui/releases/download/v$version/ytdlp-tui-windows-x86_64.exe"

$packageArgs = @{
  packageName    = $env:ChocolateyPackageName
  fileFullPath   = Join-Path $toolsDir 'ytdlp-tui.exe'
  url64bit       = $url64
  checksum64     = $checksum64
  checksumType64 = 'sha256'
}

Get-ChocolateyWebFile @packageArgs
