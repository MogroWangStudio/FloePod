param([string]$Destination = "dist/github-artifacts", [string]$Commit = $(git rev-parse HEAD))
$ErrorActionPreference = "Stop"
$version = (Get-Content package.json -Raw | ConvertFrom-Json).version
# 与发布流水线同源：含后缀即预发布，跳过 MSI/MSIX（只接受纯数字版本号）。
$isPrerelease = $version -match '-'
$binary = Get-Item "src-tauri/target/release/FloePod.exe"
function Require-One([string]$Folder, [string]$Filter) {
  $items = @(Get-ChildItem -LiteralPath $Folder -Filter $Filter -File)
  if ($items.Count -ne 1) { throw "Expected one $Filter in $Folder; found $($items.Count)" }
  return $items[0]
}
$nsis = Require-One "src-tauri/target/release/bundle/nsis" "*.exe"
$portable = Require-One "dist" "FloePod-$version-win-x64-portable.zip"
$msi = $null
$msix = $null
if (-not $isPrerelease) {
  $msi = Require-One "src-tauri/target/release/bundle/msi" "*.msi"
  $msix = Require-One "dist" "FloePod-$version-win-x64.msix"
}
foreach ($pe in @($binary, $nsis)) {
  $actual = [Diagnostics.FileVersionInfo]::GetVersionInfo($pe.FullName).ProductVersion
  if ($actual -ne $version -and $actual -ne "$version.0") { throw "PE version mismatch: $($pe.Name) = $actual" }
}
if ($msi) {
  $installer = New-Object -ComObject WindowsInstaller.Installer
  $database = $installer.OpenDatabase($msi.FullName, 0)
  $view = $database.OpenView("SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = 'ProductVersion'")
  $view.Execute()
  $record = $view.Fetch()
  if ($record.StringData(1) -ne $version) { throw "MSI version mismatch" }
  $view.Close()
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$binaryHash = (Get-FileHash -LiteralPath $binary.FullName -Algorithm SHA256).Hash
$archives = @($portable)
if ($msix) { $archives += $msix }
foreach ($archiveFile in $archives) {
  $archive = [IO.Compression.ZipFile]::OpenRead($archiveFile.FullName)
  try {
    $entryPath = if ($archiveFile.Extension -eq ".zip") { "FloePod/FloePod.exe" } else { "FloePod.exe" }
    $entry = $archive.GetEntry($entryPath)
    if (-not $entry) { throw "Missing executable in $($archiveFile.Name)" }
    $stream = $entry.Open()
    try { $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($stream)) } finally { $stream.Dispose() }
    if ($hash -ne $binaryHash) { throw "Packaged executable differs from final executable" }
    if ($archiveFile.Extension -eq ".msix") {
      $reader = [IO.StreamReader]::new($archive.GetEntry("AppxManifest.xml").Open())
      try { [xml]$manifest = $reader.ReadToEnd() } finally { $reader.Dispose() }
      if ($manifest.Package.Identity.Version -ne "$version.0") { throw "MSIX version mismatch" }
    }
  } finally { $archive.Dispose() }
}
$signing = "unsigned"
if ($env:FLOEPOD_SIGN_CERT_PATH) {
  $signed = @($binary.FullName, $nsis.FullName)
  if ($msi) { $signed += $msi.FullName }
  if ($msix) { $signed += $msix.FullName }
  ./scripts/sign-windows-artifacts.ps1 -Path $signed -VerifyOnly
  $signing = "verified"
}
New-Item -ItemType Directory -Path $Destination -Force | Out-Null
if (@(Get-ChildItem -LiteralPath $Destination -File).Count -ne 0) { throw "Artifact destination must be empty" }
Copy-Item -LiteralPath $binary.FullName -Destination (Join-Path $Destination "FloePod-$version-windows-x64.exe")
$packages = @($nsis, $portable)
if ($msi) { $packages += $msi }
if ($msix) { $packages += $msix }
foreach ($file in $packages) { Copy-Item -LiteralPath $file.FullName -Destination $Destination }
@{ version = $version; commit = $Commit; signing = $signing; channel = $(if ($isPrerelease) { "beta" } else { "stable" }) } |
  ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Destination "release-manifest.json") -Encoding utf8NoBOM
$checksums = Get-ChildItem -LiteralPath $Destination -File | Sort-Object Name | ForEach-Object {
  "$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $($_.Name)"
}
$checksums | Set-Content -LiteralPath (Join-Path $Destination "SHA256SUMS.txt") -Encoding utf8NoBOM
$packageCount = @(Get-ChildItem -LiteralPath $Destination -File).Count - 2
Write-Output "Verified version $version, $packageCount Windows packages, signing=$signing, SHA-256 checksums"
