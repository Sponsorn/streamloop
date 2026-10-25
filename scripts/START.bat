@echo off
setlocal enabledelayedexpansion
title StreamLoop 3
echo ======================================
echo   StreamLoop 3 - 24/7 YouTube Streamer
echo ======================================
echo.

:: Resolve paths relative to this batch file
set "ROOT=%~dp0"
set "NODE=%ROOT%node\node.exe"
set "APP=%ROOT%app"

call :heal

:: Pre-flight checks
if not exist "%NODE%" (
    echo [ERROR] Node.js is missing: %NODE%. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

if not exist "%APP%\src\server\index.ts" (
    echo [ERROR] The app is missing: %APP%. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

if not exist "%APP%\node_modules" (
    echo [ERROR] The app's dependencies are missing: %APP%\node_modules. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

if not exist "%ROOT%supervisor\streamloop-supervisor.exe" (
    echo [ERROR] The supervisor is missing: %ROOT%supervisor\streamloop-supervisor.exe. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

if not exist "%ROOT%ffmpeg\ffmpeg.exe" (
    echo [ERROR] ffmpeg is missing: %ROOT%ffmpeg\ffmpeg.exe. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

if not exist "%ROOT%pot\build\main.js" (
    echo [ERROR] The token server is missing: %ROOT%pot\build\main.js. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

echo Starting server...
echo.

:loop
:: Heal again on every start: a rollback in the update block below can fail too.
call :heal

:: Start the server (this blocks until the server exits)
cd /d "%APP%"
"%NODE%" node_modules\tsx\dist\cli.mjs src\server\index.ts

:: Check if server exited with code 75 (update restart). Inside the block use rem, never ::,
:: and no parentheses in comments: cmd parses it on every exit and a :: line there kills the launcher.
if %ERRORLEVEL% equ 75 (
    echo.
    cd /d "%ROOT%"

    rem Swap app directory if a staged update exists. Only app\ is swapped:
    rem the supervisor holds yt-dlp\ and its token server holds node\.
    if exist "%ROOT%_update_tmp\app" (
        echo Applying update...
        if exist "%ROOT%_update_old" rmdir /s /q "%ROOT%_update_old" 2>nul
        rem Retry rename up to 5 times: Windows may still hold file locks
        rem from the Node process that just exited
        set "RENAME_OK=0"
        for /L %%i in (1,1,5) do (
            if "!RENAME_OK!"=="0" (
                rename "%ROOT%app" _update_old 2>nul
                if not exist "%ROOT%app" (
                    set "RENAME_OK=1"
                ) else (
                    echo   Waiting for file locks to release (attempt %%i/5^)...
                    ping -n 4 127.0.0.1 >nul
                )
            )
        )
        if not exist "%ROOT%app" (
            move "%ROOT%_update_tmp\app" "%ROOT%app"
            if exist "%ROOT%app\src\server\index.ts" (
                rem Carry over config and state from old app. The server already flushed them.
                if exist "%ROOT%_update_old\config.json" copy /y "%ROOT%_update_old\config.json" "%ROOT%app\config.json" >nul
                if exist "%ROOT%_update_old\state.json" copy /y "%ROOT%_update_old\state.json" "%ROOT%app\state.json" >nul
                if exist "%ROOT%_update_old\logs" xcopy "%ROOT%_update_old\logs" "%ROOT%app\logs\" /E /I /Y >nul 2>nul
                rem Stage the new launcher only if it actually differs - never
                rem overwrite the running script in place. A .bat that replaces
                rem itself desyncs cmd's on-disk read position and usually closes
                rem the window before the relaunch. The swap is handed off below.
                if exist "%ROOT%_update_tmp\START.bat" (
                    fc /b "%ROOT%_update_tmp\START.bat" "%ROOT%START.bat" >nul 2>&1
                    if errorlevel 1 copy /y "%ROOT%_update_tmp\START.bat" "%ROOT%START.bat.new" >nul
                )
                echo Update applied successfully.

                rem Clean up temp directories only after successful swap
                if exist "%ROOT%_update_old" rmdir /s /q "%ROOT%_update_old" 2>nul
                if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul
            ) else (
                echo [ERROR] New app directory is incomplete, rolling back...
                if exist "%ROOT%app" rmdir /s /q "%ROOT%app" 2>nul
                rename "%ROOT%_update_old" app
            )
        ) else (
            echo [ERROR] Failed to rename old app directory, aborting update.
        )
    )

    rem If a new launcher was staged, hand off to a detached one-shot script that
    rem swaps START.bat and relaunches in a fresh window. The swapping script and
    rem the swapped script must never be the same file: a self-replacing .bat
    rem desyncs cmd and closes the window before it can relaunch. The trampoline
    rem finds the root from its own path, since an echoed path follows the console codepage.
    if exist "%ROOT%START.bat.new" (
        echo Applying new launcher and restarting...
        set "TRAMP=%ROOT%_apply_launcher.bat"
        > "!TRAMP!" echo @echo off
        >> "!TRAMP!" echo ping -n 3 127.0.0.1 ^>nul
        >> "!TRAMP!" echo move /y "%%~dp0START.bat.new" "%%~dp0START.bat" ^>nul
        >> "!TRAMP!" echo cd /d "%%~dp0"
        >> "!TRAMP!" echo start "StreamLoop 3" "%%~dp0START.bat"
        >> "!TRAMP!" echo del "%%~f0"
        start "" cmd /c "!TRAMP!"
        exit /b 0
    )

    echo Restarting server...
    ping -n 3 127.0.0.1 >nul
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
ping -n 11 127.0.0.1 >nul
goto loop

:: Self-heal: an interrupted swap or a failed rollback can leave app\ missing,
:: which would fail every start. Restore the old copy, or promote the staged one.
:heal
if not exist "%APP%\src\server\index.ts" (
    if exist "%ROOT%_update_old\src\server\index.ts" (
        echo [RECOVER] app missing - restoring from _update_old ...
        if exist "%APP%" rmdir /s /q "%APP%" 2>nul
        rename "%ROOT%_update_old" app
    ) else if exist "%ROOT%_update_tmp\app\src\server\index.ts" (
        echo [RECOVER] app missing - promoting staged _update_tmp\app ...
        if exist "%APP%" rmdir /s /q "%APP%" 2>nul
        move "%ROOT%_update_tmp\app" "%APP%"
    )
)
goto :eof
