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

if not exist "%EXE%" (
    echo [ERROR] StreamLoop is missing: %EXE%. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

if not exist "%ROOT%ffmpeg\ffmpeg.exe" (
    echo [ERROR] ffmpeg is missing: %ROOT%ffmpeg\ffmpeg.exe. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

if not exist "%ROOT%node\node.exe" (
    echo [ERROR] Node.js is missing: %ROOT%node\node.exe. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

if not exist "%ROOT%pot\build\main.js" (
    echo [ERROR] The token server is missing: %ROOT%pot\build\main.js. Reinstall StreamLoop from the full ZIP.
    pause
    exit /b 1
)

echo Starting StreamLoop...
echo.

set "STREAMLOOP_START=first"

:loop
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
if not exist "%EXE%" (
    rem The update was cut off between its two renames, so the new version never ran.
    rename "%ROOT%streamloop.exe.old" streamloop.exe
    echo [RECOVER] An update was cut off before it finished, so the previous version is back.
    goto :eof
)
if exist "%ROOT%streamloop.exe.bad" del /f /q "%ROOT%streamloop.exe.bad"
rename "%EXE%" streamloop.exe.bad
rename "%ROOT%streamloop.exe.old" streamloop.exe
if not exist "%ROOT%data" mkdir "%ROOT%data"
move /y "%ROOT%_update\landed" "%ROOT%data\update-rolled-back" >nul
set "STREAMLOOP_START=rollback"
echo [RECOVER] The updated StreamLoop stopped before it was confirmed, so the previous version is back.
goto :eof
