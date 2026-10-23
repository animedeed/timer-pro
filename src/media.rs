//! Плашка «Сейчас играет»: системные медиаконтролы Windows
//! (GlobalSystemMediaTransportControls). Музыку мы не проигрываем — только
//! показываем, что играет в другом приложении, и переключаем его.
//!
//! WinRT-вызовы блокирующие, поэтому вся работа идёт в фоновом потоке:
//! он опрашивает текущую сессию раз в секунду и выполняет команды из канала.

use eframe::egui;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Sender;

/// Обложка трека, уменьшенная до 80×80 (40 px при 2x).
pub struct Cover {
    /// Меняется вместе с треком — по нему UI понимает, что текстуру пора обновить.
    pub key: u64,
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

#[derive(Clone)]
pub struct Snapshot {
    pub title: String,
    pub artist: String,
    pub source: String,
    pub playing: bool,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_toggle: bool,
    /// Позиция 0..1; None — источник не сообщает позицию.
    pub progress: Option<f32>,
    /// Длительность в секундах (0, если источник её не сообщает).
    pub duration_secs: f64,
    /// Источник поддерживает перемотку.
    pub can_seek: bool,
    pub cover: Option<Arc<Cover>>,
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    TogglePlayPause,
    Play,
    Pause,
    Next,
    Previous,
    /// Перемотать на позицию от начала трека, в секундах.
    Seek(f64),
}

pub struct MediaController {
    shared: Arc<Mutex<Option<Snapshot>>>,
    tx: Sender<Command>,
}

impl MediaController {
    pub fn start(ctx: egui::Context) -> Self {
        let shared = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("smtc".into())
            .spawn(move || backend::run(ctx, thread_shared, rx))
            .ok();
        Self { shared, tx }
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        self.shared.lock().ok().and_then(|s| s.clone())
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

/// Человеческое имя источника по AppUserModelId из SMTC.
pub fn friendly_source(aumid: &str) -> String {
    let id = aumid.to_lowercase();
    let browsers = [
        "chrome", "msedge", "firefox", "308046b0af4a39cb", "opera", "brave", "vivaldi",
        "browser", "arc.exe", "thebrowsercompany",
    ];
    if browsers.iter().any(|b| id.contains(b)) {
        return "Браузер".into();
    }
    if id.contains("yandex") {
        return "Яндекс Музыка".into();
    }
    if id.contains("spotify") {
        return "Spotify".into();
    }
    if id.contains("zunemusic") || id.contains("mediaplayer") {
        return "Медиаплеер".into();
    }
    if id.contains("vlc") {
        return "VLC".into();
    }
    if id.contains("telegram") {
        return "Telegram".into();
    }
    // Иначе — последний осмысленный кусок идентификатора без «.exe».
    let tail = aumid.rsplit(['!', '\\', '/']).next().unwrap_or(aumid);
    let tail = tail.strip_suffix(".exe").or_else(|| tail.strip_suffix(".EXE")).unwrap_or(tail);
    if tail.trim().is_empty() { "Плеер".into() } else { tail.to_string() }
}

#[cfg(windows)]
mod backend {
    use super::{friendly_source, Command, Cover, Snapshot};
    use eframe::egui;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };
    use windows::Storage::Streams::DataReader;

    const POLL: Duration = Duration::from_secs(1);
    /// После команды перечитываем состояние почти сразу, чтобы иконка play/pause
    /// не отставала на секунду.
    const AFTER_COMMAND: Duration = Duration::from_millis(150);
    const MAX_COVER_BYTES: u64 = 8 * 1024 * 1024;

    struct CoverCache {
        key: u64,
        cover: Option<Arc<Cover>>,
        /// Сколько раз уже читали обложку для текущего трека. Читаем дважды:
        /// сразу после смены трека SMTC иногда ещё отдаёт старую картинку.
        reads: u32,
        polls_since_change: u32,
    }

    pub fn run(ctx: egui::Context, shared: Arc<Mutex<Option<Snapshot>>>, rx: Receiver<Command>) {
        let manager = loop {
            match Manager::RequestAsync().and_then(|op| op.join()) {
                Ok(m) => break m,
                Err(_) => {
                    // SMTC недоступен (редкий случай) — пробуем позже, команды выбрасываем.
                    match rx.recv_timeout(Duration::from_secs(5)) {
                        Err(RecvTimeoutError::Disconnected) => return,
                        _ => continue,
                    }
                }
            }
        };

        let mut cache = CoverCache { key: 0, cover: None, reads: 0, polls_since_change: 0 };
        let mut last: Option<(String, String, String, bool, bool, bool, bool, bool, Option<i32>, u64)> = None;
        let mut next_poll = Instant::now();

        loop {
            let wait = next_poll.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(cmd) => {
                    if let Ok(session) = manager.GetCurrentSession() {
                        execute(&session, cmd);
                    }
                    next_poll = Instant::now() + AFTER_COMMAND;
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }

            let snap = manager.GetCurrentSession().ok().and_then(|s| read(&s, &mut cache).ok());

            // Будим UI только когда что-то видимое поменялось.
            let fingerprint = snap.as_ref().map(|s| {
                (
                    s.title.clone(),
                    s.artist.clone(),
                    s.source.clone(),
                    s.playing,
                    s.can_prev,
                    s.can_next,
                    s.can_toggle,
                    s.can_seek,
                    s.progress.map(|p| (p * 1000.0) as i32),
                    s.cover.as_ref().map(|c| c.key).unwrap_or(0),
                )
            });
            if fingerprint != last {
                last = fingerprint;
                if let Ok(mut guard) = shared.lock() {
                    *guard = snap;
                }
                ctx.request_repaint();
            }
            next_poll = Instant::now() + POLL;
        }
    }

    fn execute(session: &Session, cmd: Command) {
        let op = match cmd {
            Command::TogglePlayPause => session.TryTogglePlayPauseAsync(),
            Command::Play => session.TryPlayAsync(),
            Command::Pause => session.TryPauseAsync(),
            Command::Next => session.TrySkipNextAsync(),
            Command::Previous => session.TrySkipPreviousAsync(),
            Command::Seek(secs) => {
                // Позиция отсчитывается от StartTime в сотнях наносекунд.
                let start = session
                    .GetTimelineProperties()
                    .and_then(|tl| tl.StartTime())
                    .map(|t| t.Duration)
                    .unwrap_or(0);
                session.TryChangePlaybackPositionAsync(start + (secs.max(0.0) * 10_000_000.0) as i64)
            }
        };
        if let Ok(op) = op {
            let _ = op.join();
        }
    }

    /// Текущее время в формате Windows FILETIME (сотни наносекунд с 1601 года).
    fn now_filetime() -> i64 {
        const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
        let since_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        EPOCH_DIFF + (since_unix.as_nanos() / 100) as i64
    }

    fn read(session: &Session, cache: &mut CoverCache) -> windows::core::Result<Snapshot> {
        let props = session.TryGetMediaPropertiesAsync()?.join()?;
        let title = props.Title().map(|s| s.to_string_lossy()).unwrap_or_default();
        let artist = props.Artist().map(|s| s.to_string_lossy()).unwrap_or_default();
        let source = friendly_source(&session.SourceAppUserModelId().map(|s| s.to_string_lossy()).unwrap_or_default());

        let info = session.GetPlaybackInfo()?;
        let playing = info.PlaybackStatus().map(|s| s == Status::Playing).unwrap_or(false);
        let (can_prev, can_next, can_toggle, can_seek) = match info.Controls() {
            Ok(c) => (
                c.IsPreviousEnabled().unwrap_or(false),
                c.IsNextEnabled().unwrap_or(false),
                c.IsPlayPauseToggleEnabled().unwrap_or(false)
                    || c.IsPlayEnabled().unwrap_or(false)
                    || c.IsPauseEnabled().unwrap_or(false),
                c.IsPlaybackPositionEnabled().unwrap_or(false),
            ),
            Err(_) => (false, false, true, false),
        };

        let timeline = session.GetTimelineProperties().ok().and_then(|tl| {
            let start = tl.StartTime().ok()?.Duration;
            let end = tl.EndTime().ok()?.Duration;
            let mut pos = tl.Position().ok()?.Duration;
            let duration = end - start;
            if duration <= 0 {
                return None;
            }
            // Позиция сообщается на момент LastUpdatedTime — досчитываем до «сейчас».
            if playing {
                if let Ok(updated) = tl.LastUpdatedTime() {
                    if updated.UniversalTime > 0 {
                        pos += (now_filetime() - updated.UniversalTime).max(0);
                    }
                }
            }
            let pos = (pos - start).clamp(0, duration);
            Some((pos as f64 / 10_000_000.0, duration as f64 / 10_000_000.0))
        });
        let progress = timeline.map(|(pos, dur)| (pos / dur).clamp(0.0, 1.0) as f32);
        let duration_secs = timeline.map(|(_, dur)| dur).unwrap_or(0.0);

        let mut hasher = DefaultHasher::new();
        (&title, &artist, &source).hash(&mut hasher);
        let key = hasher.finish();
        if key != cache.key {
            cache.key = key;
            cache.cover = None;
            cache.reads = 0;
            cache.polls_since_change = 0;
        } else {
            cache.polls_since_change += 1;
        }
        let want_read = (cache.reads == 0) || (cache.reads == 1 && cache.polls_since_change == 2);
        if want_read {
            if let Some(c) = read_cover(&props, key) {
                cache.cover = Some(Arc::new(c));
            }
            cache.reads += 1;
        }

        Ok(Snapshot {
            title,
            artist,
            source,
            playing,
            can_prev,
            can_next,
            can_toggle,
            progress,
            duration_secs,
            can_seek: can_seek && timeline.is_some(),
            cover: cache.cover.clone(),
        })
    }

    fn read_cover(
        props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
        key: u64,
    ) -> Option<Cover> {
        let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
        let size = stream.Size().ok()?;
        if size == 0 || size > MAX_COVER_BYTES {
            return None;
        }
        let reader = DataReader::CreateDataReader(&stream).ok()?;
        reader.LoadAsync(size as u32).ok()?.join().ok()?;
        let mut bytes = vec![0u8; size as usize];
        reader.ReadBytes(&mut bytes).ok()?;
        let img = image::load_from_memory(&bytes).ok()?;
        let img = img.resize_to_fill(80, 80, image::imageops::FilterType::Triangle).to_rgba8();
        let (w, h) = img.dimensions();
        Some(Cover { key, size: [w as usize, h as usize], rgba: img.into_raw() })
    }
}

#[cfg(not(windows))]
mod backend {
    use super::{Command, Snapshot};
    use eframe::egui;
    use std::sync::mpsc::Receiver;
    use std::sync::{Arc, Mutex};

    /// Вне Windows системных медиаконтролов нет — плашка просто остаётся пустой.
    pub fn run(_ctx: egui::Context, _shared: Arc<Mutex<Option<Snapshot>>>, rx: Receiver<Command>) {
        while rx.recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::friendly_source;

    #[test]
    fn known_sources_get_readable_names() {
        assert_eq!(friendly_source("Chrome"), "Браузер");
        assert_eq!(friendly_source("MSEdge"), "Браузер");
        assert_eq!(friendly_source("308046B0AF4A39CB"), "Браузер");
        assert_eq!(friendly_source("Spotify.exe"), "Spotify");
        assert_eq!(friendly_source("ru.yandex.desktop.music"), "Яндекс Музыка");
    }

    #[test]
    fn yandex_browser_is_a_browser_not_music() {
        assert_eq!(friendly_source("YandexBrowser"), "Браузер");
    }

    #[test]
    fn unknown_source_falls_back_to_its_own_name() {
        assert_eq!(friendly_source("C:\\Apps\\foobar2000.exe"), "foobar2000");
        assert_eq!(friendly_source(""), "Плеер");
    }
}
