use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub enum TimerMode {
    Timer(Duration),
    Stopwatch,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TimerInstance {
    pub id: u64,
    pub name: String,
    pub mode: TimerMode,

    // ИСПРАВЛЕНИЕ: Пропускаем это поле при сериализации,
    // так как Instant не реализует Serialize/Deserialize
    #[serde(skip)]
    pub start_time: Option<Instant>,

    pub accumulated_time: Duration,

    /// Таймер идёт: момент (мс Unix), на который `accumulated_time` точен.
    /// Сохраняется на диск, чтобы после перезапуска таймер продолжил идти
    /// и учёл время, пока программа была закрыта. `None` — на паузе.
    #[serde(default)]
    pub running_since_ms: Option<u64>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl TimerInstance {
    pub fn new(name: String, mode: TimerMode) -> Self {
        Self {
            id: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            name,
            mode,
            start_time: None,
            accumulated_time: Duration::ZERO,
            running_since_ms: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.start_time.is_some()
    }

    pub fn time_left(&self) -> Duration {
        match self.mode {
            TimerMode::Timer(total) => {
                let elapsed = self.elapsed_duration();
                if total > elapsed {
                    total - elapsed
                } else {
                    Duration::ZERO
                }
            }
            TimerMode::Stopwatch => self.elapsed_duration(),
        }
    }

    pub fn elapsed_duration(&self) -> Duration {
        let mut total = self.accumulated_time;
        if let Some(start) = self.start_time {
            total += start.elapsed();
        }
        total
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.elapsed_duration().as_secs()
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.mode, TimerMode::Timer(_)) && self.time_left().is_zero()
    }

    pub fn toggle(&mut self) {
        if let Some(start) = self.start_time.take() {
            self.accumulated_time += start.elapsed();
            self.running_since_ms = None;
        } else {
            self.start_time = Some(Instant::now());
            self.running_since_ms = Some(now_ms());
        }
    }

    /// Переносит уже отработанное время в accumulated_time, НЕ останавливая таймер.
    /// Нужно перед записью на диск: Instant не сериализуется, и без этого
    /// весь прогресс идущего таймера терялся бы при закрытии программы.
    pub fn sync(&mut self) {
        if let Some(start) = self.start_time {
            self.accumulated_time += start.elapsed();
            self.start_time = Some(Instant::now());
            self.running_since_ms = Some(now_ms());
        }
    }

    /// После загрузки с диска: таймер, который шёл при сохранении, продолжает
    /// идти, а время, пока программа была закрыта, засчитывается.
    pub fn resume_after_load(&mut self) {
        if let Some(since) = self.running_since_ms {
            let gap = now_ms().saturating_sub(since);
            self.accumulated_time += Duration::from_millis(gap);
            self.start_time = Some(Instant::now());
            self.running_since_ms = Some(now_ms());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer(secs: u64) -> TimerInstance {
        TimerInstance::new("t".into(), TimerMode::Timer(Duration::from_secs(secs)))
    }

    #[test]
    fn new_timer_is_paused_and_shows_full_duration() {
        let t = timer(60);
        assert!(!t.is_running());
        assert_eq!(t.time_left(), Duration::from_secs(60));
        assert_eq!(t.elapsed_secs(), 0);
    }

    #[test]
    fn toggle_starts_and_pauses() {
        let mut t = timer(60);
        t.toggle();
        assert!(t.is_running());
        t.toggle();
        assert!(!t.is_running());
    }

    #[test]
    fn timer_never_goes_below_zero() {
        let mut t = timer(10);
        t.accumulated_time = Duration::from_secs(999);
        assert_eq!(t.time_left(), Duration::ZERO);
        assert!(t.is_finished());
    }

    #[test]
    fn stopwatch_counts_up_and_is_never_finished() {
        let mut s = TimerInstance::new("s".into(), TimerMode::Stopwatch);
        s.accumulated_time = Duration::from_secs(42);
        assert_eq!(s.time_left(), Duration::from_secs(42));
        assert!(!s.is_finished());
    }

    #[test]
    fn sync_preserves_progress_and_keeps_running() {
        let mut t = TimerInstance::new("s".into(), TimerMode::Stopwatch);
        t.toggle();
        std::thread::sleep(Duration::from_millis(20));
        t.sync();
        assert!(t.is_running(), "sync не должен останавливать таймер");
        assert!(t.accumulated_time >= Duration::from_millis(15));
    }

    #[test]
    fn running_timer_resumes_after_reload_and_counts_the_gap() {
        let mut t = TimerInstance::new("s".into(), TimerMode::Stopwatch);
        t.toggle();
        t.sync();
        // Как будто программа была закрыта 5 секунд.
        t.running_since_ms = t.running_since_ms.map(|ms| ms - 5_000);
        let json = serde_json::to_string(&t).unwrap();
        let mut loaded: TimerInstance = serde_json::from_str(&json).unwrap();
        assert!(!loaded.is_running(), "Instant не сохраняется");
        loaded.resume_after_load();
        assert!(loaded.is_running());
        assert!(loaded.elapsed_duration() >= Duration::from_millis(4_900));
    }

    #[test]
    fn paused_timer_stays_paused_after_reload() {
        let mut t = TimerInstance::new("s".into(), TimerMode::Stopwatch);
        t.toggle();
        t.toggle();
        let mut loaded: TimerInstance = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        loaded.resume_after_load();
        assert!(!loaded.is_running());
        // Старый формат без поля — тоже на паузе.
        let old: TimerInstance = serde_json::from_str(r#"{"id":1,"name":"x","mode":"Stopwatch","accumulated_time":{"secs":3,"nanos":0}}"#).unwrap();
        assert_eq!(old.running_since_ms, None);
    }

    #[test]
    fn sync_on_paused_timer_does_nothing() {
        let mut t = timer(60);
        t.accumulated_time = Duration::from_secs(5);
        t.sync();
        assert!(!t.is_running(), "sync не должен запускать остановленный таймер");
        assert_eq!(t.accumulated_time, Duration::from_secs(5));
    }
}
