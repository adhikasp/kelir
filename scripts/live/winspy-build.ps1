# Build winspy.exe from winspy.cs with the .NET Framework compiler (ships with Windows).
# Usage: powershell -File scripts/live/winspy-build.ps1 [-Out <dir>]
param([string]$Out = "$env:USERPROFILE\kelir-scratch\flash")
$csc = "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
New-Item -ItemType Directory -Force $Out | Out-Null
& $csc /nologo /optimize /platform:x64 /out:"$Out\winspy.exe" "$PSScriptRoot\winspy.cs"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "built $Out\winspy.exe"
