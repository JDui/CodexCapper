param([Parameter(Mandatory = $true)][string]$OutDir)
$ErrorActionPreference = 'Stop'
$sourceDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'src-tauri\native'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path -LiteralPath $OutDir).Path
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $installation) { throw 'MSVC C++ tools are required for the native taskbar bridge.' }
$toolset = Get-ChildItem -LiteralPath (Join-Path $installation 'VC\Tools\MSVC') -Directory | Sort-Object Name -Descending | Select-Object -First 1
$compiler = Join-Path $toolset.FullName 'bin\Hostx64\x64\cl.exe'
$sdkBase = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
$sdk = Get-ChildItem -LiteralPath (Join-Path $sdkBase 'Include') -Directory | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'cppwinrt\winrt\base.h') } | Sort-Object Name -Descending | Select-Object -First 1
if (-not $sdk) { throw 'Windows SDK with C++/WinRT headers is required.' }
$env:PATH = (Join-Path $toolset.FullName 'bin\Hostx64\x64') + ';' + $env:PATH
$includeArgs = @('/I' + (Join-Path $toolset.FullName 'include'))
foreach ($part in @('ucrt','shared','um','winrt','cppwinrt')) { $includeArgs += '/I' + (Join-Path $sdk.FullName $part) }
$libraryArgs = @('/LIBPATH:' + (Join-Path $toolset.FullName 'lib\x64'))
foreach ($part in @('ucrt','um')) { $libraryArgs += '/LIBPATH:' + (Join-Path $sdkBase ('Lib\' + $sdk.Name + '\' + $part + '\x64')) }
$common = @('/nologo','/std:c++20','/EHsc','/MT','/O2','/W4','/utf-8','/DUNICODE','/D_UNICODE') + $includeArgs
Push-Location -LiteralPath $OutDir
try {
    & $compiler @common '/LD' (Join-Path $sourceDir 'taskbar_bridge.cpp') '/Fecodexcapper-taskbar-bridge.dll' '/Focodexcapper-taskbar-bridge.obj' '/link' ('/DEF:' + (Join-Path $sourceDir 'taskbar_bridge.def')) @libraryArgs 'windowsapp.lib' 'runtimeobject.lib' 'ole32.lib' 'oleaut32.lib' 'user32.lib'
    if ($LASTEXITCODE -ne 0) { throw "Taskbar bridge compilation failed: $LASTEXITCODE" }
    & $compiler @common (Join-Path $sourceDir 'injector.cpp') '/Fecodexcapper-taskbar-injector.exe' '/Focodexcapper-taskbar-injector.obj' '/link' @libraryArgs 'ole32.lib' 'user32.lib'
    if ($LASTEXITCODE -ne 0) { throw "Taskbar injector compilation failed: $LASTEXITCODE" }
} finally { Pop-Location }
