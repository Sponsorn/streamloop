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

:: Install parts a full update swaps, in swap order, and the file in each that shows it is whole.
:: launcher.test.ts holds these to SWAP_FOLDERS and PART_KEY_FILES in src\server\components.js.
set "PARTS=node supervisor ffmpeg pot yt-dlp"
set "KEY_node=node.exe"
set "KEY_supervisor=streamloop-supervisor.exe"
set "KEY_ffmpeg=ffmpeg.exe"
set "KEY_pot=build\main.js"
set "KEY_yt-dlp=plugins\bgutil-ytdlp-pot-provider.zip"
:: One "<folder> <status> <detail>" line per swap event, read by the dashboard.
set "SWAP_LOG=%ROOT%data\update-swap.log"
:: Exists from the first part rename until the app swap is done, so heal can tell a cut-off swap.
set "SWAPPING=%ROOT%data\update-swapping"
:: Exists from a part swap until the new version passes its health check, when Node removes it.
set "CHECKING=%ROOT%data\update-checking"
set "CHECK_CRASHES=0"

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

:: A launcher that replaced itself through the trampoline gets the start that was due next.
set "STREAMLOOP_START=first"
for %%A in (restart update rollback) do if /i "%~1"=="%%A" set "STREAMLOOP_START=%%A"

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
    set "NEXT_START=restart"
    set "RESTORE_FAILED=0"

    rem Parts first, app last. A part that fails puts every swapped part back and skips app.
    call :swap_parts
    if "!PARTS_RESULT!"=="failed" (
        echo Starting the previous version...
        set "STREAMLOOP_START=rollback"
        goto loop
    )
    if "!PARTS_RESULT!"=="swapped" set "NEXT_START=update"

    rem Swap app directory if a staged update exists
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
            move "%ROOT%_update_tmp\app" "%ROOT%app" >nul
            if exist "%ROOT%app\src\server\index.ts" (
                rem Carry over config and logs from old app. Playback state lives in data, outside app.
                if exist "%ROOT%_update_old\config.json" copy /y "%ROOT%_update_old\config.json" "%ROOT%app\config.json" >nul
                if exist "%ROOT%_update_old\logs" xcopy "%ROOT%_update_old\logs" "%ROOT%app\logs\" /E /I /Y >nul 2>nul
                rem Stage the new launcher only if it actually differs - never
                rem overwrite the running script in place. A .bat that replaces
                rem itself desyncs cmd's on-disk read position and usually closes
                rem the window before the relaunch. The swap is handed off below.
                if exist "%ROOT%_update_tmp\START.bat" (
                    fc /b "%ROOT%_update_tmp\START.bat" "%ROOT%START.bat" >nul 2>&1
                    if errorlevel 1 copy /y "%ROOT%_update_tmp\START.bat" "%ROOT%START.bat.new" >nul
                )
                if "!PARTS_RESULT!"=="swapped" call :log app ok swapped
                if exist "%SWAPPING%" del /f /q "%SWAPPING%"
                echo Update applied successfully.

                rem Clean up temp directories only after successful swap.
                rem After a part swap the old app stays in _update_old for an exit 76 rollback.
                if not "!PARTS_RESULT!"=="swapped" if exist "%ROOT%_update_old" rmdir /s /q "%ROOT%_update_old" 2>nul
                if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul
            ) else (
                echo [ERROR] New app directory is incomplete, rolling back...
                if exist "%ROOT%app" rmdir /s /q "%ROOT%app" 2>nul
                rename "%ROOT%_update_old" app
                call :app_failed the new app is incomplete
            )
        ) else (
            echo [ERROR] Failed to rename old app directory, aborting update.
            if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul
            call :app_failed the app folder stayed in use
        )
    )
    rem A restore that failed keeps the marker, so heal tries again on the next start.
    if "!RESTORE_FAILED!"=="0" if exist "%SWAPPING%" del /f /q "%SWAPPING%"
    rem Written here so a new Node that crashes before its check starts still counts toward a rollback.
    if "!NEXT_START!"=="update" type nul > "%CHECKING%"
    if "!NEXT_START!"=="rollback" if exist "%CHECKING%" del /f /q "%CHECKING%"

    rem If a new launcher was staged, hand off to a detached one-shot script that
    rem swaps START.bat and relaunches in a fresh window. The swapping script and
    rem the swapped script must never be the same file: a self-replacing .bat
    rem desyncs cmd and closes the window before it can relaunch. The trampoline
    rem finds the root from its own path, since an echoed path follows the console codepage.
    rem It passes on the start that is due, a plain word: update after a part swap, else restart.
    if exist "%ROOT%START.bat.new" (
        echo Applying new launcher and restarting...
        set "TRAMP=%ROOT%_apply_launcher.bat"
        > "!TRAMP!" echo @echo off
        >> "!TRAMP!" echo ping -n 3 127.0.0.1 ^>nul
        >> "!TRAMP!" echo move /y "%%~dp0START.bat.new" "%%~dp0START.bat" ^>nul
        >> "!TRAMP!" echo cd /d "%%~dp0"
        >> "!TRAMP!" echo start "StreamLoop 3" "%%~dp0START.bat" !NEXT_START!
        >> "!TRAMP!" echo del "%%~f0"
        start "" cmd /c "!TRAMP!"
        exit /b 0
    )

    echo Restarting server...
    ping -n 3 127.0.0.1 >nul
    set "STREAMLOOP_START=!NEXT_START!"
    goto loop
)

