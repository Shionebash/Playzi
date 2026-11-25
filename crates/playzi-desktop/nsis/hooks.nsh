; Playzi NSIS installer hooks
; Installs required tools (yt-dlp, ffmpeg) and optionally prompts for MPV / VLC.

!macro NSIS_HOOK_POSTINSTALL
  ${If} $UpdateMode <> 1
    ${If} ${Silent}
    ${OrIf} $PassiveMode = 1
      DetailPrint "Verificando e instalando requisitos de Playzi (yt-dlp, ffmpeg)..."
      nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "$INSTDIR\scripts\install-deps.ps1" -InstallDir "$INSTDIR" -Silent'
      Pop $0
    ${Else}
      DetailPrint "Configurando requisitos de Playzi (yt-dlp, ffmpeg, MPV/VLC)..."
      nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "$INSTDIR\scripts\install-deps.ps1" -InstallDir "$INSTDIR"'
      Pop $0
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    RMDir /r "$INSTDIR\bin"
    RMDir /r "$INSTDIR\scripts"
    RMDir "$INSTDIR"
  ${EndIf}
!macroend
