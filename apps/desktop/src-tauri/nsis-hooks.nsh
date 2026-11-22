; Use Tauri's Restart Manager check for sidecars as well as the main executable.
; Close the UI and CLI first so they cannot restart mintcatd during setup.
!macro MintCatCloseInstalledProcesses
  !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  !insertmacro CheckIfAppIsRunning "$INSTDIR\mintcat.exe" "${PRODUCTNAME}"
  !insertmacro CheckIfAppIsRunning "$INSTDIR\mintcat-module.exe" "${PRODUCTNAME}"
  !insertmacro CheckIfAppIsRunning "$INSTDIR\mintcatd.exe" "${PRODUCTNAME}"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro MintCatCloseInstalledProcesses
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro MintCatCloseInstalledProcesses
!macroend
