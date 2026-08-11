param(
    [ValidateSet("all", "package", "native", "ffi-header", "exports")]
    [string]$Check = "all"
)

$ErrorActionPreference = "Stop"

$RepoDir = (Resolve-Path (Join-Path $PSScriptRoot "../../..")).Path
$CrateDir = Join-Path $RepoDir "rumqttc-c"
$TargetDir = Join-Path $RepoDir "target/debug"

cargo build --manifest-path (Join-Path $CrateDir "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

if ($Check -in @("all", "package")) {
$PkgConfigBuildDir = Join-Path $TargetDir "pkgconfig-check"
$PkgConfigPrefix = Join-Path $TargetDir "pkgconfig-original"
cmake -S $CrateDir -B $PkgConfigBuildDir "-DCMAKE_INSTALL_PREFIX=$PkgConfigPrefix"
if ($LASTEXITCODE -ne 0) { throw "pkg-config metadata configuration failed" }
$PkgConfig = Get-Content -Raw (Join-Path $PkgConfigBuildDir "rumqttc.pc")
if (-not $PkgConfig.Contains('prefix=${pcfiledir}/../..')) {
    throw "pkg-config prefix is not relocatable"
}
$ExpectedPrivateLibraries = "Libs.private: -lws2_32 -lbcrypt -lcrypt32 -lncrypt" +
    " -lsecur32 -luserenv -ladvapi32 -lkernel32 -lntdll"
if (-not $PkgConfig.Contains($ExpectedPrivateLibraries)) {
    throw "pkg-config metadata does not contain the Windows static-link dependencies"
}
}

if ($Check -in @("all", "native")) {
cl /nologo /W4 /WX /std:c11 "/I$CrateDir/include" `
    (Join-Path $CrateDir "tests/c/header_smoke.c") `
    "/Fe$TargetDir/rumqttc-header-smoke-c.exe" `
    /link "/LIBPATH:$TargetDir" rumqttc.dll.lib
if ($LASTEXITCODE -ne 0) { throw "C smoke build failed" }

cl /nologo /W4 /WX /EHsc /std:c++17 "/I$CrateDir/include" `
    (Join-Path $CrateDir "tests/c/header_smoke.cpp") `
    "/Fe$TargetDir/rumqttc-header-smoke-cpp.exe" `
    /link "/LIBPATH:$TargetDir" rumqttc.dll.lib
if ($LASTEXITCODE -ne 0) { throw "C++ smoke build failed" }

$env:PATH = "$TargetDir;$env:PATH"
& (Join-Path $TargetDir "rumqttc-header-smoke-c.exe")
& (Join-Path $TargetDir "rumqttc-header-smoke-cpp.exe")
}

if ($Check -in @("all", "ffi-header")) {
$GeneratedFunctions = Get-ChildItem (Join-Path $TargetDir "build") -Recurse `
    -Filter "rumqttc.generated-functions.h" |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
$GeneratedFullHeader = Get-ChildItem (Join-Path $TargetDir "build") -Recurse `
    -Filter "rumqttc.generated.h" |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
$CheckedHeader = (Join-Path $CrateDir "include/rumqttc.h").Replace('\', '/')
$GeneratedHeader = $GeneratedFunctions.FullName.Replace('\', '/')
$CompatibilitySource = Join-Path $TargetDir "rumqttc-signature-compatibility.c"
Set-Content $CompatibilitySource "#include `"$CheckedHeader`"`n#include `"$GeneratedHeader`"`n"
cl /nologo /W4 /WX /std:c11 /c $CompatibilitySource `
    "/Fo$TargetDir/rumqttc-signature-compatibility.obj"
if ($LASTEXITCODE -ne 0) { throw "generated function signatures differ from the checked header" }

$ContractTool = Join-Path $CrateDir "tests/abi/contract.py"
$CheckedContract = Join-Path $TargetDir "rumqttc-checked-contract.json"
$GeneratedContract = Join-Path $TargetDir "rumqttc-generated-contract.json"
python $ContractTool generate --header $CheckedHeader --output $CheckedContract
if ($LASTEXITCODE -ne 0) { throw "checked header contract generation failed" }
python $ContractTool generate --header $GeneratedFullHeader.FullName --output $GeneratedContract
if ($LASTEXITCODE -ne 0) { throw "generated header contract generation failed" }
python $ContractTool compare --old $CheckedContract --new $GeneratedContract `
    --mode containment --categories functions,records
if ($LASTEXITCODE -ne 0) { throw "generated C ABI types differ from the checked header" }
python $ContractTool compare --old $GeneratedContract --new $CheckedContract `
    --mode containment --categories functions,records
if ($LASTEXITCODE -ne 0) { throw "checked C ABI types differ from the generated header" }
}

if ($Check -in @("all", "exports")) {
$CheckedHeader = (Join-Path $CrateDir "include/rumqttc.h").Replace('\', '/')
$ContractTool = Join-Path $CrateDir "tests/abi/contract.py"
$Contract = Join-Path $TargetDir "rumqttc-abi-contract.json"
python $ContractTool generate --header $CheckedHeader `
    --library (Join-Path $TargetDir "rumqttc.dll") --output $Contract
if ($LASTEXITCODE -ne 0) { throw "C ABI contract generation failed" }
python $ContractTool verify-exports --contract $Contract
if ($LASTEXITCODE -ne 0) { throw "declared and exported rumqttc symbols differ" }
}
