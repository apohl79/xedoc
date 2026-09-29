@echo off
setlocal

set "SCRIPT_DIR=%~dp0"
for %%I in ("%SCRIPT_DIR%..") do set "PACKAGE_ROOT=%%~fI"
set "PYTHON=%PACKAGE_ROOT%\xedoc-resources\remote-agent\runtime\python\python.exe"
set "PAYLOAD=%PACKAGE_ROOT%\xedoc-resources\remote-agent\remote-agent.pyz"

if not exist "%PYTHON%" (
  >&2 echo xedoc-remote-agentd: bundled Python runtime is unavailable
  exit /b 1
)
if not exist "%PAYLOAD%" (
  >&2 echo xedoc-remote-agentd: bundled remote-agent payload is unavailable
  exit /b 1
)

if "%~1"=="" (
  "%PYTHON%" -I "%PAYLOAD%" daemon
) else (
  "%PYTHON%" -I "%PAYLOAD%" daemon %*
)
exit /b %ERRORLEVEL%
