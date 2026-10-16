; Desk Bridge - Tauri 2 NSIS installer hooks (plan section 12.2).
;
; Referenced from app/src-tauri/tauri.conf.json -> bundle.windows.nsis.installerHooks.
; Tauri expands these macros inside its generated installer script:
;   NSIS_HOOK_PREINSTALL   at the top of `Section Install`   (before files are copied)
;   NSIS_HOOK_POSTINSTALL  at the end of `Section Install`   (after files, registry, shortcuts)
;   NSIS_HOOK_PREUNINSTALL at the top of `Section Uninstall` (before anything is removed)
;   NSIS_HOOK_POSTUNINSTALL at the end of `Section Uninstall`
;
; Variables provided by Tauri that we rely on: $INSTDIR, $UpdateMode (1 when the updater runs
; the installer with /UPDATE), $PassiveMode (1 for /P), ${MAINBINARYNAME}, ${Silent} (/S).
;
; Payload layout: deskbridge-agent.exe, deskbridge-service.exe, deskbridge-setup.exe, the FFmpeg
; DLLs and drivers\** are expected directly in $INSTDIR (see installer/README.md for the
; bundle.resources mapping). If the bundler put them below $INSTDIR\resources instead, the
; macros below fall back to that directory.
;
; User data (%APPDATA%\softwelop\DeskBridge, %LOCALAPPDATA%\softwelop\DeskBridge) is NEVER
; touched on install, upgrade or update. Only an interactive uninstall can delete it, and only
; after an explicit "Yes" (default is "No"; silent uninstall always keeps it).

!define DB_SETUP_EXE "deskbridge-setup.exe"
!define DB_AGENT_EXE "deskbridge-agent.exe"
!define DB_TASK_NAME "DeskBridge Agent"

Var DbPayloadDir
Var DbPurgeArg

; Sets $DbPayloadDir to the directory that contains deskbridge-setup.exe.
!macro DeskBridgeResolvePayload
  StrCpy $DbPayloadDir "$INSTDIR"
  ${IfNot} ${FileExists} "$INSTDIR\${DB_SETUP_EXE}"
    ${If} ${FileExists} "$INSTDIR\resources\${DB_SETUP_EXE}"
      StrCpy $DbPayloadDir "$INSTDIR\resources"
    ${EndIf}
  ${EndIf}
!macroend

