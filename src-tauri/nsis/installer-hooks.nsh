!macro NSIS_HOOK_PREINSTALL
  !insertmacro CheckIfAppIsRunning "mullion-bridge-worker.exe" "Mullion Bridge Worker"
!macroend
