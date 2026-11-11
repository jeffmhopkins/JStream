@echo off
rem Runs the release pass (release-pass.ps1 next to this file) in Windows PowerShell.
rem Pass options through, for example:  release-pass.bat -Zip C:\path\to\the.zip   or   -AutomatedOnly
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0release-pass.ps1" %*
echo.
pause
