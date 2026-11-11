@echo off
setlocal
title StreamLoop 3
echo ======================================
echo   StreamLoop 3 - 24/7 YouTube Streamer
echo ======================================
echo.

set "ROOT=%~dp0"
set "EXE=%ROOT%streamloop.exe"

:: An update stopped between its two renames leaves only streamloop.exe.old.
if not exist "%EXE%" if exist "%ROOT%streamloop.exe.old" rename "%ROOT%streamloop.exe.old" streamloop.exe

if not exist "%EXE%" goto :missing_exe
if not exist "%ROOT%ffmpeg\ffmpeg.exe" goto :missing_ffmpeg
rem The Rust token server, or the TypeScript one an exe-only update keeps until the full ZIP.
if exist "%ROOT%supervisor\streamloop-pot.exe" goto :token_server_ok
if exist "%ROOT%node\node.exe" if exist "%ROOT%pot\build\main.js" goto :token_server_ok
goto :missing_pot
:token_server_ok

echo Starting StreamLoop...
echo.

set "STREAMLOOP_START=first"

:loop
if not exist "%EXE%" if exist "%ROOT%streamloop.exe.old" rename "%ROOT%streamloop.exe.old" streamloop.exe
cd /d "%ROOT%"
"%EXE%"
set "CODE=%ERRORLEVEL%"

if "%CODE%"=="0" (
    echo.
    echo StreamLoop stopped.
    pause
    exit /b 0
)

if "%CODE%"=="2" (
    echo.
    echo Another StreamLoop is already running. Close it and start this one again.
    pause
    exit /b 2
)

set "STREAMLOOP_START=restart"
if "%CODE%"=="75" goto restart
if exist "%ROOT%_update\landed" if exist "%ROOT%streamloop.exe.old" call :rollback

:restart
echo.
echo StreamLoop exited (code %CODE%). Starting it again in 10 seconds...
ping -n 11 127.0.0.1 >nul
goto loop

:: An update landed and the new version stopped before confirming it: put the previous exe back.
:rollback
if not exist "%EXE%" goto :cut_off
if exist "%ROOT%streamloop.exe.bad" del /f /q "%ROOT%streamloop.exe.bad"
rename "%EXE%" streamloop.exe.bad
if exist "%EXE%" goto :rollback_failed
rename "%ROOT%streamloop.exe.old" streamloop.exe
if not exist "%EXE%" goto :rollback_undo
if not exist "%ROOT%data" mkdir "%ROOT%data"
move /y "%ROOT%_update\landed" "%ROOT%data\update-rolled-back" >nul
set "STREAMLOOP_START=rollback"
echo [RECOVER] The updated StreamLoop stopped before it was confirmed, so the previous version is back.
goto :eof

:: The previous exe would not go back, so the new one returns and the record stays for the next exit.
:rollback_undo
rename "%ROOT%streamloop.exe.bad" streamloop.exe
:: The updated exe could not be put aside: it stays, with the record, for the next exit to try again.
:rollback_failed
echo [WARN] The updated StreamLoop stopped before it was confirmed, and the previous version could not be put back yet. StreamLoop tries again the next time it stops.
goto :eof

:: The preflight labels hold the expanded paths here, outside any block, so a ")" in the install path cannot close one.
:missing_exe
echo [ERROR] StreamLoop is missing: %EXE%. Reinstall StreamLoop from the full ZIP.
pause
exit /b 1

:missing_ffmpeg
echo [ERROR] ffmpeg is missing: %ROOT%ffmpeg\ffmpeg.exe. Reinstall StreamLoop from the full ZIP.
pause
exit /b 1

:missing_pot
echo [ERROR] The token server is missing: %ROOT%supervisor\streamloop-pot.exe. Reinstall StreamLoop from the full ZIP.
pause
exit /b 1

:cut_off
rem The update was cut off between its two renames, so the new version never ran.
rename "%ROOT%streamloop.exe.old" streamloop.exe
if not exist "%EXE%" goto :eof
echo [RECOVER] An update was cut off before it finished, so the previous version is back.
goto :eof
