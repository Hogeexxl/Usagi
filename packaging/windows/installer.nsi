Unicode true

!include "MUI2.nsh"

!ifndef VERSION
    !error "VERSION define is required"
!endif
!ifndef APP_EXE
    !error "APP_EXE define is required"
!endif
!ifndef OUTPUT
    !error "OUTPUT define is required"
!endif

Name "Usagi"
Caption "Usagi ${VERSION}"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\Usagi"
RequestExecutionLevel user
SetOverwrite on

!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "Install Usagi ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "Usagi runs in the background and opens its Dashboard in your default browser."
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "English"

Section "Usagi" SecUsagi
    SetShellVarContext current
    SetOutPath "$INSTDIR"
    File "${APP_EXE}"
    WriteUninstaller "$INSTDIR\Uninstall.exe"

    CreateDirectory "$SMPROGRAMS"
    CreateShortcut "$SMPROGRAMS\Usagi.lnk" "$INSTDIR\usagi.exe" "" "$INSTDIR\usagi.exe" 0

    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi" "DisplayName" "Usagi"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi" "DisplayVersion" "${VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi" "DisplayIcon" "$INSTDIR\usagi.exe"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi" "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi" "NoRepair" 1
SectionEnd

Section "Uninstall"
    SetShellVarContext current
    Delete "$SMPROGRAMS\Usagi.lnk"
    Delete "$INSTDIR\usagi.exe"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi"
SectionEnd
