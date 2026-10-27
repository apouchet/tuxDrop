@echo off
rem Double-click to install tuxDrop; "install.cmd -Uninstall" removes it.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
if errorlevel 1 pause
