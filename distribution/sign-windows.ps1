param([Parameter(Mandatory=$true)][string]$Binary)
$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($env:WINDOWS_CERTIFICATE_BASE64)) {
  Write-Host 'WINDOWS_SIGNING=disabled (unsigned development artifact)'
  exit 0
}
$certificate = Join-Path $env:RUNNER_TEMP 'apex-trace-signing.pfx'
[IO.File]::WriteAllBytes($certificate, [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE_BASE64))
try { & signtool sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com /f $certificate /p $env:WINDOWS_CERTIFICATE_PASSWORD $Binary }
finally { Remove-Item -Force $certificate -ErrorAction SilentlyContinue }
