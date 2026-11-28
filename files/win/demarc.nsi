; Windows installer for demarc. Built by `just win-installer` and the release
; workflow, which pass
; VERSION, NUMVER, EXE, DBDIR, ART and OUT with -D.

Unicode true
!include "MUI2.nsh"

Name "demarc"
OutFile "${OUT}"
InstallDir "$PROGRAMFILES64\demarc"
InstallDirRegKey HKLM "Software\demarc" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

VIProductVersion "${NUMVER}.0"
VIAddVersionKey "ProductName" "demarc"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "demarc installer"
VIAddVersionKey "LegalCopyright" "Jonas Minnberg"

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\demarc"

!define MUI_ICON "${ART}\demarc.ico"
!define MUI_UNICON "${ART}\demarc.ico"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "${ART}\header.bmp"
!define MUI_HEADERIMAGE_UNBITMAP "${ART}\header.bmp"
!define MUI_WELCOMEFINISHPAGE_BITMAP "${ART}\welcome.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "${ART}\welcome.bmp"
!define MUI_FINISHPAGE_RUN "$INSTDIR\demarc.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start demarc"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Function .onInit
    SetRegView 64
    SetShellVarContext all
FunctionEnd

Function un.onInit
    SetRegView 64
    SetShellVarContext all
FunctionEnd

Section "Install"
    SetOutPath "$INSTDIR"
    File "${EXE}"
    File "${DBDIR}\csdb.txt.gz"
    File "${DBDIR}\demozoo.txt.gz"
    File "${ART}\demarc.ico"
    WriteUninstaller "$INSTDIR\uninstall.exe"

    CreateShortcut "$SMPROGRAMS\demarc.lnk" "$INSTDIR\demarc.exe" "" "$INSTDIR\demarc.ico"

    WriteRegStr HKLM "Software\demarc" "InstallDir" "$INSTDIR"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "demarc"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
    WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "Jonas Minnberg"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\demarc.ico"
    WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
    WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
    Delete "$SMPROGRAMS\demarc.lnk"
    Delete "$INSTDIR\demarc.exe"
    Delete "$INSTDIR\csdb.txt.gz"
    Delete "$INSTDIR\demozoo.txt.gz"
    Delete "$INSTDIR\demarc.ico"
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"
    DeleteRegKey HKLM "${UNINST_KEY}"
    DeleteRegKey HKLM "Software\demarc"
SectionEnd
