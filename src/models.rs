use serde::{Deserialize, Serialize};
use chrono::{DateTime, Local};
use std::path::PathBuf;
use crate::notes::{Folder, Note, NotesSort};
use crate::timer_logic::TimerInstance;

#[derive(Serialize, Deserialize, Clone)]
pub struct HistoryEntry {
    pub id: u64,
    pub date: DateTime<Local>,
    pub mode: String,
    pub duration_secs: u64,
    #[serde(default)]
    pub comment: String,
}

// serde(default) на уровне структуры: старые config.json без новых полей
// продолжают читаться, недостающие поля берутся из Default вместо сброса всего файла.
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct AppConfig {
    pub primary_color: [u8; 3],
    pub data_path: Option<PathBuf>, // Используем PathBuf для путей
    pub alarm_sound: Option<String>,
    pub alarm_volume: f32,
    pub always_on_top: bool,
    /// Экранная позиция левого верхнего угла виджета в логических точках.
    /// Устарела: на мониторах с разным масштабом точки не сопоставимы между
    /// собой. Читается только для перехода со старых конфигов.
    pub window_pos: Option<[f32; 2]>,
    /// Левый верхний угол виджета в физических пикселях рабочего стола —
    /// единая система координат для всех мониторов.
    pub widget_pos_px: Option<[i32; 2]>,
    /// Плашка «Сейчас играет» (системные медиаконтролы Windows).
    pub music_enabled: bool,
    /// Прятать плашку через 3 с, если ничего не играет; иначе — мини-плашка 32 px.
    pub music_hide_idle: bool,
    /// Ставить музыку на паузу, когда звенит будильник.
    pub music_pause_on_alarm: bool,
    /// Возобновлять музыку по «Тихо» или со стартом следующего фокус-таймера.
    pub music_resume_on_focus: bool,
    /// Плашка «Сейчас играет» закреплена над виджетом (иначе — под ним).
    pub music_above: bool,
    /// Прозрачный зазор между блоками, px.
    pub block_gap: f32,
    /// Уведомление Windows, когда таймер дошёл до нуля.
    pub notify_enabled: bool,
    /// Глобальные горячие клавиши.
    pub hotkeys_enabled: bool,
    pub hotkeys: crate::system::Bindings,
    /// Значок в трее.
    pub tray_enabled: bool,
    /// Крестик виджета сворачивает в трей, а не закрывает программу.
    pub close_to_tray: bool,
    /// Голосовое управление «Джарвис» (офлайн-модель Vosk).
    pub voice_enabled: bool,
    /// Микрофон для голосового управления; `None` — по умолчанию в Windows.
    pub voice_device: Option<String>,
    /// Порядок списка заметок.
    pub notes_sort: NotesSort,
    /// Светлая тема вместо тёмной.
    pub light_theme: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub active_timers: Vec<TimerInstance>, pub history: Vec<HistoryEntry>,
    /// Заметки-задачи и их папки.
    pub notes: Vec<Note>,
    pub folders: Vec<Folder>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            primary_color: [100, 120, 255],
            data_path: None,
            // None означает встроенный в бинарник звук, а не файл на диске.
            alarm_sound: None,
            alarm_volume: 1.0,
            always_on_top: true,
            window_pos: None,
            widget_pos_px: None,
            music_enabled: true,
            music_hide_idle: true,
            music_pause_on_alarm: true,
            music_resume_on_focus: false,
            music_above: false,
            block_gap: 6.0,
            notify_enabled: true,
            hotkeys_enabled: true,
            hotkeys: crate::system::Bindings::default(),
            tray_enabled: true,
            close_to_tray: true,
            voice_enabled: false,
            voice_device: None,
            notes_sort: NotesSort::default(),
            light_theme: false,
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            active_timers: Vec::new(),
            history: Vec::new(),
            notes: Vec::new(),
            folders: Vec::new(),
        }
    }
}
