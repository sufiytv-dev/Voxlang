@echo off
setlocal
echo Adding Vox to your user PATH...
set "TARGET_DIR=%~dp0target\release"
setx PATH "%PATH%;%TARGET_DIR%"
echo.
echo Success! Vox has been added to your PATH.
echo PLEASE RESTART YOUR TERMINAL for changes to take effect.
pause
