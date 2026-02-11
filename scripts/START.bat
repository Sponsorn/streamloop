@echo off
title StreamLoop
echo ======================================
echo   StreamLoop - 24/7 YouTube Streamer
echo ======================================
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
    cd /d "%ROOT%"

    :: Swap app directory if a staged update exists
    if exist "%ROOT%_update_tmp\app" (
        echo Applying update...
        if exist "%ROOT%_update_old" rmdir /s /q "%ROOT%_update_old" 2>nul
        rename "%ROOT%app" _update_old
        move "%ROOT%_update_tmp\app" "%ROOT%app"
        :: Copy new START.bat if included in the update
        if exist "%ROOT%_update_tmp\START.bat" (
            copy /y "%ROOT%_update_tmp\START.bat" "%ROOT%START.bat" >nul
        )
        echo Update applied successfully.
    )

    :: Clean up temp directories
    if exist "%ROOT%_update_old" rmdir /s /q "%ROOT%_update_old" 2>nul
    if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul

    echo Restarting server...
    timeout /t 2 /nobreak >nul
    goto loop
)

:: Exit code 0 = clean shutdown, don't restart
if %ERRORLEVEL% equ 0 (
    echo.
    echo Server stopped cleanly.
    pause
    exit /b 0
)

:: Any other exit code = crash, auto-restart after delay
echo.
echo Server crashed (exit code %ERRORLEVEL%). Restarting in 10 seconds...
timeout /t 10 /nobreak >nul
goto loop
