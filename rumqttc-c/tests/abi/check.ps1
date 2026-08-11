$ErrorActionPreference = "Stop"

$RepoDir = (Resolve-Path (Join-Path $PSScriptRoot "../../..")).Path
$CrateDir = Join-Path $RepoDir "rumqttc-c"
$TargetDir = Join-Path $RepoDir "target/debug"

cargo build --manifest-path (Join-Path $CrateDir "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

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

$GeneratedFunctions = Get-ChildItem (Join-Path $TargetDir "build") -Recurse `
    -Filter "rumqttc.generated-functions.h" |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
$CheckedHeader = (Join-Path $CrateDir "include/rumqttc.h").Replace('\', '/')
$GeneratedHeader = $GeneratedFunctions.FullName.Replace('\', '/')
$CompatibilitySource = Join-Path $TargetDir "rumqttc-signature-compatibility.c"
Set-Content $CompatibilitySource "#include `"$CheckedHeader`"`n#include `"$GeneratedHeader`"`n"
cl /nologo /W4 /WX /std:c11 /c $CompatibilitySource `
    "/Fo$TargetDir/rumqttc-signature-compatibility.obj"
if ($LASTEXITCODE -ne 0) { throw "generated function signatures differ from the checked header" }

$CheckedFunctions = Select-String -Path $CheckedHeader -Pattern 'rumqttc_[A-Za-z0-9_]+\(' -AllMatches |
    ForEach-Object { $_.Matches.Value.TrimEnd('(') } |
    Sort-Object -Unique
$GeneratedFunctionNames = Select-String -Path $GeneratedHeader -Pattern 'rumqttc_[A-Za-z0-9_]+\(' -AllMatches |
    ForEach-Object { $_.Matches.Value.TrimEnd('(') } |
    Sort-Object -Unique
$DeclarationDifference = Compare-Object $CheckedFunctions $GeneratedFunctionNames
if ($DeclarationDifference) {
    $DeclarationDifference | Format-Table | Out-String | Write-Error
    throw "generated declaration names differ from the checked header"
}

$Expected = Get-Content (Join-Path $CrateDir "tests/abi/rumqttc-v1.symbols")
$Actual = dumpbin /nologo /exports (Join-Path $TargetDir "rumqttc.dll") |
    Select-String -Pattern '\srumqttc_[A-Za-z0-9_]+$' |
    ForEach-Object { ($_ -split '\s+')[-1] } |
    Sort-Object -Unique
$Difference = Compare-Object $Expected $Actual
if ($Difference) {
    $Difference | Format-Table | Out-String | Write-Error
    throw "exported symbols differ from the ABI-v1 baseline"
}
