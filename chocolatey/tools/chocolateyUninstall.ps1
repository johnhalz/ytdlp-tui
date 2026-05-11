$ErrorActionPreference = 'Stop'
$toolsDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$exe = Join-Path $toolsDir 'ytdlp-tui.exe'
if (Test-Path $exe) {
  Remove-Item $exe -Force
}
