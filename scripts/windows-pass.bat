@echo off
rem Runs the Windows hardware pass (windows-pass.ps1 next to this file) in Windows PowerShell.
rem Pass options through, for example:  windows-pass.bat -List   or   -Only render,nv12
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-pass.ps1" %*
echo.
pause
