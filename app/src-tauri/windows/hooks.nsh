; TunnelHop's part of the installer (SPEC §6): the privileged service lives next to the app as
; th-service.exe, and is registered and removed here. The installer runs elevated (per-machine
; install), which is what registering a service needs; the app itself never is.

; What the hooks say, in each of the installer's languages.
!include /CHARSET=UTF8 "${__FILEDIR__}\messages.nsh"

; The app is closed before the service is touched. Tauri's own check for a running app comes
; after these hooks and can be cancelled, or fail (it cannot close an app in another session):
; either would leave the service stopped or removed with the old app still open. The app holds
; no state of its own, so closing it loses nothing.
!macro TH_CLOSE_APP
  nsExec::ExecToLog '"$SYSDIR\taskkill.exe" /F /IM "${MAINBINARYNAME}.exe"'
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro TH_CLOSE_APP
  ; An upgrade: the running service holds th-service.exe open, so it stops before the files
  ; are replaced. Stopping disconnects and lifts the block, as every stop does. `net stop`
  ; returns once the service has stopped; on a first install there is nothing to stop.
  !insertmacro TH_PRINT STOPPING_MAYBE
  nsExec::ExecToLog '"$SYSDIR\net.exe" stop TunnelHop'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Registers the service (or points an existing registration at the new file) and starts it.
  !insertmacro TH_PRINT INSTALLING
  nsExec::ExecToLog '"$INSTDIR\th-service.exe" install'
  Pop $0
  ${If} $0 != 0
    !insertmacro TH_WARN INSTALL_FAILED install
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro TH_CLOSE_APP
  ; $UpdateMode is 1 when a newer installer runs this uninstaller to replace the app: then
  ; the service only stops, and its bundles, settings and saved sign-in stay.
  ${If} $UpdateMode = 1
    !insertmacro TH_PRINT STOPPING
    nsExec::ExecToLog '"$SYSDIR\net.exe" stop TunnelHop'
    Pop $0
  ${Else}
    ; Disconnects, lifts the block, removes the service and %ProgramData%\TunnelHop.
    !insertmacro TH_PRINT REMOVING
    nsExec::ExecToLog '"$INSTDIR\th-service.exe" uninstall'
    Pop $0
    ${If} $0 != 0
      !insertmacro TH_WARN REMOVE_FAILED disengage
    ${EndIf}
  ${EndIf}
!macroend