:: Exit code 0 = clean shutdown, don't restart
if %ERRORLEVEL% equ 0 (
    echo.
    echo Server stopped cleanly.
    pause
    exit /b 0
)

:: Exit code 76 = the new version failed its health check after a part swap
if %ERRORLEVEL% equ 76 goto rollback

:: Any other exit code = crash, auto-restart after delay. While the new version runs its
:: health check, the third crash in a row puts the previous version back instead.
set "EXIT_CODE=%ERRORLEVEL%"
set "NEXT_START=restart"
if exist "%CHECKING%" (
    set /a CHECK_CRASHES+=1
    if !CHECK_CRASHES! geq 3 (
        echo.
        echo Server crashed during the update check for the third time, exit code %EXIT_CODE%.
        goto rollback
    )
    set "NEXT_START=update"
) else (
    set "CHECK_CRASHES=0"
)
echo.
echo Server crashed (exit code %EXIT_CODE%). Restarting in 10 seconds...
ping -n 11 127.0.0.1 >nul
set "STREAMLOOP_START=%NEXT_START%"
goto loop

:: Put back every part and the app, carrying the newer config.json and logs over; the marker lets
:: heal finish a rollback cut off half-way. Nothing here relies on the restored app's update code.
:rollback
echo.
echo Putting the previous version back...
cd /d "%ROOT%"
if not exist "%ROOT%data" mkdir "%ROOT%data"
type nul > "%SWAPPING%"
set "RESTORE_FAILED=0"
call :restore_parts
call :restore_app
if exist "%CHECKING%" del /f /q "%CHECKING%"
if "!RESTORE_FAILED!"=="0" del /f /q "%SWAPPING%" 2>nul
set "CHECK_CRASHES=0"
echo Starting the previous version...
ping -n 3 127.0.0.1 >nul
set "STREAMLOOP_START=rollback"
goto loop

:: Self-heal: an interrupted swap or a failed rollback can leave app\ or a part folder missing,
:: which would fail every start. Restore the old copy, or promote the staged one.
:heal
cd /d "%ROOT%"
:: A swap or rollback cut off by a closed window or a power cut would mix old and new parts:
:: put the old ones back.
if exist "%SWAPPING%" (
    echo [RECOVER] An update or rollback was cut off while it replaced files - putting the previous version back ...
    set "RESTORE_FAILED=0"
    call :restore_parts
    call :restore_app
    if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul
    if exist "%ROOT%START.bat.new" del /f /q "%ROOT%START.bat.new"
    if exist "%CHECKING%" del /f /q "%CHECKING%"
    if "!RESTORE_FAILED!"=="0" del /f /q "%SWAPPING%" 2>nul
)
if not exist "%APP%\src\server\index.ts" (
    if exist "%ROOT%_update_old\src\server\index.ts" (
        echo [RECOVER] app missing - restoring from _update_old ...
        if exist "%APP%" rmdir /s /q "%APP%" 2>nul
        rename "%ROOT%_update_old" app
    ) else if exist "%ROOT%_update_tmp\app\src\server\index.ts" (
        echo [RECOVER] app missing - promoting staged _update_tmp\app ...
        if exist "%APP%" rmdir /s /q "%APP%" 2>nul
        move "%ROOT%_update_tmp\app" "%APP%"
    ) else if exist "%ROOT%_update_bad_app\src\server\index.ts" (
        echo [RECOVER] app missing - taking back _update_bad_app ...
        if exist "%APP%" rmdir /s /q "%APP%" 2>nul
        rename "%ROOT%_update_bad_app" app
    )
)
for %%F in (%PARTS%) do call :heal_part %%F
goto :eof