; Ask the agent/service/UI to quit, then force whatever is left, so files can be replaced.
!macro DeskBridgeStopProcesses
  DetailPrint "Stopping Desk Bridge ..."
  ; 1. Stop the supervising service first (it would restart the agent). Ignore errors:
  ;    the setup helper may not exist yet (fresh install) or the service may not be registered.
  ${If} ${FileExists} "$DbPayloadDir\${DB_SETUP_EXE}"
    nsExec::Exec '"$DbPayloadDir\${DB_SETUP_EXE}" service stop --quiet --install-dir "$DbPayloadDir"'
    Pop $0
  ${EndIf}
  ; 2. Stop the scheduled task instance (the agent is started by it at logon).
  nsExec::Exec 'schtasks.exe /End /TN "${DB_TASK_NAME}"'
  Pop $0
  ; 3. Graceful stop of the agent (assumed CLI; harmless if the flag is unknown).
  ${If} ${FileExists} "$DbPayloadDir\${DB_AGENT_EXE}"
    nsExec::Exec '"$DbPayloadDir\${DB_AGENT_EXE}" --stop'
    Pop $0
  ${EndIf}
  ; 4. Polite taskkill (WM_CLOSE), short grace period, then force.
  nsExec::Exec 'taskkill.exe /IM "${DB_AGENT_EXE}"'
  Pop $0
  Sleep 800
  nsExec::Exec 'taskkill.exe /F /IM "${DB_AGENT_EXE}"'
  Pop $0
  ; The UI is handled by Tauri's own CheckIfAppIsRunning right after this hook.
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro DeskBridgeResolvePayload
  ; On first install nothing exists yet and every command above simply fails silently.
  !insertmacro DeskBridgeStopProcesses
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro DeskBridgeResolvePayload
  DetailPrint "Configuring Desk Bridge (firewall rule, autostart task, Send-to entry) ..."
  ; --install-dir is the directory that holds deskbridge-agent.exe (the task and the firewall
  ; rule are scoped to that exact path). Exit code: 0 ok, 2 some steps failed, other = error.
  nsExec::ExecToLog '"$DbPayloadDir\${DB_SETUP_EXE}" install --quiet --install-dir "$DbPayloadDir"'
  Pop $0
  ${If} $0 <> 0
    DetailPrint "deskbridge-setup install returned $0"
    ${IfNot} ${Silent}
      ${If} $LANGUAGE = 1031
        MessageBox MB_OK|MB_ICONEXCLAMATION "Die Einrichtung von Desk Bridge ist nicht vollständig abgeschlossen (Code $0).$\r$\n$\r$\nDetails: %LOCALAPPDATA%\softwelop\DeskBridge\logs\setup.log"
      ${Else}
        MessageBox MB_OK|MB_ICONEXCLAMATION "Desk Bridge setup did not finish completely (code $0).$\r$\n$\r$\nDetails: %LOCALAPPDATA%\softwelop\DeskBridge\logs\setup.log"
      ${EndIf}
    ${EndIf}
  ${EndIf}

  ; Launch the UI de-elevated. Interactive installs use the finish page's "Run" checkbox
  ; (Tauri), the updater passes /R (handled in .onInstSuccess). For plain silent/passive installs
  ; nobody would start the app, so do it here.
  ${If} ${Silent}
  ${OrIf} $PassiveMode = 1
    ClearErrors
    ${GetOptions} $CMDLINE "/R" $R0
    ${If} ${Errors}
      nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" ""
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; The updater runs the old uninstaller with /UPDATE (or skips it): keep everything in place,
  ; the new installer re-registers idempotently.
  ${If} $UpdateMode <> 1
    !insertmacro DeskBridgeResolvePayload
    !insertmacro DeskBridgeStopProcesses

    StrCpy $DbPurgeArg ""
    ${IfNot} ${Silent}
    ${AndIf} $PassiveMode <> 1
      ${If} $LANGUAGE = 1031
        MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Sollen auch die Benutzerdaten von Desk Bridge gelöscht werden (Kopplung mit anderen Geräten, Einstellungen, Protokolle)?$\r$\n$\r$\nBei „Nein“ bleibt alles erhalten und eine spätere Neuinstallation muss nicht neu koppeln." IDNO db_keep_user_data
      ${Else}
        MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Also delete the Desk Bridge user data (pairings with other devices, settings, logs)?$\r$\n$\r$\nChoose No to keep everything; a later reinstall will not need to pair again." IDNO db_keep_user_data
      ${EndIf}
      StrCpy $DbPurgeArg "--purge-user-data"
    ${EndIf}
    db_keep_user_data:

    DetailPrint "Removing firewall rule, autostart task, Send-to entry, drivers ..."
    ; Removes what `install` created (reads installed.json), restores the lid-close action,
    ; and deletes user data only when $DbPurgeArg is set.
    nsExec::ExecToLog '"$DbPayloadDir\${DB_SETUP_EXE}" uninstall --quiet --install-dir "$DbPayloadDir" $DbPurgeArg'
    Pop $0
    ${If} $0 <> 0
      DetailPrint "deskbridge-setup uninstall returned $0 (see setup.log)"
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    ; Files that were not part of the bundle manifest and therefore not removed by Tauri.
    Delete "$INSTDIR\installed.json"
    Delete "$INSTDIR\installed.json.tmp"
    RMDir "$INSTDIR\drivers"
    RMDir "$INSTDIR\resources"
    RMDir "$INSTDIR"
  ${EndIf}
!macroend
