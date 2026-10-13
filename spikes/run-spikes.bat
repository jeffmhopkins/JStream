@echo off
rem Runs the M0 Windows spikes (S1 GPU, S2 virtual camera, S3 audio clock) through
rem run-spikes.ps1 next to this file, and collects the results into a zip.
rem Pass options through, for example:  run-spikes.bat -Only S1,S2   or   -S3Minutes 10
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-spikes.ps1" %*
echo.
pause