:heal_part
if exist "%ROOT%%~1\!KEY_%~1!" goto :eof
if exist "%ROOT%_update_old_%~1\!KEY_%~1!" (
    echo [RECOVER] %~1 missing - restoring from _update_old_%~1 ...
    if exist "%ROOT%%~1" rmdir /s /q "%ROOT%%~1" 2>nul
    rename "%ROOT%_update_old_%~1" %~1
    goto :eof
)
if exist "%ROOT%_update_tmp\%~1\!KEY_%~1!" (
    echo [RECOVER] %~1 missing - promoting staged _update_tmp\%~1 ...
    if exist "%ROOT%%~1" rmdir /s /q "%ROOT%%~1" 2>nul
    move "%ROOT%_update_tmp\%~1" "%ROOT%%~1" >nul
    goto :eof
)
if exist "%ROOT%_update_bad_%~1\!KEY_%~1!" (
    echo [RECOVER] %~1 missing - taking back _update_bad_%~1 ...
    if exist "%ROOT%%~1" rmdir /s /q "%ROOT%%~1" 2>nul
    rename "%ROOT%_update_bad_%~1" %~1
)
goto :eof

:: Swap every part staged in _update_tmp, in PARTS order. Sets PARTS_RESULT to none, swapped or failed.
:swap_parts
set "PARTS_RESULT=none"
set "STAGED="
for %%F in (%PARTS%) do if exist "%ROOT%_update_tmp\%%F" set "STAGED=!STAGED! %%F"
if not defined STAGED goto :eof
echo Replacing install parts:%STAGED%
if not exist "%ROOT%data" mkdir "%ROOT%data"
if exist "%SWAP_LOG%" del /f /q "%SWAP_LOG%"
set "CHECK_CRASHES=0"
call :clear_leftovers
:: Nothing is renamed yet, so there is nothing to put back: the current version starts as it is.
if "%REFUSED%"=="1" (
    echo [ERROR] Copies left by an earlier update are in the way. The update is called off.
    if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul
    if exist "%CHECKING%" del /f /q "%CHECKING%"
    set "PARTS_RESULT=failed"
    goto :eof
)
type nul > "%SWAPPING%"
:: Nothing may run from a folder being replaced, whether it holds the folder or not.
call :end_processes %STAGED%
set "SWAP_FAILED=0"
for %%F in (%STAGED%) do if "!SWAP_FAILED!"=="0" call :swap_one %%F
if "%SWAP_FAILED%"=="0" (
    set "PARTS_RESULT=swapped"
    goto :eof
)
echo [ERROR] An install part could not be replaced. Putting the previous version back...
call :restore_parts
if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul
if "!RESTORE_FAILED!"=="0" if exist "%SWAPPING%" del /f /q "%SWAPPING%"
if exist "%CHECKING%" del /f /q "%CHECKING%"
set "PARTS_RESULT=failed"
goto :eof

