@echo off
setlocal enabledelayedexpansion
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
        :: Retry rename up to 5 times — Windows may still hold file locks
        :: from the just-killed mpv process
        set "RENAME_OK=0"
        for /L %%i in (1,1,5) do (
            if "!RENAME_OK!"=="0" (
                rename "%ROOT%app" _update_old 2>nul
                if not exist "%ROOT%app" (
                    set "RENAME_OK=1"
                ) else (
                    echo   Waiting for file locks to release (attempt %%i/5^)...
                    timeout /t 3 /nobreak >nul
                )
            )
        )
        if not exist "%ROOT%app" (
            move "%ROOT%_update_tmp\app" "%ROOT%app"
            if exist "%ROOT%app\src\server\index.ts" (
                :: Carry over config and state from old app (server already flushed)
                if exist "%ROOT%_update_old\config.json" copy /y "%ROOT%_update_old\config.json" "%ROOT%app\config.json" >nul
                if exist "%ROOT%_update_old\state.json" copy /y "%ROOT%_update_old\state.json" "%ROOT%app\state.json" >nul
                if exist "%ROOT%_update_old\logs" xcopy "%ROOT%_update_old\logs" "%ROOT%app\logs\" /E /I /Y >nul 2>nul
                :: Copy new START.bat if included in the update
                if exist "%ROOT%_update_tmp\START.bat" (
                    copy /y "%ROOT%_update_tmp\START.bat" "%ROOT%START.bat" >nul
                )
                echo Update applied successfully.

                :: Swap yt-dlp/ if the update bundled new binaries (yt-dlp.exe,
                :: deno.exe). YouTube rotates challenge shapes faster than our
                :: release cadence; refreshing on every update keeps things working.
                if exist "%ROOT%_update_tmp\yt-dlp" (
                    if exist "%ROOT%_update_old_ytdlp" rmdir /s /q "%ROOT%_update_old_ytdlp" 2>nul
                    set "YTDLP_OK=0"
                    for /L %%j in (1,1,5) do (
                        if "!YTDLP_OK!"=="0" (
                            rename "%ROOT%yt-dlp" _update_old_ytdlp 2>nul
                            if not exist "%ROOT%yt-dlp" (
                                set "YTDLP_OK=1"
                            ) else (
                                echo   Waiting for yt-dlp file locks (attempt %%j/5^)...
                                timeout /t 3 /nobreak >nul
                            )
                        )
                    )
                    if not exist "%ROOT%yt-dlp" (
                        move "%ROOT%_update_tmp\yt-dlp" "%ROOT%yt-dlp"
                        if exist "%ROOT%yt-dlp\yt-dlp.exe" (
                            echo yt-dlp updated.
                            if exist "%ROOT%_update_old_ytdlp" rmdir /s /q "%ROOT%_update_old_ytdlp" 2>nul
                        ) else (
                            echo [WARNING] New yt-dlp directory incomplete, rolling back yt-dlp.
                            if exist "%ROOT%yt-dlp" rmdir /s /q "%ROOT%yt-dlp" 2>nul
                            rename "%ROOT%_update_old_ytdlp" yt-dlp
                        )
                    ) else (
                        echo [WARNING] Could not rename old yt-dlp directory, keeping existing.
                    )
                )

                :: Clean up temp directories only after successful swap
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
