; Babbage NSIS installer hooks.
;
; The stock Tauri uninstaller removes the install directory and shortcuts but
; knows nothing about the data Babbage writes at runtime. These hooks make
; uninstalling a full cleanup:
;
;   - the app keeps running in the background when its window is hidden, so it
;     has to be stopped before its files are deleted;
;   - the runtime data lives in %APPDATA%\babbage (config, index, metadata DB,
;     thumbnails, OCR packs, model packs, agent HTTP token);
;   - WebView2 keeps its own profile per Tauri identifier under
;     %LOCALAPPDATA%\com.babbage.desktop;
;   - the auto-launch crate registers HKCU\...\Run with the display name
;     "Babbage".

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Setting up bundled models and OCR packages..."
  CreateDirectory "$APPDATA\babbage\models"
  CreateDirectory "$APPDATA\babbage\ocr"
  CreateDirectory "$APPDATA\babbage\ocr-indra"
  CopyFiles /SILENT "$INSTDIR\resources\models\*.*" "$APPDATA\babbage\models\"
  CopyFiles /SILENT "$INSTDIR\resources\ocr\*.*" "$APPDATA\babbage\ocr\"
  CopyFiles /SILENT "$INSTDIR\resources\ocr-indra\*.*" "$APPDATA\babbage\ocr-indra\"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Stopping Babbage..."
  nsExec::Exec 'taskkill /IM "babbage.exe" /F'
  Pop $R9
  nsExec::Exec 'taskkill /IM "babbage-daemon.exe" /F'
  Pop $R9
  nsExec::Exec 'taskkill /IM "babbage-quick.exe" /F'
  Pop $R9
  Sleep 500
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Removing Babbage data..."
  ; Runtime data: config, index, metadata.db, thumbs, tessdata, ocr packs,
  ; model packs, agent HTTP token, logs, crawl report.
  RMDir /r "$APPDATA\babbage"
  RMDir /r "$APPDATA\com.babbage.desktop"
  RMDir /r "$LOCALAPPDATA\com.babbage.desktop"
  RMDir /r "$LOCALAPPDATA\babbage"
  RMDir /r "$TEMP\babbage-updates"
  ; Autostart entry written by the auto-launch crate
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Babbage"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "babbage"
  DetailPrint "Babbage data removed."
!macroend