:: A rollback puts back every _update_old_ copy it finds, so copies left by an earlier update
:: must go first. heal ran before this Node started, so the live folders are whole. Sets REFUSED.
:clear_leftovers
set "REFUSED=0"
for %%F in (%PARTS%) do (
    for %%P in (_update_old_ _update_bad_) do (
        if exist "%ROOT%%%P%%F" if exist "%ROOT%%%F\!KEY_%%F!" rmdir /s /q "%ROOT%%%P%%F" 2>nul
        if exist "%ROOT%%%P%%F" (
            call :log %%F failed %%P%%F from an earlier update could not be removed
            set "REFUSED=1"
        )
    )
)
for %%P in (_update_old _update_bad_app) do (
    if exist "%ROOT%%%P" if exist "%APP%\src\server\index.ts" rmdir /s /q "%ROOT%%%P" 2>nul
    if exist "%ROOT%%%P" (
        call :log app failed %%P from an earlier update could not be removed
        set "REFUSED=1"
    )
)
goto :eof

:: ffmpeg and the supervisor are run by Node's health check, so only node.exe is run here.
:swap_one
call :rename_tries "%~1" "_update_old_%~1" %~1
if "!RENAME_OK!"=="0" (
    call :log %~1 failed the folder stayed in use after 3 tries
    set "SWAP_FAILED=1"
    goto :eof
)
move "%ROOT%_update_tmp\%~1" "%ROOT%%~1" >nul 2>nul
if not exist "%ROOT%%~1\!KEY_%~1!" (
    call :log %~1 failed the new folder has no !KEY_%~1!
    set "SWAP_FAILED=1"
    goto :eof
)
if /i "%~1"=="node" (
    "%ROOT%node\node.exe" --version >nul 2>&1
    if errorlevel 1 (
        call :log node failed the new node.exe does not run
        set "SWAP_FAILED=1"
        goto :eof
    )
)
if /i "%~1"=="yt-dlp" if exist "%ROOT%_update_old_yt-dlp\yt-dlp.exe" (
    rem yt-dlp updates itself, so the installed yt-dlp.exe is newer than the one in the ZIP.
    copy /y "%ROOT%_update_old_yt-dlp\yt-dlp.exe" "%ROOT%yt-dlp\yt-dlp.exe" >nul
    if errorlevel 1 (
        call :log yt-dlp failed could not keep the installed yt-dlp.exe
        set "SWAP_FAILED=1"
        goto :eof
    )
)
call :log %~1 ok swapped
goto :eof

:: Rename ROOT\%1 to %2 in 3 tries, ending what runs from ROOT\%1 before tries 2 and 3.
:: %3 names the part in the swap log. Sets RENAME_OK.
:rename_tries
set "RENAME_OK=0"
for /L %%i in (1,1,3) do (
    if "!RENAME_OK!"=="0" (
        if %%i gtr 1 (
            echo   %~3 is in use - ending what runs from it, try %%i of 3 ...
            call :log %~3 retry try %%i of 3 after a failed rename
            call :end_processes %~1
            ping -n 4 127.0.0.1 >nul
        )
        rename "%ROOT%%~1" %~2 2>nul
        if not exist "%ROOT%%~1" set "RENAME_OK=1"
    )
)
goto :eof

:: Ends what runs from ROOT\<folder>\ for each folder named. Paths go through the environment, where
:: a quote or a non-ASCII letter cannot break them. No -WindowStyle Hidden: it would hide this window.
:end_processes
set "SL_DIRS=%*"
set "SL_ROOT=%ROOT%"
set "SL_LOG=%SWAP_LOG%"
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "$dirs = @($env:SL_DIRS -split ' ' | Where-Object { $_ }); Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath } | ForEach-Object { $p = $_; foreach ($d in $dirs) { if ($p.ExecutablePath.StartsWith($env:SL_ROOT + $d + '\', [StringComparison]::OrdinalIgnoreCase)) { $n = $d -replace '^_update_(old|bad)_', ''; if ($n -eq '_update_old') { $n = 'app' }; try { Stop-Process -Id $p.ProcessId -Force -ErrorAction Stop; Wait-Process -Id $p.ProcessId -Timeout 10 -ErrorAction SilentlyContinue; [IO.File]::AppendAllText($env:SL_LOG, $n + ' killed ' + $p.ExecutablePath.Substring($env:SL_ROOT.Length) + ' pid ' + $p.ProcessId + [Environment]::NewLine) } catch {}; break } } }" >nul 2>&1
goto :eof

