; Скрипт для Inno Setup
; Внимание: вне секции [Code] Inno Setup понимает как комментарий только ';'.
; Строки с '//' раньше вызывали ошибку разбора скрипта.
;
; Сборка: cargo build --release, затем
;   "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" setup.iss
; Перед сборкой нужна папка resources\vosk (scripts\get-vosk.ps1).

#define AppVersion "0.3.0"

[Setup]
; Постоянный идентификатор: по нему новая версия ставится поверх старой.
AppId={{6F3C2A51-8E4B-4D7A-9C1E-2B5F7A9D3E10}
AppName=Timer Pro
AppVersion={#AppVersion}
AppVerName=Timer Pro {#AppVersion}
AppPublisher=animedeed
AppPublisherURL=https://github.com/animedeed/timer-pro
AppSupportURL=https://github.com/animedeed/timer-pro/issues
DefaultDirName={autopf}\TimerPro
DefaultGroupName=Timer Pro
DisableProgramGroupPage=yes
; Установка для текущего пользователя — без запроса прав администратора.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; Папка куда Inno Setup положит готовый инсталлятор
OutputDir=.\installer
OutputBaseFilename=TimerPro-{#AppVersion}-Setup
SetupIconFile=resources\timer.ico
UninstallDisplayIcon={app}\timer_pro.exe
LicenseFile=LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Обновление поверх запущенной программы: установщик предложит её закрыть.
CloseApplications=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
; Звук будильника и шрифты вшиты в exe, отдельно копировать нечего
Source: "target\release\timer_pro.exe"; DestDir: "{app}"; Flags: ignoreversion
; Голосовое управление: libvosk.dll с зависимостями и офлайн-модель русской речи
Source: "resources\vosk\*"; DestDir: "{app}\vosk"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "THIRD_PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; AppUserModelID нужен для уведомлений Windows: без ярлыка с этим идентификатором
; система молча не показывает уведомления обычной программы.
Name: "{group}\Timer Pro"; Filename: "{app}\timer_pro.exe"; AppUserModelID: "TimerPro.Widget"
Name: "{autodesktop}\Timer Pro"; Filename: "{app}\timer_pro.exe"; AppUserModelID: "TimerPro.Widget"; Tasks: desktopicon

[Run]
Filename: "{app}\timer_pro.exe"; Description: "{cm:LaunchProgram,Timer Pro}"; Flags: nowait postinstall skipifsilent
