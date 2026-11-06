; Windows installer of tcanvas (NSIS 3, built on Linux with makensis).
;
;   makensis -DVERSION=1.2.3 -DEXE=path/to/tcanvas.exe -DOUTFILE=path/to/setup.exe installer/tcanvas.nsi
;
; Per-user install (no admin rights) in %LOCALAPPDATA%\Programs\tcanvas, Start menu shortcut,
; entry in "Installed apps" with an uninstaller. `/S` installs silently (used by winget).
; A running tcanvas is stopped first so an update can replace the exe; its tmux sessions live
; in WSL and survive, like with scripts/deploy.sh.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "pass -DVERSION=x.y.z"
!endif
!ifndef EXE
  !error "pass -DEXE=path/to/tcanvas.exe"
!endif
!ifndef OUTFILE
  !define OUTFILE "tcanvas-v${VERSION}-setup-x64.exe"
!endif

!define NAME "tcanvas"
!define PUBLISHER "Sebastien Asselin"
!define URL "https://github.com/sasselin/tcanvas"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${NAME}"

Name "${NAME}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${NAME}"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
RequestExecutionLevel user
BrandingText "${NAME} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=0 "ProductName" "${NAME}"
VIAddVersionKey /LANG=0 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "FileDescription" "${NAME} installer"
VIAddVersionKey /LANG=0 "CompanyName" "${PUBLISHER}"
VIAddVersionKey /LANG=0 "LegalCopyright" "MIT OR Apache-2.0"

!include "MUI2.nsh"

!define MUI_ICON "..\assets\icon.ico"
!define MUI_UNICON "..\assets\icon.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\tcanvas.exe"

!insertmacro MUI_PAGE_LICENSE "..\LICENSE-MIT"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; Follows the Windows display language, like the app.
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "French"

!macro StopApp
  nsExec::Exec 'taskkill /IM tcanvas.exe /F'
  Pop $0
  Sleep 300
!macroend

Section "tcanvas"
  !insertmacro StopApp
  SetOutPath "$INSTDIR"
  File "/oname=tcanvas.exe" "${EXE}"
  File "/oname=LICENSE-MIT.txt" "..\LICENSE-MIT"
  File "/oname=LICENSE-APACHE.txt" "..\LICENSE-APACHE"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${NAME}.lnk" "$INSTDIR\tcanvas.exe"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "${URL}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\tcanvas.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  SectionGetSize 0 $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0
SectionEnd

; Keeps the saved layout and everything in WSL (agent, tmux sessions).
Section "Uninstall"
  !insertmacro StopApp
  Delete "$INSTDIR\tcanvas.exe"
  Delete "$INSTDIR\LICENSE-MIT.txt"
  Delete "$INSTDIR\LICENSE-APACHE.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${NAME}.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
