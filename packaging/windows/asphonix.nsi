; ASPHONIX Windows installer (NSIS 3, Unicode).
;
; Built by .github/workflows/release.yml:
;   makensis /DVERSION=0.2.0 /DSRCDIR=<dir with asphonix.exe, asphonix-mcp.exe, LICENSE> /DOUTFILE=<setup.exe> asphonix.nsi
;
; Per-user install (no admin rights, no UAC prompt): %LOCALAPPDATA%\Programs\ASPHONIX.
; Switches used by the in-app updater (crates/asphonix-core/src/updates.rs, docs/UPDATES.md):
;   /P       passive: no questions, only the progress page, closes itself
;   /UPDATE  update an existing installation (same folder, no new shortcuts beyond the existing ones)
;   /R       relaunch ASPHONIX when done
; Plus the standard NSIS switches (/S silent, /D=<dir> install folder).
;
; User data (settings, library database, logs in %APPDATA% / %LOCALAPPDATA%\dev.asphonix.app) is never touched,
; neither by install nor by uninstall.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "pass /DVERSION=x.y.z"
!endif
!ifndef SRCDIR
  !error "pass /DSRCDIR=<folder with the release binaries>"
!endif
; Numeric x.y.z.w for the version resource (a pre-release tag like 0.2.0-rc.1 passes /DVIVERSION=0.2.0.0).
!ifndef VIVERSION
  !define VIVERSION "${VERSION}.0"
!endif
!ifndef OUTFILE
  !define OUTFILE "ASPHONIX_${VERSION}_x64-setup.exe"
!endif

!define APP "ASPHONIX"
!define EXE "asphonix.exe"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"
!define APP_KEY "Software\${APP}"

Name "${APP}"
OutFile "${OUTFILE}"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VIVERSION}"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "LegalCopyright" "MIT License"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define MUI_ICON "..\..\crates\asphonix-ui\res\asphonix.ico"
!define MUI_UNICON "..\..\crates\asphonix-ui\res\asphonix.ico"
!define MUI_ABORTWARNING

Var Passive
Var Update
Var Relaunch

; Pages: every interactive page is skipped in passive mode, only the progress page remains.
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_WELCOME
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_LICENSE "${SRCDIR}\LICENSE"
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "German"

Function .onInit
  StrCpy $Passive 0
  StrCpy $Update 0
  StrCpy $Relaunch 0
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/P" $R1
  ${IfNot} ${Errors}
    StrCpy $Passive 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    StrCpy $Update 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/R" $R1
  ${IfNot} ${Errors}
    StrCpy $Relaunch 1
  ${EndIf}
  ${If} $Passive == 1
    SetAutoClose true
  ${EndIf}
FunctionEnd

Function SkipIfPassive
  ${If} $Passive == 1
    Abort
  ${EndIf}
FunctionEnd

; The updater exits the app right after starting the setup: wait (up to ~15 s) until the old exe can be replaced.
Function WaitForAppExit
  IfFileExists "$INSTDIR\${EXE}" 0 done
  StrCpy $R2 0
  loop:
    ClearErrors
    FileOpen $R3 "$INSTDIR\${EXE}" a
    ${IfNot} ${Errors}
      FileClose $R3
      Goto done
    ${EndIf}
    IntOp $R2 $R2 + 1
    ${If} $R2 >= 30
      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${APP} is still running. Close it and retry." /SD IDCANCEL IDRETRY loop
      Abort
    ${EndIf}
    Sleep 500
    Goto loop
  done:
FunctionEnd

Section "${APP}" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"
  Call WaitForAppExit

  File "${SRCDIR}\${EXE}"
  File "${SRCDIR}\asphonix-mcp.exe"
  File "${SRCDIR}\LICENSE"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Start menu entry on first install (an update keeps whatever the user did with it).
  ${If} $Update == 0
    CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}"
  ${EndIf}

  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

Function .onInstSuccess
  ${If} $Relaunch == 1
    Exec '"$INSTDIR\${EXE}"'
  ${EndIf}
FunctionEnd

Section "Uninstall"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\asphonix-mcp.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "${APP_KEY}"
SectionEnd
