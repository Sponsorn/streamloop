@echo off
rem Check 1: the GUI-subsystem probe started from a batch file in a console window (conhost or
rem Windows Terminal). probe.exe sits beside this file; RESULTS, when set, collects what the batch saw.
rem   console.bat codes   exit codes 0, 2, 75 and 1, a pipe into findstr, then a wait for Ctrl+C
rem   console.bat close   a wait for the window to close
rem   console.bat spawn   a parent that starts launch-like children on NUL, then a wait for the close
setlocal
set "PROBE=%~dp0probe.exe"
if not defined RESULTS set "RESULTS=nul"
goto %1

:codes
"%PROBE%" exit 0 500
set "EL=%ERRORLEVEL%"
echo batch read errorlevel %EL%
>>"%RESULTS%" echo exit0=%EL%
"%PROBE%" exit 2
set "EL=%ERRORLEVEL%"
echo batch read errorlevel %EL%
>>"%RESULTS%" echo exit2=%EL%
"%PROBE%" exit 75
set "EL=%ERRORLEVEL%"
echo batch read errorlevel %EL%
>>"%RESULTS%" echo exit75=%EL%
"%PROBE%" exit 1
set "EL=%ERRORLEVEL%"
echo batch read errorlevel %EL%
>>"%RESULTS%" echo exit1=%EL%
rem The version line goes through a pipe, so the probe keeps that handle and does not attach.
"%PROBE%" version | findstr .
>>"%RESULTS%" echo findstr=%ERRORLEVEL%
"%PROBE%" version | findstr . >>"%RESULTS%"
"%PROBE%" wait 120
>>"%RESULTS%" echo wait=%ERRORLEVEL%
goto :eof

:close
"%PROBE%" wait 120
>>"%RESULTS%" echo wait=%ERRORLEVEL%
goto :eof

:spawn
"%PROBE%" spawn-null launchlike
>>"%RESULTS%" echo spawn=%ERRORLEVEL%
goto :eof
