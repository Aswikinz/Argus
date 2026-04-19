@echo off
:: Build the Argus container image and optionally extract the compiled binary
:: to .\dist\ for native use on Linux / WSL2.
::
:: Usage:
::   podman-build.bat                         no OCR  ->  argus:latest
::   podman-build.bat --features ocr          Tesseract  ->  argus:ocr
::   podman-build.bat --features ocrs         ONNX  ->  argus:ocrs
::   podman-build.bat --features ocrs --extract   build + copy binary to .\dist\
::
:: Note: The extracted binary is a Linux ELF. Run it on Linux or via WSL2.
::       Use "podman run" directly to run on this Windows host.

setlocal EnableDelayedExpansion

echo.
echo      =====================================
echo       Argus  ^|  Podman Build Script
echo      =====================================
echo.

set FEATURES=
set EXTRACT=false
set PLATFORM=linux/amd64
set TAG_BASE=argus

:parse
if "%~1"=="" goto done_parse
if /i "%~1"=="--features" ( set FEATURES=%~2 & shift & shift & goto parse )
if /i "%~1"=="--extract"  ( set EXTRACT=true & shift & goto parse )
if /i "%~1"=="--arm64"    ( set PLATFORM=linux/arm64 & shift & goto parse )
if /i "%~1"=="--platform" ( set PLATFORM=%~2 & shift & shift & goto parse )
if /i "%~1"=="--tag"      ( set TAG_BASE=%~2 & shift & shift & goto parse )
if /i "%~1"=="--help"     goto show_help
if /i "%~1"=="-h"         goto show_help
echo [ERROR] Unknown argument: %~1
exit /b 1

:show_help
echo Usage: podman-build.bat [OPTIONS]
echo.
echo Options:
echo   --features ^<FEAT^>   Build variant: (empty default), ocr, or ocrs
echo   --extract            Copy built binary to .\dist\argus
echo   --arm64              Build for linux/arm64
echo   --platform ^<PLAT^>   Target platform (default: linux/amd64)
echo   --tag ^<NAME^>        Image base name (default: argus)
echo.
echo Examples:
echo   podman-build.bat
echo   podman-build.bat --features ocr
echo   podman-build.bat --features ocrs --extract
exit /b 0

:done_parse

where podman >nul 2>nul
if %errorlevel% neq 0 (
    echo [ERROR] podman not found.
    echo Install from https://podman.io/getting-started/installation
    exit /b 1
)
for /f "tokens=*" %%v in ('podman --version') do echo [OK] %%v
echo.

if "!FEATURES!"=="" (
    set IMAGE_TAG=!TAG_BASE!:latest
) else (
    set IMAGE_TAG=!TAG_BASE!:!FEATURES!
)

echo Build configuration:
echo   Image    : !IMAGE_TAG!
echo   Features : !FEATURES!
echo   Platform : !PLATFORM!
echo   Extract  : !EXTRACT!
echo.

echo Building image...
echo.

podman build ^
    --file Containerfile ^
    --tag !IMAGE_TAG! ^
    --platform !PLATFORM! ^
    --build-arg "FEATURES=!FEATURES!" ^
    .

if %errorlevel% neq 0 (
    echo [ERROR] Build failed.
    exit /b 1
)

echo.
echo [OK] Build complete: !IMAGE_TAG!
echo.

if "!EXTRACT!"=="true" (
    echo Extracting binary to .\dist\ ...
    if not exist "dist\" mkdir dist

    set CNAME=argus-extract-%RANDOM%
    podman create --name !CNAME! !IMAGE_TAG! >nul
    podman cp !CNAME!:/usr/local/bin/argus .\dist\argus
    podman rm !CNAME! >nul

    echo [OK] Extracted: .\dist\argus
    echo.
    echo NOTE: This is a Linux ELF binary.
    echo   Linux : ./dist/argus "pattern"
    echo   WSL2  : wsl ./dist/argus "pattern"
    echo   Windows host: use podman run instead (see below)
    echo.
)

echo Run in container:
echo.
echo   Non-interactive:
echo     podman run --rm -v "%CD%:/data" !IMAGE_TAG! /data "pattern"
echo.
echo   Interactive TUI:
echo     podman run -it --rm -v "%CD%:/data" !IMAGE_TAG!
echo.
echo [DONE]
endlocal
