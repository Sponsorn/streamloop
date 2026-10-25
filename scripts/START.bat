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

call :heal

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

    rem Swap app directory if a staged update exists
    if exist "%ROOT%_update_tmp\app" (
        echo Applying update...
        if exist "%ROOT%_update_old" rmdir /s /q "%ROOT%_update_old" 2>nul
        rem Retry rename up to 5 times: Windows may still hold file locks
        rem from the just-killed mpv process
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

                rem Swap yt-dlp/ if the update bundled new binaries: yt-dlp.exe and
                rem deno.exe. YouTube rotates challenge shapes faster than our
                rem release cadence; refreshing on every update keeps things working.
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

                rem Swap node/ if the update staged a new Node runtime. The server
                rem has exited and this script runs in cmd, so nothing holds node.exe.
                if exist "%ROOT%_update_tmp\node\node.exe" (
                    if exist "%ROOT%_update_old_node" rmdir /s /q "%ROOT%_update_old_node" 2>nul
                    set "NODE_OK=0"
                    for /L %%k in (1,1,5) do (
                        if "!NODE_OK!"=="0" (
                            rename "%ROOT%node" _update_old_node 2>nul
                            if not exist "%ROOT%node" (
                                set "NODE_OK=1"
                            ) else (
                                echo   Waiting for Node file locks (attempt %%k/5^)...
                                timeout /t 3 /nobreak >nul
                            )
                        )
                    )
                    if not exist "%ROOT%node" (
                        move "%ROOT%_update_tmp\node" "%ROOT%node"
                        "%ROOT%node\node.exe" --version >nul 2>&1
                        if errorlevel 1 (
                            echo [WARNING] New Node does not run, rolling back Node.
                            if exist "%ROOT%node" rmdir /s /q "%ROOT%node" 2>nul
                            set "BACK_OK=0"
                            for /L %%m in (1,1,5) do (
                                if "!BACK_OK!"=="0" (
                                    rename "%ROOT%_update_old_node" node 2>nul
                                    if exist "%ROOT%node\node.exe" (
                                        set "BACK_OK=1"
                                    ) else (
                                        echo   Waiting to restore the old Node (attempt %%m/5^)...
                                        timeout /t 3 /nobreak >nul
                                    )
                                )
                            )
                        ) else (
                            echo Node updated.
                            if exist "%ROOT%_update_old_node" rmdir /s /q "%ROOT%_update_old_node" 2>nul
                        )
                    ) else (
                        echo [WARNING] Could not rename old node directory, keeping existing.
                    )
                )

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
    rem desyncs cmd and closes the window before it can relaunch.
    if exist "%ROOT%START.bat.new" (
        echo Applying new launcher and restarting...
        set "TRAMP=%ROOT%_apply_launcher.bat"
        > "!TRAMP!" echo @echo off
        >> "!TRAMP!" echo timeout /t 2 /nobreak ^>nul
        >> "!TRAMP!" echo move /y "%ROOT%START.bat.new" "%ROOT%START.bat" ^>nul
        >> "!TRAMP!" echo cd /d "%ROOT%"
        >> "!TRAMP!" echo start "StreamLoop" "%ROOT%START.bat"
        >> "!TRAMP!" echo del "%%~f0"
        start "" cmd /c "!TRAMP!"
        exit /b 0
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

:: Self-heal: an interrupted swap or a failed rollback can leave app\ or node\
:: missing, which would fail every start. Restore the old copy, or promote the staged one.
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
if exist "%ROOT%node\node.exe" goto :eof
if exist "%ROOT%_update_old_node\node.exe" (
    echo [RECOVER] node missing - restoring from _update_old_node ...
    if exist "%ROOT%node" rmdir /s /q "%ROOT%node" 2>nul
    rename "%ROOT%_update_old_node" node
    goto :eof
)
if exist "%ROOT%_update_tmp\node\node.exe" (
    "%ROOT%_update_tmp\node\node.exe" --version >nul 2>&1
    if not errorlevel 1 (
        echo [RECOVER] node missing - promoting staged _update_tmp\node ...
        if exist "%ROOT%node" rmdir /s /q "%ROOT%node" 2>nul
        move "%ROOT%_update_tmp\node" "%ROOT%node"
    )
)
goto :eof
