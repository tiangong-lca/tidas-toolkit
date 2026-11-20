[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$VcpkgRoot,
  [Parameter(Mandatory)][string]$OverlayRoot,
  [ValidateSet('x64-windows-static')][string]$Triplet = 'x64-windows-static'
)
$ErrorActionPreference = 'Stop'

$PinnedTriplet = Join-Path $VcpkgRoot "triplets/$Triplet.cmake"
$ReleaseTriplet = Join-Path $OverlayRoot "$Triplet.cmake"
if ([IO.Path]::GetFullPath($PinnedTriplet) -eq [IO.Path]::GetFullPath($ReleaseTriplet)) {
  throw 'The release overlay must not replace the pinned triplet.'
}
$PinnedText = [IO.File]::ReadAllText($PinnedTriplet)
$GeneratedBytes = [Text.UTF8Encoding]::new($false).GetBytes($PinnedText + "`nset(VCPKG_BUILD_TYPE release)`n")
[IO.Directory]::CreateDirectory($OverlayRoot) | Out-Null
try {
  $Output = [IO.File]::Open($ReleaseTriplet, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
} catch [IO.IOException] {
  if ([IO.File]::Exists($ReleaseTriplet)) {
    $Existing = [IO.File]::ReadAllBytes($ReleaseTriplet)
    if ([Convert]::ToBase64String($Existing) -ceq [Convert]::ToBase64String($GeneratedBytes)) { return }
  }
  throw 'The release overlay already exists with different contents or cannot be created exclusively.'
}
try {
  $Output.Write($GeneratedBytes, 0, $GeneratedBytes.Length)
} finally {
  $Output.Dispose()
}
