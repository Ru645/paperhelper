; PaperHelper Windows 安装包（NSIS 3 / Unicode）
;
; 注意：本文件必须保存为 UTF-8 with BOM——makensis 按「有 BOM → UTF-8，无 BOM → 系统 ANSI 代码页」
; 读脚本；不带 BOM 在英文 Windows（ACP=1252）上会把中文文件名/文案解成乱码，导致 File 找不到文件。
;
; 构建（由 scripts/package-windows.ps1 调用）：
;   makensis /DVERSION=0.1.0 /DVI4=0.1.0.0 /DDIST=<abs runtime 目录> /DOUT=<abs setup.exe> /DICON=<abs icon.ico> installer.nsi
;
; 特点：per-user 安装（无需管理员，装到 %LOCALAPPDATA%\PaperHelper）；
; 桌面 + 开始菜单快捷方式；卸载只删程序，不动 %USERPROFILE%\PaperHelper\.paperhelper 里的笔记；
; 检测 Microsoft Edge WebView2 运行时，缺了则打开官方下载页（PaperHelper 桌面窗口依赖它）。

Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!ifndef VI4
  !define VI4 "0.1.0.0"
!endif
!ifndef DIST
  !define DIST "dist-win\runtime"
!endif
!ifndef OUT
  !define OUT "dist-win\paperhelper-setup.exe"
!endif
!ifndef ICON
  !define ICON "..\..\assets\icon.ico"
!endif

!define APPNAME "PaperHelper"
!define APPEXE "paperhelper-desktop.exe"
!define PUBLISHER "PaperHelper"
!define WEBVIEW2_URL "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
!define WEBVIEW2_KEY "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

Name "${APPNAME} ${VERSION}"
!pragma warning disable 9000
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\PaperHelper"
InstallDirRegKey HKCU "Software\PaperHelper" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${VI4}"
VIAddVersionKey "ProductName" "${APPNAME}"
VIAddVersionKey "FileDescription" "${APPNAME} 安装程序"
VIAddVersionKey "FileVersion" "${VI4}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" ""

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APPEXE}"
!define MUI_FINISHPAGE_RUN_TEXT "立即启动 ${APPNAME}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

; WebView2 运行时检测（HKLM 64 位 / HKLM 32 位 / HKCU 三种注册表位置）
Function CheckWebView2Runtime
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_KEY}" "pv"
  StrCmp $0 "" 0 done
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_KEY}" "pv"
  StrCmp $0 "" 0 done
  ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_KEY}" "pv"
  StrCmp $0 "" 0 done
  MessageBox MB_OK|MB_ICONINFORMATION "未检测到 Microsoft Edge WebView2 运行时。$\r$\n$\r$\nPaperHelper 桌面窗口依赖它显示界面；接下来会打开官方下载页，装好后重新打开 PaperHelper 即可（安装很快，无需重启）。"
  ExecShell "open" "${WEBVIEW2_URL}"
  done:
FunctionEnd

Function .onInstSuccess
  Call CheckWebView2Runtime
FunctionEnd

Section "PaperHelper" SEC_MAIN
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "${DIST}\paperhelper.exe"
  File "${DIST}\paperhelper-desktop.exe"
  File "${DIST}\快速开始.html"
  SetOutPath "$INSTDIR\python"
  File /r "${DIST}\python\*.*"
  SetOutPath "$INSTDIR"
  File "${ICON}"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortCut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${APPEXE}" "" "$INSTDIR\icon.ico"
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortCut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\${APPEXE}" "" "$INSTDIR\icon.ico"
  CreateShortCut "$SMPROGRAMS\${APPNAME}\快速开始.lnk" "$INSTDIR\快速开始.html" "" "$INSTDIR\icon.ico"
  CreateShortCut "$SMPROGRAMS\${APPNAME}\卸载 ${APPNAME}.lnk" "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "Software\PaperHelper" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "DisplayName" "${APPNAME}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "DisplayIcon" "$INSTDIR\icon.ico"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$DESKTOP\${APPNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\快速开始.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\卸载 ${APPNAME}.lnk"
  RMDir "$SMPROGRAMS\${APPNAME}"

  RMDir /r "$INSTDIR\python"
  Delete "$INSTDIR\paperhelper.exe"
  Delete "$INSTDIR\paperhelper-desktop.exe"
  Delete "$INSTDIR\快速开始.html"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"
  DeleteRegKey HKCU "Software\PaperHelper"
SectionEnd
