@echo off
rem Builds the Rust version. The GNU toolchain (no Visual Studio) calls dlltool, which needs
rem the assembler as.exe; the minimal Rust install has neither. They are taken from MSYS2
rem (MinGW-w64) for the duration of the build only. Another MSYS2 folder: set MINGW=...
rem Arguments go to cargo: build.cmd (release build), build.cmd test, build.cmd run, etc.
setlocal
if not defined MINGW set "MINGW=C:\msys64\mingw64\bin"
if not exist "%MINGW%\as.exe" goto :no_mingw
set "PATH=%MINGW%;%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0"
if "%~1"=="" (cargo build --release) else (cargo %*)
exit /b %errorlevel%

:no_mingw
echo as.exe not found in %MINGW%
echo Install MSYS2 with the mingw-w64-x86_64-binutils package or set MINGW=^<folder with as.exe^>
exit /b 1
