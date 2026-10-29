//! Громкость отдельной программы через микшер громкости Windows (Core Audio):
//! браузера, Spotify, Яндекс Музыки… Системная громкость не меняется.
//!
//! Программа ищется по источнику SMTC — тому же, что показывает плашка
//! «Сейчас играет». Если по нему не нашлась — берётся та, из которой сейчас
//! реально идёт звук (самый громкий активный поток).
//!
//! Громкость внутри сайта (ползунок на music.yandex.ru) так не изменить — это
//! умеет только расширение браузера. Ползунок браузера в микшере звучит так же.

/// Что сделать с громкостью. Уровни — доли 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolumeChange {
    /// Прибавить (+) или убавить (−).
    By(f32),
    /// Поставить уровень.
    Set(f32),
    Mute(bool),
}

#[derive(Debug)]
pub struct VolumeResult {
    /// Имя программы (по exe), например «Chrome».
    pub app: String,
    pub level: f32,
    pub muted: bool,
}

/// Новый уровень и «без звука» из текущих. Прибавка снимает «без звука».
pub fn next_level(current: f32, muted: bool, change: VolumeChange) -> (f32, bool) {
    let round = |v: f32| (v.clamp(0.0, 1.0) * 100.0).round() / 100.0;
    match change {
        VolumeChange::By(d) => (round(current + d), if d > 0.0 { false } else { muted }),
        VolumeChange::Set(v) => (round(v), false),
        VolumeChange::Mute(m) => (current, m),
    }
}

/// Совпадает ли источник SMTC (AUMID) с exe-файлом процесса.
pub fn source_matches(aumid: &str, exe: &str) -> bool {
    let id = aumid.to_lowercase();
    let exe = exe.to_lowercase();
    let exe = exe.strip_suffix(".exe").unwrap_or(&exe);
    if id.is_empty() || exe.is_empty() {
        return false;
    }
    // Идентификаторы, не похожие на имя exe.
    const KNOWN: &[(&str, &[&str])] = &[
        ("308046b0af4a39cb", &["firefox"]),
        ("yandex", &["browser", "yandex", "яндекс"]),
        ("zunemusic", &["music.ui", "zunemusic"]),
        ("opera", &["opera", "launcher"]),
        ("thebrowsercompany", &["arc"]),
    ];
    if KNOWN.iter().any(|(key, exes)| id.contains(key) && exes.iter().any(|e| exe.contains(e))) {
        return true;
    }
    // Общее правило: имя exe входит в идентификатор (Chrome ↔ chrome.exe, Spotify.exe ↔ spotify.exe).
    exe.chars().count() >= 3 && id.contains(exe)
}

/// «chrome» → «Chrome».
pub fn pretty_app(exe: &str) -> String {
    let stem = exe.strip_suffix(".exe").or_else(|| exe.strip_suffix(".EXE")).unwrap_or(exe);
    match stem.to_lowercase().as_str() {
        "msedge" => "Edge".into(),
        "browser" => "Яндекс Браузер".into(),
        _ => crate::notes::capitalize(stem),
    }
}

/// Меняет громкость программы, где играет музыка. `source` — AUMID источника SMTC.
#[cfg(windows)]
pub fn apply(source: Option<String>, change: VolumeChange) -> Result<VolumeResult, String> {
    // Отдельный поток со своим COM-апартаментом: UI-поток не трогаем.
    std::thread::spawn(move || unsafe { imp::run(source.as_deref(), change) })
        .join()
        .map_err(|_| "сбой микшера громкости".to_string())?
}

/// Программы со звуком: (exe, громкость, без звука, пик, играет) — для --volume-list.
#[cfg(windows)]
pub fn list() -> Result<Vec<(String, f32, bool, f32, bool)>, String> {
    std::thread::spawn(|| unsafe { imp::list() }).join().map_err(|_| "сбой микшера громкости".to_string())?
}

#[cfg(not(windows))]
pub fn apply(_source: Option<String>, _change: VolumeChange) -> Result<VolumeResult, String> {
    Err("Громкость программ есть только в Windows".into())
}

