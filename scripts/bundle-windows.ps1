$ErrorActionPreference = 'Stop'
# 测试版本（x.y.z-beta<N>）跳过 MSI 与 MSIX：这两种 Windows 安装包格式只
# 接受纯数字版本号，无法表达预发布。测试通道产物为 NSIS 安装器与便携包。
$version = (Get-Content (Join-Path $PSScriptRoot '..\package.json') -Raw | ConvertFrom-Json).version
$isPrerelease = $version -match '-'
$arguments = @('tauri', 'bundle', '--ci', '--bundles', $(if ($isPrerelease) { 'nsis' } else { 'nsis,msi' }))
$signingConfig = $null
try {
  if ($env:FLOEPOD_SIGN_CERT_PATH) {
    # Tauri patches the executable separately for NSIS and MSI. Its callback
    # signs each patched executable and installer, including the uninstaller.
    $signingConfig = Join-Path ([IO.Path]::GetTempPath()) ("floepod-signing-{0}.json" -f [Guid]::NewGuid())
    $config = @{ bundle = @{ windows = @{ signCommand = @{
      cmd = 'pwsh'
      args = @('-NoProfile', '-File', (Join-Path $PSScriptRoot 'sign-windows-artifacts.ps1'), '-Path', '%1')
    } } } }
    $config | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $signingConfig -Encoding utf8
    $arguments += @('--config', $signingConfig)
  }
  & pnpm @arguments
  if ($LASTEXITCODE -ne 0) { throw 'Tauri bundle failed' }
  # Tauri restores the original executable after bundling. Sign this restored
  # file before MSIX/portable packaging so those formats contain final bytes.
  if ($env:FLOEPOD_SIGN_CERT_PATH) {
    & (Join-Path $PSScriptRoot 'sign-windows-artifacts.ps1') -Path 'src-tauri/target/release/FloePod.exe'
  }
  if ($isPrerelease) {
    Write-Output "Pre-release ${version}: MSI and MSIX packaging skipped"
  } else {
    & (Join-Path $PSScriptRoot 'package-msix.ps1')
  }
} finally {
  if ($signingConfig -and (Test-Path -LiteralPath $signingConfig)) {
    Remove-Item -LiteralPath $signingConfig -Force
  }
}
