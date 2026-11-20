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
[IO.Directory]::CreateDirectory($OverlayRoot) | Out-Null
[IO.File]::WriteAllText($ReleaseTriplet, $PinnedText + "`nset(VCPKG_BUILD_TYPE release)`n", [Text.UTF8Encoding]::new($false))
