$ErrorActionPreference = 'Stop'
$Fixture = Join-Path ([IO.Path]::GetTempPath()) ('toolkit284-' + [Guid]::NewGuid().ToString('N'))
if (Test-Path -LiteralPath $Fixture) { throw 'Fixture directory already exists.' }
[IO.Directory]::CreateDirectory($Fixture) | Out-Null
$Generator = Join-Path $PSScriptRoot 'create-release-triplet.ps1'
$Aliases = @()
# Exact builtin text at the existing pinned vcpkg revision, including Fortran selection.
$PinnedText = "set(VCPKG_TARGET_ARCHITECTURE x64)`nset(VCPKG_CRT_LINKAGE static)`nset(VCPKG_LIBRARY_LINKAGE static)`nset(VCPKG_PROVIDED_FORTRAN ON)`n"
try {
  foreach ($Ending in @('LF', 'CRLF')) {
    $VcpkgRoot = Join-Path $Fixture "vcpkg source with spaces $Ending"
    $OverlayRoot = Join-Path $Fixture "release overlay with spaces $Ending"
    [IO.Directory]::CreateDirectory((Join-Path $VcpkgRoot 'triplets')) | Out-Null
    $Text = if ($Ending -eq 'CRLF') { $PinnedText.Replace("`n", "`r`n") } else { $PinnedText }
    $InputFile = Join-Path $VcpkgRoot 'triplets/x64-windows-static.cmake'
    [IO.File]::WriteAllText($InputFile, $Text, [Text.UTF8Encoding]::new($false))
    $InputHash = (Get-FileHash -LiteralPath $InputFile -Algorithm SHA256).Hash
    & $Generator -VcpkgRoot $VcpkgRoot -OverlayRoot $OverlayRoot
    $OutputFile = Join-Path $OverlayRoot 'x64-windows-static.cmake'
    $Expected = [Text.UTF8Encoding]::new($false).GetBytes($Text + "`nset(VCPKG_BUILD_TYPE release)`n")
    $Actual = [IO.File]::ReadAllBytes($OutputFile)
    if ([Convert]::ToBase64String($Actual) -ne [Convert]::ToBase64String($Expected)) { throw 'Release overlay changed the pinned prefix or encoding.' }
    if ((Get-FileHash -LiteralPath $InputFile -Algorithm SHA256).Hash -ne $InputHash) { throw 'Pinned source changed.' }
    & $Generator -VcpkgRoot $VcpkgRoot -OverlayRoot $OverlayRoot
    if ([IO.File]::ReadAllText($OutputFile) -cne ($Text + "`nset(VCPKG_BUILD_TYPE release)`n")) { throw 'Repeated generation appended another override.' }
    [IO.File]::SetAttributes($OutputFile, [IO.FileAttributes]::ReadOnly)
    & $Generator -VcpkgRoot $VcpkgRoot -OverlayRoot $OverlayRoot
    [IO.File]::SetAttributes($OutputFile, [IO.FileAttributes]::Normal)
    [IO.File]::WriteAllText($OutputFile, 'unexpected existing output')
    $Rejected = $false
    try { & $Generator -VcpkgRoot $VcpkgRoot -OverlayRoot $OverlayRoot } catch { $Rejected = $true }
    if (-not $Rejected -or [IO.File]::ReadAllText($OutputFile) -cne 'unexpected existing output') { throw 'Unexpected output was overwritten.' }
    $CompetingRoot = Join-Path $Fixture "competing output with spaces $Ending"
    [IO.Directory]::CreateDirectory($CompetingRoot) | Out-Null
    $CompetingFile = Join-Path $CompetingRoot 'x64-windows-static.cmake'
    $Writer = [IO.File]::Open($CompetingFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
      $Rejected = $false
      try { & $Generator -VcpkgRoot $VcpkgRoot -OverlayRoot $CompetingRoot } catch { $Rejected = $true }
      if (-not $Rejected) { throw 'An in-progress output was accepted.' }
    } finally { $Writer.Dispose() }
    if ((Get-Item -LiteralPath $CompetingFile).Length -ne 0) { throw 'Another creator output was replaced.' }
    $Alias = Join-Path $Fixture "aliased output with spaces $Ending"
    $AliasType = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
    New-Item -ItemType $AliasType -Path $Alias -Target (Join-Path $VcpkgRoot 'triplets') | Out-Null
    $Aliases += $Alias
    $Rejected = $false
    try { & $Generator -VcpkgRoot $VcpkgRoot -OverlayRoot $Alias } catch { $Rejected = $true }
    if (-not $Rejected -or (Get-FileHash -LiteralPath $InputFile -Algorithm SHA256).Hash -ne $InputHash) { throw 'A directory alias replaced the pinned source.' }
    $Rejected = $false
    try { & $Generator -VcpkgRoot $VcpkgRoot -OverlayRoot (Join-Path $VcpkgRoot 'triplets') } catch { $Rejected = $true }
    if (-not $Rejected) { throw 'Pinned source overwrite was accepted.' }
    $Rejected = $false
    try { & $Generator -VcpkgRoot $VcpkgRoot -OverlayRoot $OverlayRoot -Triplet 'x64-windows' } catch { $Rejected = $true }
    if (-not $Rejected) { throw 'A different triplet was accepted.' }
    Write-Output "Release triplet fixture $Ending passed, including paths with spaces."
  }
  $Rejected = $false
  try { & $Generator -VcpkgRoot (Join-Path $Fixture 'missing source') -OverlayRoot (Join-Path $Fixture 'missing overlay') } catch { $Rejected = $true }
  if (-not $Rejected -or (Test-Path -LiteralPath (Join-Path $Fixture 'missing overlay'))) { throw 'Missing pinned input created an overlay.' }
} finally {
  foreach ($Alias in $Aliases) { [IO.Directory]::Delete($Alias) }
  Remove-Item -LiteralPath $Fixture -Recurse -Force
}
