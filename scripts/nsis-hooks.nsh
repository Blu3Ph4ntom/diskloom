!macro NSIS_HOOK_POSTINSTALL
  IfFileExists "$INSTDIR\resources\dlm.exe" 0 +2
    CopyFiles /SILENT "$INSTDIR\resources\dlm.exe" "$INSTDIR\dlm.exe"

  IfFileExists "$INSTDIR\resources\diskloom-setup.exe" 0 +2
    ExecWait '"$INSTDIR\resources\diskloom-setup.exe" --source "$INSTDIR" --install-dir "$INSTDIR" --no-shortcut'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  IfFileExists "$INSTDIR\resources\diskloom-setup.exe" 0 +2
    ExecWait '"$INSTDIR\resources\diskloom-setup.exe" --remove-path --install-dir "$INSTDIR" --no-shortcut'
!macroend
