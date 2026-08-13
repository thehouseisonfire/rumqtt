$ErrorActionPreference = "Stop"

$OsArchitecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
$ProcessArchitecture = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture
if ($OsArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64 -or
    $ProcessArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
    throw "unsupported Windows ABI comparison host: OS $OsArchitecture, process $ProcessArchitecture; expected X64"
}

$WorkspaceDir = (Resolve-Path (Join-Path $PSScriptRoot "../../..")).Path
$CrateDir = Join-Path $WorkspaceDir "c"
$ReportDir = if ($env:ABI_REPORT_DIR) { $env:ABI_REPORT_DIR } else { Join-Path $WorkspaceDir "target/abi-reports" }
$VersionMatch = Select-String -Path (Join-Path $CrateDir "Cargo.toml") `
    -Pattern '^version = "([^"]+)"'
$Version = $VersionMatch.Matches[0].Groups[1].Value
$Header = Join-Path $CrateDir "include/rumqttc.h"
$AbiMajor = (Select-String -Path $Header -Pattern 'RUMQTTC_ABI_VERSION_MAJOR (\d+)u').Matches[0].Groups[1].Value
$AbiMinor = (Select-String -Path $Header -Pattern 'RUMQTTC_ABI_VERSION_MINOR (\d+)u').Matches[0].Groups[1].Value
$AbiLine = if ($AbiMajor -eq "0") { "0_$AbiMinor" } else { $AbiMajor }

New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null
python (Join-Path $CrateDir "tests/abi/baseline.py") `
    --version $Version --platform windows-x86_64 --output (Join-Path $ReportDir "baseline")
if ($LASTEXITCODE -ne 0) { throw "baseline resolution failed" }
if (Test-Path (Join-Path $ReportDir "baseline/no-baseline")) { exit 0 }

cargo build --locked --release --manifest-path (Join-Path $WorkspaceDir "Cargo.toml") -p rumqttc-c-next
if ($LASTEXITCODE -ne 0) { throw "release C library build failed" }
$VersionedDll = Join-Path $WorkspaceDir "target/release/rumqttc-$AbiLine.dll"
Copy-Item (Join-Path $WorkspaceDir "target/release/rumqttc.dll") $VersionedDll -Force
$CurrentContract = Join-Path $ReportDir "current-contract.json"
python (Join-Path $CrateDir "tests/abi/contract.py") generate `
    --header $Header --library $VersionedDll --package-version $Version `
    --target windows-x86_64 --output $CurrentContract
if ($LASTEXITCODE -ne 0) { throw "current ABI contract generation failed" }
$Baseline = Get-Content -Raw (Join-Path $ReportDir "baseline/baseline.json") | ConvertFrom-Json
python (Join-Path $CrateDir "tests/abi/contract.py") compare `
    --old $Baseline.contract --new $CurrentContract --mode containment
if ($LASTEXITCODE -ne 0) { throw "historical C ABI compatibility failed" }