:restore_parts
set "AWAY="
for %%F in (%PARTS%) do if exist "%ROOT%_update_old_%%F" if exist "%ROOT%%%F" set "AWAY=!AWAY! %%F"
if defined AWAY call :end_processes %AWAY%
for %%F in (%PARTS%) do if exist "%ROOT%_update_old_%%F" call :restore_one %%F
goto :eof

:: Sets RESTORE_FAILED when a folder stays in use, so the marker stays and heal tries again.
:restore_one
if exist "%ROOT%%~1" (
    if exist "%ROOT%_update_bad_%~1" rmdir /s /q "%ROOT%_update_bad_%~1" 2>nul
    call :rename_tries "%~1" "_update_bad_%~1" %~1
    if "!RENAME_OK!"=="0" (
        call :log %~1 failed the new version stayed in use and could not be put back
        set "RESTORE_FAILED=1"
        goto :eof
    )
)
call :rename_tries "_update_old_%~1" "%~1" %~1
if exist "%ROOT%%~1\!KEY_%~1!" (
    call :log %~1 restored the previous version
    if exist "%ROOT%_update_bad_%~1" rmdir /s /q "%ROOT%_update_bad_%~1" 2>nul
    goto :eof
)
:: The previous copy did not come back whole: keep the current one rather than none.
if "!RENAME_OK!"=="0" (
    call :log %~1 failed the previous version stayed in use and could not be put back
    set "RESTORE_FAILED=1"
) else (
    call :log %~1 failed the previous version is incomplete so the current one stays
    rename "%ROOT%%~1" _update_old_%~1 2>nul
)
if not exist "%ROOT%%~1" if exist "%ROOT%_update_bad_%~1" rename "%ROOT%_update_bad_%~1" %~1 2>nul
goto :eof

:: Puts back the app in _update_old, then carries the newer config.json and logs over from
:: _update_bad_app, also when a rollback cut off after the rename left that step undone.
:restore_app
if exist "%ROOT%_update_old\src\server\index.ts" (
    if exist "%APP%" (
        if exist "%ROOT%_update_bad_app" rmdir /s /q "%ROOT%_update_bad_app" 2>nul
        call :end_processes app
        call :rename_tries "app" "_update_bad_app" app
        if "!RENAME_OK!"=="0" (
            call :log app failed the new version stayed in use and could not be put back
            set "RESTORE_FAILED=1"
            goto :eof
        )
    )
    call :rename_tries "_update_old" "app" app
    if "!RENAME_OK!"=="0" (
        call :log app failed the previous version stayed in use and could not be put back
        set "RESTORE_FAILED=1"
        if not exist "%APP%" if exist "%ROOT%_update_bad_app" rename "%ROOT%_update_bad_app" app 2>nul
        goto :eof
    )
)
if not exist "%ROOT%_update_bad_app" goto :eof
if not exist "%APP%\src\server\index.ts" goto :eof
:: When config.json cannot be carried over, _update_bad_app stays, so the newer settings are not lost.
if exist "%ROOT%_update_bad_app\config.json" (
    copy /y "%ROOT%_update_bad_app\config.json" "%APP%\config.json" >nul
    if errorlevel 1 (
        call :log app failed could not carry config.json into the previous version
        goto :eof
    )
)
if exist "%ROOT%_update_bad_app\logs" xcopy "%ROOT%_update_bad_app\logs" "%APP%\logs\" /E /I /Y >nul 2>nul
if exist "%ROOT%_update_bad_app" rmdir /s /q "%ROOT%_update_bad_app" 2>nul
call :log app restored the previous version
goto :eof

:: The app swap failed after parts were swapped: put the parts back too, so old and new never mix.
:app_failed
if not "!PARTS_RESULT!"=="swapped" goto :eof
call :log app failed %*
echo Putting the previous install parts back...
call :restore_parts
if exist "%ROOT%_update_tmp" rmdir /s /q "%ROOT%_update_tmp" 2>nul
set "NEXT_START=rollback"
goto :eof

:log
if not exist "%ROOT%data" mkdir "%ROOT%data"
>> "%SWAP_LOG%" echo %*
goto :eof
