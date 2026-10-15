@echo off
rem Builds and runs spike S4 (CEF browser source) through run-s4.ps1 next to this file,
rem unattended, and collects the results into a zip. Options pass through, for example:
rem   run-s4.bat -Seconds 60     or     run-s4.bat -Skip4K
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-s4.ps1" %*
echo.
pause
