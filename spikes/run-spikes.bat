@echo off
rem Runs the M0 Windows spikes (S1 GPU, S3 audio clock; S2 virtual camera on request)
rem through run-spikes.ps1 next to this file, unattended, and collects the results into a zip.
rem Pass options through, for example:  run-spikes.bat -Only S1   or   -Only S3 -S3Minutes 10
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-spikes.ps1" %*
echo.
pause
