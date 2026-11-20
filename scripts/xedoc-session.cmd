@echo off
setlocal

for %%I in ("%~dp0..") do set "PACKAGE_ROOT=%%~fI"
set "PYTHON=%PACKAGE_ROOT%\xedoc-resources\remote-agent\runtime\python\python.exe"

if not exist "%PYTHON%" (
  >&2 echo xedoc-session: bundled Python runtime is unavailable
  exit /b 1
)

"%PYTHON%" -B -E -s -X utf8 "%~dp0xedoc-session" %*
exit /b %ERRORLEVEL%
