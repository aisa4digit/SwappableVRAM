@echo off
set BOOT=
for %%D in (C D E F G H I J K L M N O P Q R S T U V W Y Z) do if exist %%D:\SVRAM.ID set BOOT=%%D:
if not defined BOOT goto ERROR
if "%SVRAM_LOGGING%"=="1" goto START
set SVRAM_LOGGING=1
call "%~f0" >>"%BOOT%\GUEST.LOG" 2>&1
wpeutil shutdown
goto :eof
:START
echo SVRAM_GUEST_STARTED
echo Boot media: %BOOT%
"%BOOT%\svram.exe" lab-environment --require-guest
if errorlevel 1 goto ERROR
if exist "%BOOT%\STAGE1.OK" goto INVENTORY
echo SVRAM_STAGE1_GUEST_BCD_ONLY
bcdedit /store "%BOOT%\Boot\BCD" /set {default} testsigning on
if errorlevel 1 goto ERROR
bcdedit /store "%BOOT%\EFI\Microsoft\Boot\BCD" /set {default} testsigning on
if errorlevel 1 goto ERROR
bcdedit /store "%BOOT%\EFI\Microsoft\Boot\BCD" /enum all
if errorlevel 1 goto ERROR
echo Disposable guest boot media only>"%BOOT%\STAGE1.OK"
echo SVRAM_STAGE1_SHUTDOWN
exit /b 0
:INVENTORY
echo SVRAM_STAGE2_INVENTORY
"%BOOT%\svram.exe" lab-code-integrity
if errorlevel 1 goto ERROR
pnputil /enum-devices /class DiskDrive /deviceids /stack
if not exist "%BOOT%\DRIVER\svramflt.inf" goto LIST
"%BOOT%\svram.exe" lab-trust "%BOOT%\DRIVER\svram-lab.cer"
if errorlevel 1 goto ERROR
pnputil /add-driver "%BOOT%\DRIVER\svramflt.inf" /install
if errorlevel 1 goto ERROR
pnputil /restart-device /deviceid "SCSI\DiskSVRAMLABPAGINGDISK______"
if errorlevel 1 goto ERROR
pnputil /scan-devices
if errorlevel 1 goto ERROR
if not exist "%BOOT%\VERIFIER.ID" goto NO_VERIFIER
"%BOOT%\svram.exe" lab-enable-io-verifier
if errorlevel 1 goto ERROR
:NO_VERIFIER
pnputil /enum-devices /class DiskDrive /deviceids /stack
pnputil /enum-devices /bus SVRAMCTRL /deviceids /relations /drivers
pnputil /enum-devices /connected /deviceids /relations
pnputil /enum-devices /problem /deviceids /relations
reg query HKLM\SYSTEM\CurrentControlSet\Services\svramflt
for /r "%SystemRoot%\System32\DriverStore\FileRepository" %%F in (svramflt.sys) do if exist "%%F" copy /y "%%F" "%BOOT%\LOADED.SYS"
"%BOOT%\svram.exe" driver-list
copy /y X:\Windows\INF\setupapi.dev.log "%BOOT%\SETUPAPI.LOG"
if exist "%BOOT%\EXP.ID" goto EXPERIMENTAL
"%BOOT%\svram.exe" lab-check-bypass
if errorlevel 1 goto ERROR
"%BOOT%\svram.exe" lab-prepare-storage
if errorlevel 1 goto ERROR
goto LIST
:EXPERIMENTAL
set /p SCENARIO=<"%BOOT%\EXP.ID"
"%BOOT%\svram.exe" lab-cache-scenario "%BOOT%\." "%SCENARIO%"
if errorlevel 1 goto ERROR
:LIST
if exist "%BOOT%\VERIFIER.ID" "%SystemRoot%\System32\verifier.exe" /query
"%BOOT%\svram.exe" driver-list
echo SVRAM_INVENTORY_COMPLETE
exit /b 0
:ERROR
echo SVRAM_GUEST_ERROR
echo Boot discovery: %BOOT%
if "%SVRAM_LOGGING%"=="1" exit /b 1
pause
wpeutil shutdown