#[cfg(windows)]
mod imp {
    use super::{next_level, pretty_app, source_matches, VolumeChange, VolumeResult};
    use windows::core::{Interface, PWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
    use windows::Win32::Media::Audio::{
        eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
        ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// Звуковой поток одной программы на одном устройстве.
    struct Stream {
        exe: String,
        volume: ISimpleAudioVolume,
        peak: f32,
        active: bool,
    }

    /// Имя exe процесса без пути: «chrome.exe».
    unsafe fn exe_name(pid: u32) -> String {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
    }

    /// Потоки всех программ на всех включённых устройствах вывода (кроме системных звуков и нас).
    unsafe fn streams() -> windows::core::Result<Vec<Stream>> {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let me = std::process::id();
        let mut out = Vec::new();
        for d in 0..devices.GetCount()? {
            let Ok(device) = devices.Item(d) else { continue };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
            for i in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(control) = sessions.GetSession(i) else { continue };
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                // S_OK — поток системных звуков.
                if control2.IsSystemSoundsSession().0 == 0 {
                    continue;
                }
                let pid = control2.GetProcessId().unwrap_or(0);
                if pid == 0 || pid == me {
                    continue;
                }
                let Ok(volume) = control.cast::<ISimpleAudioVolume>() else { continue };
                let peak = control.cast::<IAudioMeterInformation>().and_then(|m| m.GetPeakValue()).unwrap_or(0.0);
                let active = control.GetState().map(|s| s == AudioSessionStateActive).unwrap_or(false);
                out.push(Stream { exe: exe_name(pid), volume, peak, active });
            }
        }
        Ok(out)
    }

    pub(super) unsafe fn list() -> Result<Vec<(String, f32, bool, f32, bool)>, String> {
        let all = streams().map_err(|e| format!("микшер громкости: {e}"))?;
        Ok(all
            .iter()
            .map(|s| {
                let level = s.volume.GetMasterVolume().unwrap_or(0.0);
                let muted = s.volume.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                (s.exe.clone(), level, muted, s.peak, s.active)
            })
            .collect())
    }

    pub(super) unsafe fn run(source: Option<&str>, change: VolumeChange) -> Result<VolumeResult, String> {
        let all = streams().map_err(|e| format!("микшер громкости: {e}"))?;
        // 1. Программа из «Сейчас играет».
        let mut exe = source.and_then(|s| all.iter().find(|x| source_matches(s, &x.exe))).map(|x| x.exe.clone());
        // 2. Иначе — та, где сейчас громче всего играет звук.
        if exe.is_none() {
            exe = all
                .iter()
                .filter(|x| x.active && !x.exe.is_empty())
                .max_by(|a, b| a.peak.total_cmp(&b.peak))
                .map(|x| x.exe.clone());
        }
        let exe = exe.ok_or("Не нашёл программу, где играет звук")?;
        let targets: Vec<&Stream> = all.iter().filter(|x| x.exe == exe).collect();
        // Точка отсчёта — поток, из которого сейчас идёт звук (у браузера их бывает несколько).
        let first = targets
            .iter()
            .copied()
            .max_by(|a, b| (a.active, a.peak).partial_cmp(&(b.active, b.peak)).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(targets[0]);
        let current = first.volume.GetMasterVolume().unwrap_or(1.0);
        let muted = first.volume.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        let (level, mute) = next_level(current, muted, change);
        // У браузера бывает несколько потоков (по устройствам) — меняем все.
        for s in &targets {
            let _ = s.volume.SetMasterVolume(level, std::ptr::null());
            let _ = s.volume.SetMute(mute, std::ptr::null());
        }
        Ok(VolumeResult { app: pretty_app(&exe), level, muted: mute })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtc_sources_match_processes() {
        assert!(source_matches("Chrome", "chrome.exe"));
        assert!(source_matches("MSEdge", "msedge.exe"));
        assert!(source_matches("308046B0AF4A39CB", "firefox.exe"));
        assert!(source_matches("Spotify.exe", "Spotify.exe"));
        assert!(source_matches("YandexBrowser", "browser.exe"));
        assert!(source_matches("ru.yandex.desktop.music", "Яндекс Музыка.exe"));
        assert!(!source_matches("Chrome", "msedge.exe"));
        assert!(!source_matches("Spotify.exe", "chrome.exe"));
        assert!(!source_matches("", "chrome.exe"));
    }

    #[test]
    fn levels() {
        assert_eq!(next_level(0.5, false, VolumeChange::By(0.1)), (0.6, false));
        assert_eq!(next_level(0.05, false, VolumeChange::By(-0.1)), (0.0, false));
        assert_eq!(next_level(0.95, true, VolumeChange::By(0.1)), (1.0, false));
        assert_eq!(next_level(0.4, true, VolumeChange::By(-0.1)), (0.3, true));
        assert_eq!(next_level(0.4, false, VolumeChange::Set(0.3)), (0.3, false));
        assert_eq!(next_level(0.4, false, VolumeChange::Mute(true)), (0.4, true));
    }

    #[test]
    fn app_names() {
        assert_eq!(pretty_app("chrome.exe"), "Chrome");
        assert_eq!(pretty_app("msedge.exe"), "Edge");
        assert_eq!(pretty_app("browser.exe"), "Яндекс Браузер");
    }
}
