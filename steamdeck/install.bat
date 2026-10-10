@echo off
rem Install demarc on a Steam Deck over ssh (Windows 10 or later).
rem
rem   install.bat deck@address-of-the-Deck
rem
rem Run it again with a newer demarc.tar.gz to update.

if "%~1"=="" echo Usage: install.bat deck@address-of-the-Deck& exit /b 1
scp "%~dp0demarc.tar.gz" "%~1:demarc.tar.gz" || exit /b 1
ssh "%~1" "mkdir -p demarc && tar -xzf demarc.tar.gz -C demarc && rm demarc.tar.gz && bash demarc/setup.sh"
