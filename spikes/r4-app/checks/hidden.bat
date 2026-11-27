@echo off
rem Check 2: started hidden by "probe detach-bat" with NUL on stdin, stdout and stderr. Each step must
rem return with no window on screen: ping, the GUI-subsystem probe, and pause on empty input.
set "LOG=%~dp0bat.log"
>>"%LOG%" echo started %TIME%
ping -n 3 127.0.0.1
>>"%LOG%" echo ping done %TIME% errorlevel=%ERRORLEVEL%
"%~dp0probe.exe" exit 0
>>"%LOG%" echo probe done %TIME% errorlevel=%ERRORLEVEL%
pause
>>"%LOG%" echo pause returned %TIME%
