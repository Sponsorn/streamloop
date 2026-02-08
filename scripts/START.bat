@echo off
title Freeze Monitor
echo ============================================
echo   Freeze Monitor - YouTube Playlist Monitor
echo ============================================
echo.

:: Resolve paths relative to this batch file
set "ROOT=%~dp0"
set "NODE=%ROOT%node\node.exe"
set "APP=%ROOT%app"

:: Pre-flight checks
if not exist "%NODE%" (
    echo [ERROR] Portable Node.js not found at: %NODE%
    echo Please ensure the node\ folder is present.
    pause
    exit /b 1
)

if not exist "%APP%\src\server\index.ts" (
    echo [ERROR] Application files not found at: %APP%
    echo Please ensure the app\ folder is present.
    pause
    exit /b 1
)

if not exist "%APP%\node_modules" (
    echo [ERROR] Dependencies not installed. node_modules folder missing.
    pause
    exit /b 1
)

echo Starting server...
echo.

:loop
:: Start the server (this blocks until the server exits)
cd /d "%APP%"
"%NODE%" node_modules\tsx\dist\cli.mjs src\server\index.ts

:: Check if server exited with code 75 (update restart)
if %ERRORLEVEL% equ 75 (
    echo.
    echo Update applied, restarting server...
    cd /d "%ROOT%"
    if exist "%ROOT%_update_old" (
        rmdir /s /q "%ROOT%_update_old" 2>nul
    )
    if exist "%ROOT%_update_tmp" (
        rmdir /s /q "%ROOT%_update_tmp" 2>nul
    )
    timeout /t 2 /nobreak >nul
    goto loop
)

:: If we get here, the server has exited normally
echo.
echo Server has stopped.
pause
