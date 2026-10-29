//! Голосовое управление «Джарвис»: офлайн-распознавание речи Vosk.
//!
//! `libvosk.dll` и модель лежат в папке `vosk` рядом с exe (при разработке —
//! в `resources/vosk`). Библиотека подгружается во время работы через
//! `libloading`, поэтому без неё программа собирается и работает как раньше,
//! просто голосовое управление сообщит, что модель не найдена.
//!
//! Схема: поток захвата (cpal) → моно 16 кГц i16 → рабочий поток с
//! распознавателем на полном словаре → поиск слова-активатора «Джарвис» →
//! разбор команды (`parse_command`, чистая функция с тестами) → событие в UI.

use std::ffi::{c_char, c_float, c_int, c_short, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;

use crate::notes::{self, Repeat};

/// Частота, на которой работает модель.
const SAMPLE_RATE: u32 = 16_000;
/// Сколько ждать команду после одиночного «Джарвис».
const ARMED_WINDOW: Duration = Duration::from_secs(6);

// ───────────────────────────── FFI ─────────────────────────────

type ModelNew = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type ModelFree = unsafe extern "C" fn(*mut c_void);
type RecNew = unsafe extern "C" fn(*mut c_void, c_float) -> *mut c_void;
type RecAccept = unsafe extern "C" fn(*mut c_void, *const c_short, c_int) -> c_int;
type RecText = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type RecVoid = unsafe extern "C" fn(*mut c_void);
type SetLogLevel = unsafe extern "C" fn(c_int);

/// Загруженная `libvosk.dll` с нужными функциями.
struct Vosk {
    _lib: libloading::Library,
    model_new: ModelNew,
    model_free: ModelFree,
    rec_new: RecNew,
    rec_accept: RecAccept,
    rec_result: RecText,
    rec_partial: RecText,
    rec_final: RecText,
    rec_reset: RecVoid,
    rec_free: RecVoid,
}

impl Vosk {
    fn load(dir: &Path) -> Result<Arc<Self>, String> {
        let dll = dir.join("libvosk.dll");
        // Зависимости (libstdc++ и т.п.) лежат рядом с libvosk.dll — ищем их там же.
        #[cfg(windows)]
        let lib = unsafe {
            libloading::os::windows::Library::load_with_flags(
                &dll,
                libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH,
            )
            .map(libloading::Library::from)
        };
        #[cfg(not(windows))]
        let lib = unsafe { libloading::Library::new(&dll) };
        let lib = lib.map_err(|e| format!("не удалось загрузить {}: {e}", dll.display()))?;
        unsafe {
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get($name).map_err(|e| format!("в libvosk нет функции: {e}"))?
                };
            }
            let set_log: SetLogLevel = sym!(b"vosk_set_log_level\0");
            set_log(-1); // Без логов Kaldi в консоль.
            Ok(Arc::new(Self {
                model_new: sym!(b"vosk_model_new\0"),
                model_free: sym!(b"vosk_model_free\0"),
                rec_new: sym!(b"vosk_recognizer_new\0"),
                rec_accept: sym!(b"vosk_recognizer_accept_waveform_s\0"),
                rec_result: sym!(b"vosk_recognizer_result\0"),
                rec_partial: sym!(b"vosk_recognizer_partial_result\0"),
                rec_final: sym!(b"vosk_recognizer_final_result\0"),
                rec_reset: sym!(b"vosk_recognizer_reset\0"),
                rec_free: sym!(b"vosk_recognizer_free\0"),
                _lib: lib,
            }))
        }
    }
}

struct Model {
    vosk: Arc<Vosk>,
    ptr: *mut c_void,
}

impl Model {
    fn new(vosk: &Arc<Vosk>, path: &Path) -> Result<Self, String> {
        let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let ptr = unsafe { (vosk.model_new)(c.as_ptr()) };
        if ptr.is_null() {
            return Err(format!("не удалось открыть модель {}", path.display()));
        }
        Ok(Self { vosk: vosk.clone(), ptr })
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        unsafe { (self.vosk.model_free)(self.ptr) }
    }
}

struct Recognizer {
    vosk: Arc<Vosk>,
    ptr: *mut c_void,
}

impl Recognizer {
    fn new(model: &Model) -> Result<Self, String> {
        let ptr = unsafe { (model.vosk.rec_new)(model.ptr, SAMPLE_RATE as c_float) };
        if ptr.is_null() {
            return Err("не удалось создать распознаватель".into());
        }
        Ok(Self { vosk: model.vosk.clone(), ptr })
    }

    /// `true` — фраза закончилась (пауза в речи), можно забрать `result()`.
    fn accept(&mut self, pcm: &[i16]) -> bool {
        unsafe { (self.vosk.rec_accept)(self.ptr, pcm.as_ptr(), pcm.len() as c_int) == 1 }
    }

    fn take_text(&self, f: RecText, key: &str) -> String {
        let raw = unsafe { f(self.ptr) };
        if raw.is_null() {
            return String::new();
        }
        let json = unsafe { CStr::from_ptr(raw) }.to_string_lossy();
        serde_json::from_str::<serde_json::Value>(&json)
            .ok()
            .and_then(|v| v.get(key).and_then(|t| t.as_str()).map(str::to_owned))
            .unwrap_or_default()
    }

    fn result(&self) -> String {
        self.take_text(self.vosk.rec_result, "text")
    }

    fn partial(&self) -> String {
        self.take_text(self.vosk.rec_partial, "partial")
    }

    fn final_result(&self) -> String {
        self.take_text(self.vosk.rec_final, "text")
    }

    fn reset(&mut self) {
        unsafe { (self.vosk.rec_reset)(self.ptr) }
    }
}

impl Drop for Recognizer {
    fn drop(&mut self) {
        unsafe { (self.vosk.rec_free)(self.ptr) }
    }
}

/// Папка с `libvosk.dll` и `model/`: рядом с exe, иначе — в исходниках проекта.
pub fn find_vosk_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("vosk"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join("vosk"));
    candidates
        .into_iter()
        .find(|d| d.join("libvosk.dll").is_file() && d.join("model").is_dir())
}

fn load_engine() -> Result<(Model, Recognizer), String> {
    let dir = find_vosk_dir().ok_or("модель не найдена (папка vosk рядом с программой)")?;
    let vosk = Vosk::load(&dir)?;
    let model = Model::new(&vosk, &dir.join("model"))?;
    let rec = Recognizer::new(&model)?;
    Ok((model, rec))
}

// ─────────────────────────── Команды ───────────────────────────

/// Что пользователь попросил сделать голосом.
#[derive(Clone, Debug, PartialEq)]
pub enum VoiceCommand {
    MusicPause,
    MusicPlay,
    MusicNext,
    MusicPrev,
    /// Громкость программы, где играет музыка (не системная).
    Volume(crate::volume::VolumeChange),
    /// Перемотка на N секунд (минус — назад).
    SeekBy(f64),
    /// Трек сначала.
    SeekStart,
    /// Новый таймер (сразу запущен). `name` — если пользователь его назвал.
    AddTimer { secs: u64, name: Option<String> },
    /// Новый секундомер (сразу запущен).
    AddStopwatch { name: Option<String> },
    PauseTimer,
    ResumeTimer,
    /// Остановить показанный таймер и записать в историю.
    StopTimer,
    Silence,
    ShowWidget,
    HideWidget,
    OpenMenu,
    CloseMenu,
    /// Новая заметка: текст, папка (как сказали), напоминание.
    AddNote { title: String, folders: Vec<String>, reminder: Option<(NaiveDateTime, Repeat)> },
    /// Отметка «выполнено» (`done`) или снятие отметки. `note` — явно названная
    /// заметка («в заметке курсовая …»), `target` — что отметить: пункт или сама заметка.
    /// `item_only` — сказали «пункт», названия заметок не проверять.
    Check { note: Option<String>, target: Option<String>, item_only: bool, done: bool },
    DeleteNote { query: String },
    /// Пункт чек-листа в заметку; без заметки — в текущую.
    AddNoteItem { note: Option<String>, item: String },
    /// Удалить пункт чек-листа; без заметки — ищется в текущей, потом везде.
    DeleteItem { note: Option<String>, item: String },
    /// Открыть заметку в панели (и сделать её текущей).
    OpenNote { query: String },
    ShowNotes,
    HideNotes,
    /// Удалить таймер или секундомер без записи в историю; без названия — показанный в виджете.
    DeleteTimer { query: Option<String> },
    CreateFolder { name: String },
    DeleteFolder { name: String },
    RenameFolder { from: String, to: String },
    /// Открыть панель на папке; `None` — все заметки.
    ShowFolder { name: Option<String> },
    /// Положить заметку в папки; `replace` — вместо прежних («перенеси»).
    NoteToFolder { query: String, folders: Vec<String>, replace: bool },
    NoteFromFolder { query: String, folder: String },
}

impl VoiceCommand {
    /// Короткий отклик для плашки виджета.
    pub fn feedback(&self) -> String {
        match self {
            Self::MusicPause => "Музыка на паузе".into(),
            Self::MusicPlay => "Играю музыку".into(),
            Self::MusicNext => "Следующий трек".into(),
            Self::MusicPrev => "Предыдущий трек".into(),
            Self::Volume(change) => match change {
                crate::volume::VolumeChange::By(d) if *d > 0.0 => format!("Громче на {}%", (d * 100.0).round()),
                crate::volume::VolumeChange::By(d) => format!("Тише на {}%", (-d * 100.0).round()),
                crate::volume::VolumeChange::Set(v) => format!("Громкость {}%", (v * 100.0).round()),
                crate::volume::VolumeChange::Mute(true) => "Звук выключен".into(),
                crate::volume::VolumeChange::Mute(false) => "Звук включён".into(),
            },
            Self::SeekBy(s) if *s < 0.0 => format!("Назад на {} с", (-s).round()),
            Self::SeekBy(s) => format!("Вперёд на {} с", s.round()),
            Self::SeekStart => "Трек сначала".into(),
            Self::AddTimer { secs, name } => match name {
                Some(n) => format!("Таймер «{n}» — {}", human_secs(*secs)),
                None => format!("Таймер на {}", human_secs(*secs)),
            },
            Self::AddStopwatch { name } => match name {
                Some(n) => format!("Секундомер «{n}»"),
                None => "Секундомер запущен".into(),
            },
            Self::PauseTimer => "Таймер на паузе".into(),
            Self::ResumeTimer => "Таймер идёт".into(),
            Self::StopTimer => "Таймер остановлен".into(),
            Self::Silence => "Тихо".into(),
            Self::ShowWidget => "Показываю виджет".into(),
            Self::HideWidget => "Скрываю виджет".into(),
            Self::OpenMenu => "Открываю меню".into(),
            Self::CloseMenu => "Закрываю меню".into(),
            Self::AddNote { title, reminder, .. } => match reminder {
                Some((at, repeat)) => format!(
                    "Заметка «{title}» · {}",
                    crate::notes::format_when(&crate::notes::Reminder::new(*at, *repeat), crate::notes::now())
                ),
                None => format!("Заметка «{title}»"),
            },
            Self::Check { target, note, done, .. } => {
                let what = target.as_ref().or(note.as_ref()).cloned().unwrap_or_default();
                if *done { format!("Отмечаю «{what}»") } else { format!("Снимаю отметку «{what}»") }
            }
            Self::DeleteNote { query } => format!("Удаляю заметку «{query}»"),
            Self::AddNoteItem { item, .. } => format!("Пункт «{item}»"),
            Self::DeleteItem { item, .. } => format!("Удаляю пункт «{item}»"),
            Self::OpenNote { query } => format!("Открываю «{query}»"),
            Self::ShowNotes => "Открываю заметки".into(),
            Self::HideNotes => "Скрываю заметки".into(),
            Self::DeleteTimer { query } => match query {
                Some(q) => format!("Удаляю «{q}»"),
                None => "Удаляю таймер".into(),
            },
            Self::CreateFolder { name } => format!("Папка «{name}» создана"),
            Self::DeleteFolder { name } => format!("Удаляю папку «{name}»"),
            Self::RenameFolder { to, .. } => format!("Папка теперь «{to}»"),
            Self::ShowFolder { name } => match name {
                Some(n) => format!("Папка «{n}»"),
                None => "Все заметки".into(),
            },
            Self::NoteToFolder { folders, .. } => format!("В папку {}", folders.join(", ")),
            Self::NoteFromFolder { folder, .. } => format!("Убираю из папки «{folder}»"),
        }
    }
}

fn human_secs(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h} ч"));
    }
    if m > 0 {
        parts.push(format!("{m} мин"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{s} с"));
    }
    parts.join(" ")
}

/// Нижний регистр, «ё» → «е», без пунктуации, по словам.
fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('ё', "е")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Расстояние Левенштейна по символам (для нечёткого «Джарвис»).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Похоже ли слово на «Джарвис». Малая модель может не знать этого слова и
/// записывать его созвучно — варианты собраны по прогонам на реальной речи.
pub fn is_wake_word(word: &str) -> bool {
    const EXACT: &[&str] = &[
        "джарвис", "джарвиз", "джарвес", "джервис", "жарвис", "джарвиса", "джарвисом",
        "дарвис", "чарвис", "джарвись", "джаврис", "джорвис",
    ];
    if EXACT.contains(&word) {
        return true;
    }
    let w = word.trim();
    let chars = w.chars().count();
    (5..=9).contains(&chars)
        && (w.starts_with("дж") || w.starts_with("жа") || w.starts_with("ча") || w.starts_with("дя"))
        && levenshtein(w, "джарвис") <= 2
}

/// Позиция слова-активатора (последнего, если сказали дважды).
fn wake_position(words: &[String]) -> Option<usize> {
    words.iter().rposition(|w| is_wake_word(w))
}

/// Распознанное слово-число: значение и «множитель-половинка».
pub(crate) fn number_word(w: &str) -> Option<f64> {
    if let Ok(n) = w.parse::<u32>() {
        return Some(n as f64);
    }
    Some(match w {
        "ноль" | "нуль" => 0.0,
        "один" | "одна" | "одну" | "одно" => 1.0,
        "два" | "две" => 2.0,
        "три" => 3.0,
        "четыре" => 4.0,
        "пять" => 5.0,
        "шесть" => 6.0,
        "семь" => 7.0,
        "восемь" => 8.0,
        "девять" => 9.0,
        "десять" => 10.0,
        "одиннадцать" => 11.0,
        "двенадцать" => 12.0,
        "тринадцать" => 13.0,
        "четырнадцать" => 14.0,
        "пятнадцать" => 15.0,
        "шестнадцать" => 16.0,
        "семнадцать" => 17.0,
        "восемнадцать" => 18.0,
        "девятнадцать" => 19.0,
        "двадцать" => 20.0,
        "тридцать" => 30.0,
        "сорок" => 40.0,
        "пятьдесят" => 50.0,
        "шестьдесят" => 60.0,
        "семьдесят" => 70.0,
        "восемьдесят" => 80.0,
        "девяносто" => 90.0,
        "сто" => 100.0,
        "двести" => 200.0,
        "триста" => 300.0,
        "полтора" | "полторы" => 1.5,
        "пол" | "половина" | "половину" => 0.5,
        _ => return None,
    })
}

/// Единица времени в секундах.
fn unit_word(w: &str) -> Option<f64> {
    if matches!(w, "час" | "часа" | "часов" | "часик" | "часика" | "часок" | "ч") {
        return Some(3600.0);
    }
    if w.starts_with("минут") || w.starts_with("минутк") || w == "мин" {
        return Some(60.0);
    }
    if w.starts_with("секунд") || w.starts_with("секундочк") || w == "сек" {
        return Some(1.0);
    }
    None
}

/// Слитные половинки: «полчаса», «полминуты».
fn half_unit_word(w: &str) -> Option<f64> {
    match w {
        "полчаса" | "получаса" => Some(1800.0),
        "полминуты" | "полминутки" => Some(30.0),
        _ => None,
    }
}

/// Длительность из слов: «час двадцать», «полтора часа», «пять минут тридцать
/// секунд», «полчаса». Возвращает секунды и отметки слов, ушедших в длительность.
pub(crate) fn parse_duration(words: &[String]) -> (Option<u64>, Vec<bool>) {
    let mut used = vec![false; words.len()];
    let mut total = 0.0;
    let mut found = false;
    // Накопленное число и номера его слов.
    let mut pending: Option<f64> = None;
    let mut pending_idx: Vec<usize> = Vec::new();
    let mut last_unit: Option<f64> = None;

    let smaller = |u: Option<f64>| match u {
        Some(u) if u >= 3600.0 => 60.0,
        Some(u) if u >= 60.0 => 1.0,
        Some(_) => 1.0,
        None => 60.0, // Голое число — минуты.
    };

    let flush = |pending: &mut Option<f64>, idx: &mut Vec<usize>, last: Option<f64>, total: &mut f64, used: &mut Vec<bool>, found: &mut bool| {
        if let Some(n) = pending.take() {
            *total += n * smaller(last);
            *found = true;
            for i in idx.drain(..) {
                used[i] = true;
            }
        }
        idx.clear();
    };

    for (i, w) in words.iter().enumerate() {
        if let Some(secs) = half_unit_word(w) {
            flush(&mut pending, &mut pending_idx, last_unit, &mut total, &mut used, &mut found);
            total += secs;
            found = true;
            used[i] = true;
            last_unit = Some(secs.max(60.0).min(3600.0));
            continue;
        }
        if let Some(n) = number_word(w) {
            // «двадцать пять»: младший разряд прибавляется к старшему.
            pending = Some(match pending {
                Some(p) if n < p && n >= 1.0 && p >= 20.0 && p % 10.0 == 0.0 => p + n,
                Some(p) if n < p && p >= 100.0 => p + n,
                Some(_) => {
                    flush(&mut pending, &mut pending_idx, last_unit, &mut total, &mut used, &mut found);
                    n
                }
                None => n,
            });
            pending_idx.push(i);
            continue;
        }
        if let Some(unit) = unit_word(w) {
            let n = pending.take().unwrap_or(1.0);
            total += n * unit;
            found = true;
            used[i] = true;
            for j in pending_idx.drain(..) {
                used[j] = true;
            }
            last_unit = Some(unit);
            continue;
        }
        // «с половиной»: «час с половиной» = 1.5 часа.
        if w == "с" && words.get(i + 1).map(|n| n.starts_with("половин")) == Some(true) {
            if let Some(u) = last_unit {
                total += u / 2.0;
                used[i] = true;
                used[i + 1] = true;
                continue;
            }
        }
        if w.starts_with("половин") && used.get(i).copied() == Some(true) {
            continue;
        }
        flush(&mut pending, &mut pending_idx, last_unit, &mut total, &mut used, &mut found);
    }
    flush(&mut pending, &mut pending_idx, last_unit, &mut total, &mut used, &mut found);

    let secs = total.round() as u64;
    (if found && secs > 0 { Some(secs) } else { None }, used)
}

fn has(words: &[String], stems: &[&str]) -> bool {
    words.iter().any(|w| stems.iter().any(|s| w.starts_with(s)))
}

/// Служебные слова, которые не входят в название таймера.
const FILLER: &[&str] = &[
    "поставь", "поставить", "поставим", "добавь", "добавить", "добавим", "создай", "создать",
    "заведи", "завести", "засеки", "засечь", "запусти", "запустить", "включи", "включить",
    "начни", "начать", "сделай", "новый", "новую", "новое", "еще", "один", "мне", "пожалуйста",
    "на", "с", "со", "под", "и", "по", "для", "таймер", "таймера", "таймеры", "таймером",
    "секундомер", "секундомера", "секундомером", "названием", "название", "назови", "назовем",
    "называется", "именем", "имя", "напоминание", "давай", "ка", "а", "ну", "это",
];

/// Название из оставшихся слов: первая буква заглавная.
fn extract_name(words: &[String], used: &[bool]) -> Option<String> {
    let rest: Vec<&str> = words
        .iter()
        .zip(used)
        .filter(|(w, u)| !**u && !FILLER.contains(&w.as_str()) && !is_wake_word(w))
        .map(|(w, _)| w.as_str())
        .collect();
    if rest.is_empty() {
        return None;
    }
    let joined = rest.join(" ");
    let mut chars = joined.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// Служебные слова в начале заметки: «добавь заметку», «напомни мне что…».
const NOTE_LEAD: &[&str] = &[
    "добавь", "добавить", "создай", "создать", "новая", "новую", "новое", "сделай", "заметка", "заметку",
    "заметки", "задача", "задачу", "запиши", "записать", "запомни", "запомнить", "напомни", "напомнить",
    "напоминание", "мне", "пожалуйста", "что", "чтобы", "о", "об", "про", "давай", "ка", "в", "на", "и",
];

/// Служебные слова в запросе «отметь заметку молоко выполненной».
const QUERY_FILLER: &[&str] = &[
    "отметь", "отметить", "отмечай", "отметьте", "заметку", "заметка", "заметки", "задачу", "задача", "как",
    "выполненной", "выполненную", "выполненным", "выполнена", "выполнено", "выполнил", "выполнила", "сделанной",
    "сделано", "сделал", "сделала", "готово", "готова", "удали", "удалить", "сотри", "убери", "таймер",
    "таймера", "секундомер", "секундомера", "про", "с", "на", "в", "мою", "мой", "эту", "этот", "пожалуйста",
    "названием", "под", "из", "заметке", "задаче", "заметкам",
    "поставь", "пункт", "пункты", "пункта", "галочку", "галочка", "отметку", "сними", "снять", "верни",
    "отмени", "не", "выполненным", "вычеркни", "у", "для", "это", "что",
];

/// Слова, на которых заканчивается название заметки в «в заметке курсовая отметь …».
fn is_action_word(w: &str) -> bool {
    [
        "отмет", "выполн", "сделал", "сделано", "готов", "вычеркн", "галочк", "поставь", "сними", "снять",
        "верни", "отмени", "пункт", "добав", "удали", "сотри", "открой", "покажи",
    ]
    .iter()
    .any(|s| w.starts_with(s))
}

/// Где в команде названа заметка: «в заметке X», «из заметки X», «заметку X».
/// Возвращает (начало области с предлогом, начало названия, конец названия).
/// Название идёт до первого слова-действия или предлога.
fn note_scope(words: &[String]) -> Option<(usize, usize, usize)> {
    let n = words.iter().position(|w| w.starts_with("заметк") || w.starts_with("задач") || w.starts_with("списк"))?;
    let start = if n > 0 && matches!(words[n - 1].as_str(), "в" | "во" | "из" | "у" | "для" | "к") { n - 1 } else { n };
    let mut end = n + 1;
    while end < words.len() && !is_action_word(&words[end]) && !matches!(words[end].as_str(), "в" | "во" | "на" | "из") {
        end += 1;
    }
    Some((start, n + 1, end))
}

/// Делит команду на заметку (если названа) и остальные слова.
fn split_scope(words: &[String]) -> (Option<String>, Vec<String>) {
    match note_scope(words) {
        Some((s, a, b)) => {
            let name = words[a..b].join(" ");
            let rest = words[..s].iter().chain(&words[b..]).cloned().collect();
            ((!name.is_empty()).then_some(name), rest)
        }
        None => (None, words.to_vec()),
    }
}

/// Отметка: «отметь контрольное», «в заметке курсовая отметь контрольное»,
/// «отметь заметку молоко», «сними отметку с хлеб».
fn parse_check(words: &[String]) -> Option<VoiceCommand> {
    let undo = (has(words, &["сними", "снять", "верни", "отмени"]) && has(words, &["отметк", "галочк", "выполн"]))
        || words.windows(2).any(|w| w[0] == "не" && w[1].starts_with("выполн"));
    if !undo && !has(words, &["отмет", "выполн", "сделал", "сделано", "готово", "вычеркн", "галочк"]) {
        return None;
    }
    let item_only = has(words, &["пункт"]);
    let (note, rest) = split_scope(words);
    let target = query_of(&rest);
    if note.is_none() && target.is_none() {
        return None;
    }
    Some(VoiceCommand::Check { note, target, item_only, done: !undo })
}

fn query_of(words: &[String]) -> Option<String> {
    let rest: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !QUERY_FILLER.contains(w) && !is_wake_word(w))
        .collect();
    (!rest.is_empty()).then(|| rest.join(" "))
}

/// Названия папок с позиции `start`: «работа», «работа и учёба». Возвращает их и конец.
fn folder_names(words: &[String], start: usize) -> (Vec<String>, usize) {
    let mut names = Vec::new();
    let mut j = start;
    if let Some(w) = words.get(j) {
        names.push(notes::capitalize(w));
        j += 1;
        while j + 1 < words.len() && words[j] == "и" {
            names.push(notes::capitalize(&words[j + 1]));
            j += 2;
        }
    }
    (names, j)
}

/// Команды папок: создать, удалить, переименовать, показать,
/// положить заметку в папку и убрать из неё.
fn parse_folder_command(words: &[String]) -> Option<VoiceCommand> {
    let fp = words.iter().position(|w| w.starts_with("папк"))?;
    let first = words[0].as_str();
    let tail = words[fp + 1..].join(" ");

    if fp <= 1 {
        // «создай папку учёба», «новая папка домашние дела».
        if matches!(first, "создай" | "создать" | "добавь" | "добавить" | "новая" | "новую" | "сделай" | "заведи") {
            return (!tail.is_empty()).then(|| VoiceCommand::CreateFolder { name: notes::capitalize(&tail) });
        }
        if matches!(first, "удали" | "удалить" | "сотри") {
            return (!tail.is_empty()).then(|| VoiceCommand::DeleteFolder { name: tail });
        }
        // «переименуй папку учёба в университет».
        if first.starts_with("переимен") {
            let rest = &words[fp + 1..];
            let k = rest.iter().position(|w| matches!(w.as_str(), "в" | "во" | "на"))?;
            let (from, to) = (rest[..k].join(" "), rest[k + 1..].join(" "));
            return (!from.is_empty() && !to.is_empty()).then(|| VoiceCommand::RenameFolder { from, to: notes::capitalize(&to) });
        }
        if matches!(first, "покажи" | "открой" | "отобрази") {
            return (!tail.is_empty()).then(|| VoiceCommand::ShowFolder { name: Some(tail) });
        }
    }

    // «убери заметку молоко из папки дом».
    if fp >= 2 && words[fp - 1] == "из" && matches!(first, "убери" | "убрать" | "удали" | "вынь" | "исключи" | "достань") {
        let query = query_of(&words[1..fp - 1])?;
        let folder = words.get(fp + 1)?.clone();
        return Some(VoiceCommand::NoteFromFolder { query, folder });
    }

    // «положи заметку молоко в папку дом», «назначь заметке отчёт папки работа и учёба»,
    // «перенеси отчёт в папку работа» (перенос — вместо прежних папок).
    let assign = matches!(
        first,
        "положи" | "помести" | "закинь" | "кинь" | "назначь" | "назначить" | "отнеси" | "отправь" | "перенеси" | "переложи" | "перемести"
    );
    if assign && fp >= 2 {
        let end = if matches!(words[fp - 1].as_str(), "в" | "во") { fp - 1 } else { fp };
        let query = query_of(&words[1..end])?;
        let (folders, _) = folder_names(words, fp + 1);
        if folders.is_empty() {
            return None;
        }
        let replace = matches!(first, "перенеси" | "переложи" | "перемести");
        return Some(VoiceCommand::NoteToFolder { query, folders, replace });
    }
    None
}

/// Заметка: «заметка купить молоко», «напомни завтра в 9 позвонить Ивану»,
/// «запиши в папку работа подготовить отчёт».
fn parse_note(words: &[String], now: NaiveDateTime) -> VoiceCommand {
    let mut used = vec![false; words.len()];
    // Папки: «в папку работа», «в папки работа и учёба».
    let mut folders = Vec::new();
    if let Some(p) = words.iter().position(|w| w.starts_with("папк")) {
        used[p] = true;
        if p > 0 && matches!(words[p - 1].as_str(), "в" | "во") {
            used[p - 1] = true;
        }
        let (names, end) = folder_names(words, p + 1);
        for flag in &mut used[p + 1..end] {
            *flag = true;
        }
        folders = names;
    }
    let reminder = notes::parse_when(words, now).map(|(w, marks)| {
        for (u, m) in used.iter_mut().zip(marks) {
            *u |= m;
        }
        (w.at, w.repeat)
    });
    let mut rest: Vec<&str> = words
        .iter()
        .zip(&used)
        .filter(|(w, u)| !**u && !w.starts_with("напом") && !is_wake_word(w))
        .map(|(w, _)| w.as_str())
        .collect();
    while rest.first().is_some_and(|w| NOTE_LEAD.contains(w)) {
        rest.remove(0);
    }
    while rest.last().is_some_and(|w| matches!(*w, "и" | "в" | "на" | "о" | "что")) {
        rest.pop();
    }
    let title = if rest.is_empty() { "Напоминание".to_string() } else { notes::capitalize(&rest.join(" ")) };
    VoiceCommand::AddNote { title, folders, reminder }
}

/// Команды заметок и удаления; `None` — это не про заметки.
fn parse_notes_command(words: &[String], now: NaiveDateTime) -> Option<VoiceCommand> {
    if let Some(cmd) = parse_folder_command(words) {
        return Some(cmd);
    }
    let first = words[0].as_str();
    let second = words.get(1).map(String::as_str).unwrap_or("");
    let note_word = |w: &str| w.starts_with("заметк") || w.starts_with("задач");
    let plural_notes = words.iter().any(|w| matches!(w.as_str(), "заметки" | "задачи" | "заметок" | "задач"));
    let timer_ctx = has(words, &["таймер", "секундомер"]);

    // Пункт чек-листа: «добавь пункт хлеб в заметку покупки», «добавь в заметку покупки пункт хлеб»,
    // «добавь пункт хлеб» — в текущую заметку.
    let adding = has(words, &["добав", "запиш", "внеси", "впиши"]);
    if let (true, Some(p)) = (adding, words.iter().position(|w| w.starts_with("пункт"))) {
        match words.iter().position(|w| note_word(w)) {
            Some(n) => {
                let (item_words, note_words): (&[String], &[String]) = if p < n {
                    // Между «пункт» и «в заметку» — пункт, после — заметка.
                    let end = if n > 0 && matches!(words[n - 1].as_str(), "в" | "во" | "к") { n - 1 } else { n };
                    (&words[p + 1..end.max(p + 1)], &words[n + 1..])
                } else {
                    (&words[p + 1..], &words[n + 1..p])
                };
                let item = item_words.join(" ");
                if let (false, Some(note)) = (item.is_empty(), query_of(note_words)) {
                    return Some(VoiceCommand::AddNoteItem { note: Some(note), item: notes::capitalize(&item) });
                }
            }
            None => {
                let item = words[p + 1..].join(" ");
                if !item.is_empty() {
                    return Some(VoiceCommand::AddNoteItem { note: None, item: notes::capitalize(&item) });
                }
            }
        }
    }

    // Удаление: «удали пункт хлеб [в заметке покупки]», «удали заметку молоко», «удали таймер чай».
    if has(words, &["удали", "удалить", "сотри", "стереть"]) {
        if timer_ctx {
            return Some(VoiceCommand::DeleteTimer { query: query_of(words) });
        }
        if has(words, &["пункт"]) {
            let (note, rest) = split_scope(words);
            return query_of(&rest).map(|item| VoiceCommand::DeleteItem { note, item });
        }
        if words.iter().any(|w| note_word(w)) {
            return query_of(words).map(|query| VoiceCommand::DeleteNote { query });
        }
    }

    // Одна заметка: «открой заметку курсовая».
    if !plural_notes && has(words, &["открой", "покажи", "отобрази"]) {
        if let (Some(query), _) = split_scope(words) {
            return Some(VoiceCommand::OpenNote { query });
        }
    }

    // Панель заметок: «покажи заметки», «закрой заметки», просто «заметки».
    if plural_notes && !timer_ctx {
        if has(words, &["скрой", "спрячь", "убери", "закрой", "сверни"]) {
            return Some(VoiceCommand::HideNotes);
        }
        if words.iter().any(|w| w == "все") {
            return Some(VoiceCommand::ShowFolder { name: None });
        }
        if words.len() == 1 || has(words, &["покажи", "открой", "мои", "отобрази", "список"]) {
            return Some(VoiceCommand::ShowNotes);
        }
    }

    // Новая заметка или напоминание — до отметки «сделано», чтобы
    // «заметка сделать отчёт» не превратилась в отметку.
    let starts_note = matches!(first, "заметка" | "заметку" | "запиши" | "записать" | "запомни" | "задача" | "задачу")
        || (matches!(first, "добавь" | "добавить" | "создай" | "создать" | "новая" | "новую" | "сделай") && note_word(second));
    let remind = words.iter().any(|w| w.starts_with("напом") || w == "напоминание");
    if starts_note || (remind && !timer_ctx) {
        return Some(parse_note(words, now));
    }

    // Отметка: «отметь заметку молоко», «в заметке курсовая отметь контрольное», «сними отметку с хлеб».
    if !timer_ctx {
        return parse_check(words);
    }
    None
}

/// Первое целое число в словах: «тридцать», «двадцать пять», «30».
fn first_number(words: &[String]) -> Option<f64> {
    let i = words.iter().position(|w| number_word(w).is_some())?;
    let n = number_word(&words[i])?;
    // «половину» → 50 %.
    if n > 0.0 && n < 1.0 {
        return Some(n * 100.0);
    }
    let units = words.get(i + 1).and_then(|w| number_word(w));
    Some(match units {
        Some(u) if n >= 20.0 && n % 10.0 == 0.0 && (1.0..10.0).contains(&u) => n + u,
        _ => n,
    })
}

/// Громкость: «громче», «тише» (±10 %), «громче на 20», «громкость 30»,
/// «громкость на максимум», «выключи звук», «включи звук».
fn parse_volume(words: &[String]) -> Option<VoiceCommand> {
    use crate::volume::VolumeChange;
    let louder = has(words, &["громче", "погромче", "прибав", "увелич"]);
    let quieter = has(words, &["тише", "потише", "убав", "уменьш", "приглуш"]);
    let volume_ctx = has(words, &["громкост", "звук"]);
    if !louder && !quieter && !volume_ctx {
        return None;
    }
    let pct = first_number(words).filter(|n| (0.0..=100.0).contains(n)).map(|n| n as f32 / 100.0);
    if louder {
        return Some(VoiceCommand::Volume(VolumeChange::By(pct.unwrap_or(0.1))));
    }
    if quieter {
        return Some(VoiceCommand::Volume(VolumeChange::By(-pct.unwrap_or(0.1))));
    }
    if has(words, &["выключи", "отключи", "убери", "без"]) {
        return Some(VoiceCommand::Volume(VolumeChange::Mute(true)));
    }
    if has(words, &["максим", "полн"]) {
        return Some(VoiceCommand::Volume(VolumeChange::Set(1.0)));
    }
    if let Some(p) = pct {
        return Some(VoiceCommand::Volume(VolumeChange::Set(p)));
    }
    if has(words, &["включи", "верни"]) {
        return Some(VoiceCommand::Volume(VolumeChange::Mute(false)));
    }
    None
}

/// Перемотка: «на 10 секунд назад», «вперёд на минуту», «перемотай назад», «трек сначала».
/// Просто «назад» / «вперёд» без времени — это переключение трека.
fn parse_seek(words: &[String]) -> Option<VoiceCommand> {
    if has(words, &["сначала"]) || words.windows(2).any(|w| w[0] == "с" && w[1] == "начала") {
        return Some(VoiceCommand::SeekStart);
    }
    let back = has(words, &["назад", "отмот"]);
    let forward = has(words, &["вперед"]);
    let seek_word = has(words, &["перемот", "промот", "отмот"]);
    if !back && !forward && !seek_word {
        return None;
    }
    // Голое число — секунды: «назад на 10».
    let has_unit = words.iter().any(|w| unit_word(w).is_some() || half_unit_word(w).is_some());
    let secs = if has_unit { parse_duration(words).0.map(|s| s as f64) } else { first_number(words) };
    let secs = match secs {
        Some(s) => s,
        None if seek_word => 10.0,
        None => return None,
    };
    Some(VoiceCommand::SeekBy(if back { -secs } else { secs }))
}

/// Разбор команды — слова ПОСЛЕ «Джарвис».
pub fn parse_command(text: &str) -> Option<VoiceCommand> {
    parse_command_at(text, notes::now())
}

/// То же с заданным «сейчас» — для напоминаний в тестах.
pub fn parse_command_at(text: &str, now: NaiveDateTime) -> Option<VoiceCommand> {
    let words = normalize(text);
    if words.is_empty() {
        return None;
    }
    if let Some(cmd) = parse_notes_command(&words, now) {
        return Some(cmd);
    }
    if let Some(cmd) = parse_volume(&words).or_else(|| parse_seek(&words)) {
        return Some(cmd);
    }
    let timer_ctx = has(&words, &["таймер"]);
    let stopwatch_ctx = has(&words, &["секундомер"]);
    let music_ctx = has(&words, &["музык", "трек", "песн", "плеер", "мелоди"]);

    // Будильник: «тихо», «хватит», «выключи будильник».
    if has(&words, &["тихо", "хватит", "замолчи", "заткнись", "будильник", "звонок", "сигнал"])
        && !music_ctx
    {
        return Some(VoiceCommand::Silence);
    }

    // Окно программы.
    let show = has(&words, &["покажи", "открой", "верни", "отобрази", "разверни"]);
    let hide = has(&words, &["скрой", "спрячь", "убери", "сверни", "закрой"]);
    if has(&words, &["виджет", "окно", "таймеры"]) && !timer_ctx || has(&words, &["виджет"]) {
        if hide {
            return Some(VoiceCommand::HideWidget);
        }
        if show {
            return Some(VoiceCommand::ShowWidget);
        }
    }
    if has(&words, &["меню", "настройк", "историю", "истори"]) {
        if hide {
            return Some(VoiceCommand::CloseMenu);
        }
        return Some(VoiceCommand::OpenMenu);
    }

    let stop = has(&words, &["останов", "стоп", "заверши", "законч", "сохрани", "сбрось", "выключи"]);
    let pause = has(&words, &["пауз", "приостанов", "подожди", "замри"]);
    let resume = has(&words, &["продолж", "возобнов", "дальше", "сними"]);
    let start = has(&words, &[
        "поставь", "поставить", "добавь", "добавить", "создай", "заведи", "засеки", "запусти",
        "включи", "начни", "новый", "сделай",
    ]);

    // Таймеры и секундомеры.
    if timer_ctx || stopwatch_ctx {
        let (secs, used) = parse_duration(&words);
        if timer_ctx {
            if let Some(secs) = secs {
                return Some(VoiceCommand::AddTimer { secs, name: extract_name(&words, &used) });
            }
        }
        if pause {
            return Some(VoiceCommand::PauseTimer);
        }
        if stop {
            return Some(VoiceCommand::StopTimer);
        }
        if resume {
            return Some(VoiceCommand::ResumeTimer);
        }
        if stopwatch_ctx {
            return Some(VoiceCommand::AddStopwatch { name: extract_name(&words, &used) });
        }
        if start {
            return Some(VoiceCommand::ResumeTimer);
        }
        return None;
    }

    // «Засеки пять минут» — таймер без слова «таймер».
    if start && !music_ctx {
        let (secs, used) = parse_duration(&words);
        if let Some(secs) = secs {
            return Some(VoiceCommand::AddTimer { secs, name: extract_name(&words, &used) });
        }
    }

    // Музыка.
    if has(&words, &["следующ", "дальше", "вперед", "переключи", "скип", "пропусти", "другую", "другой", "некст"]) {
        return Some(VoiceCommand::MusicNext);
    }
    if has(&words, &["предыдущ", "назад", "прошл", "обратно", "верни"]) {
        return Some(VoiceCommand::MusicPrev);
    }
    if pause || stop {
        return Some(VoiceCommand::MusicPause);
    }
    if has(&words, &["игра", "поигра", "сыгра", "плей", "плэй", "воспроизв", "продолж", "включи", "запусти", "музык"]) {
        return Some(VoiceCommand::MusicPlay);
    }
    None
}

/// Что делать с распознанной фразой.
#[derive(Debug, PartialEq)]
enum Heard {
    /// Нет слова-активатора и мы не ждём команду.
    Ignore,
    /// Только «Джарвис» — ждём команду следующей фразой.
    Armed,
    /// Текст команды (после «Джарвис» или в окне ожидания).
    Command(String),
}

fn classify(text: &str, armed: bool) -> Heard {
    let words = normalize(text);
    match wake_position(&words) {
        Some(p) => {
            let rest = words[p + 1..].join(" ");
            if rest.is_empty() { Heard::Armed } else { Heard::Command(rest) }
        }
        None if armed && !words.is_empty() => Heard::Command(words.join(" ")),
        None => Heard::Ignore,
    }
}

// ─────────────────────────── Поток ───────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum VoiceStatus {
    Loading,
    Ready,
    Error(String),
}

#[derive(Clone, Debug)]
pub enum VoiceEvent {
    Status(VoiceStatus),
    /// «Джарвис» услышан — показать индикатор (true) / погасить (false).
    Listening(bool),
    /// Команда разобрана (или не понята — тогда `command: None`).
    Heard { text: String, command: Option<VoiceCommand> },
    /// Любая распознанная фраза целиком — чтобы видеть, что слышит модель.
    Text(String),
    /// Какой микрофон реально открыт.
    Device(String),
}

/// Работающее голосовое управление. Drop — остановка.
pub struct Voice {
    rx: Receiver<VoiceEvent>,
    stop: Arc<AtomicBool>,
    /// Пиковый уровень последнего блока звука (биты f32, 0..1).
    level: Arc<AtomicU32>,
}

impl Voice {
    /// `device` — имя микрофона из `input_devices()`; `None` — микрофон Windows по умолчанию.
    pub fn start(ctx: eframe::egui::Context, device: Option<String>) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let level = Arc::new(AtomicU32::new(0));
        let (stop2, level2) = (stop.clone(), level.clone());
        std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || {
                let send = |e: VoiceEvent| {
                    let _ = tx.send(e);
                    ctx.request_repaint();
                };
                send(VoiceEvent::Status(VoiceStatus::Loading));
                if let Err(e) = run_voice(&stop2, &level2, device.as_deref(), &send) {
                    send(VoiceEvent::Status(VoiceStatus::Error(e)));
                }
            })
            .ok();
        Self { rx, stop, level }
    }

    pub fn poll(&self) -> Vec<VoiceEvent> {
        self.rx.try_iter().collect()
    }

    /// Текущий уровень сигнала микрофона, 0..1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

/// Имена микрофонов в системе.
pub fn input_devices() -> Vec<String> {
    use rodio::cpal::traits::HostTrait;
    use rodio::DeviceTrait;
    rodio::cpal::default_host()
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

impl Drop for Voice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Сводит кадры устройства к моно 16 кГц (линейная интерполяция).
struct Resampler {
    step: f64,
    pos: f64,
    prev: f32,
}

impl Resampler {
    fn new(src_rate: u32) -> Self {
        Self { step: src_rate as f64 / SAMPLE_RATE as f64, pos: 0.0, prev: 0.0 }
    }

    fn push(&mut self, mono: &[f32], out: &mut Vec<i16>) {
        // pos — позиция следующего выходного отсчёта относительно начала mono,
        // где индекс -1 — последний отсчёт прошлого блока (prev).
        let n = mono.len() as f64;
        while self.pos < n - 1.0 {
            let i = self.pos.floor();
            let frac = (self.pos - i) as f32;
            let a = if i < 0.0 { self.prev } else { mono[i as usize] };
            let b = mono[(i + 1.0) as usize];
            let v = a + (b - a) * frac;
            out.push((v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            self.pos += self.step;
        }
        self.pos -= n;
        if let Some(&last) = mono.last() {
            self.prev = last;
        }
    }
}

fn run_voice(stop: &AtomicBool, level: &AtomicU32, device_name: Option<&str>, send: &dyn Fn(VoiceEvent)) -> Result<(), String> {
    use rodio::cpal::traits::{HostTrait, StreamTrait};
    use rodio::cpal::{self, SampleFormat};
    use rodio::DeviceTrait;

    let (_model, mut rec) = load_engine()?;

    let host = cpal::default_host();
    // Выбранный микрофон; если его отключили — микрофон по умолчанию.
    let chosen = device_name.and_then(|name| {
        host.input_devices().ok()?.find(|d| d.name().is_ok_and(|n| n == name))
    });
    let device = match chosen {
        Some(d) => d,
        None => host.default_input_device().ok_or("микрофон не найден")?,
    };
    send(VoiceEvent::Device(device.name().unwrap_or_else(|_| "микрофон".into())));
    let config = device.default_input_config().map_err(|e| format!("микрофон: {e}"))?;
    let channels = config.channels() as usize;
    let format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.config();
    let mut resampler = Resampler::new(stream_config.sample_rate.0);

    let (atx, arx): (Sender<Vec<i16>>, Receiver<Vec<i16>>) = mpsc::channel();
    let err_flag = Arc::new(AtomicBool::new(false));
    let err_flag2 = err_flag.clone();
    let on_err = move |_e: cpal::StreamError| err_flag2.store(true, Ordering::Relaxed);

    // Кадры → моно f32 → 16 кГц i16 → рабочий цикл.
    let mut mono: Vec<f32> = Vec::new();
    let mut handle = move |frames: &mut dyn Iterator<Item = f32>| {
        mono.clear();
        let mut acc = 0.0;
        let mut k = 0;
        for s in frames {
            acc += s;
            k += 1;
            if k == channels {
                mono.push(acc / channels as f32);
                acc = 0.0;
                k = 0;
            }
        }
        let mut out = Vec::with_capacity(mono.len() / 2 + 2);
        resampler.push(&mono, &mut out);
        if !out.is_empty() {
            let _ = atx.send(out);
        }
    };

    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &_| handle(&mut data.iter().copied()),
            on_err,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &_| handle(&mut data.iter().map(|&s| s as f32 / 32768.0)),
            on_err,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _: &_| handle(&mut data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0)),
            on_err,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream(
            &stream_config,
            move |data: &[i32], _: &_| handle(&mut data.iter().map(|&s| s as f32 / 2_147_483_648.0)),
            on_err,
            None,
        ),
        other => return Err(format!("формат микрофона {other:?} не поддерживается")),
    }
    .map_err(|e| format!("микрофон: {e}"))?;
    stream.play().map_err(|e| format!("микрофон: {e}"))?;
    send(VoiceEvent::Status(VoiceStatus::Ready));

    let mut armed_until: Option<Instant> = None;
    let mut listening = false;
    while !stop.load(Ordering::Relaxed) {
        if err_flag.load(Ordering::Relaxed) {
            return Err("микрофон отключился".into());
        }
        let chunk = match arx.recv_timeout(Duration::from_millis(200)) {
            Ok(c) => Some(c),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("микрофон отключился".into()),
        };
        // Окно ожидания команды после одиночного «Джарвис» истекло.
        if armed_until.is_some_and(|t| Instant::now() > t) {
            armed_until = None;
            if listening {
                listening = false;
                send(VoiceEvent::Listening(false));
            }
        }
        let Some(chunk) = chunk else { continue };
        let peak = chunk.iter().map(|s| s.saturating_abs()).max().unwrap_or(0);
        level.store((peak as f32 / i16::MAX as f32).to_bits(), Ordering::Relaxed);
        let armed = armed_until.is_some();
        if rec.accept(&chunk) {
            let text = rec.result();
            if !text.trim().is_empty() {
                send(VoiceEvent::Text(text.clone()));
            }
            match classify(&text, armed) {
                Heard::Ignore => {
                    // В промежуточном тексте «Джарвис» был, а в итоговом — нет.
                    if listening && !armed {
                        listening = false;
                        send(VoiceEvent::Listening(false));
                    }
                }
                Heard::Armed => {
                    armed_until = Some(Instant::now() + ARMED_WINDOW);
                    if !listening {
                        listening = true;
                        send(VoiceEvent::Listening(true));
                    }
                }
                Heard::Command(cmd) => {
                    armed_until = None;
                    let command = parse_command(&cmd);
                    send(VoiceEvent::Heard { text: cmd, command });
                    listening = false;
                    send(VoiceEvent::Listening(false));
                }
            }
            rec.reset();
        } else if !listening {
            let partial = rec.partial();
            if wake_position(&normalize(&partial)).is_some() {
                listening = true;
                send(VoiceEvent::Listening(true));
            }
        }
    }
    drop(stream);
    Ok(())
}

// ─────────────────────── Проверка на файлах ───────────────────────

/// Простейший разбор WAV (PCM 16 бит): моно-отсчёты и частота.
fn read_wav(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("не WAV".into());
    }
    let (mut channels, mut rate, mut bits) = (1u16, 16_000u32, 16u16);
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + size).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            if bits != 16 {
                return Err(format!("нужен 16-битный WAV, а тут {bits}"));
            }
            let samples: Vec<f32> = body
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect();
            let mono = samples
                .chunks(channels.max(1) as usize)
                .map(|f| f.iter().sum::<f32>() / f.len() as f32)
                .collect();
            return Ok((mono, rate));
        }
        pos += 8 + size + (size & 1);
    }
    Err("нет блока data".into())
}

/// `timer_pro.exe --voice-test <папка>`: распознаёт все *.wav из папки и
/// печатает текст и разобранную команду.
pub fn run_file_test(dir: &Path) -> Result<(), String> {
    let (_model, mut rec) = load_engine()?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")))
        .collect();
    files.sort();
    for path in files {
        let (mono, rate) = read_wav(&path)?;
        let mut resampler = Resampler::new(rate);
        let mut pcm = Vec::new();
        resampler.push(&mono, &mut pcm);
        // Секунда тишины в конце — как пауза после фразы.
        pcm.extend(vec![0i16; SAMPLE_RATE as usize]);
        rec.reset();
        let mut texts = Vec::new();
        for chunk in pcm.chunks(3200) {
            if rec.accept(chunk) {
                texts.push(rec.result());
            }
        }
        texts.push(rec.final_result());
        let text = texts.into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" | ");
        let parsed = match classify(&text, false) {
            Heard::Command(cmd) => format!("{:?}", parse_command(&cmd)),
            other => format!("{other:?}"),
        };
        let expected = std::fs::read_to_string(path.with_extension("wav.txt")).unwrap_or_default();
        println!("{}\n  ожидалось: {}\n  услышано:  {text}\n  команда:   {parsed}", path.display(), expected.trim());
    }
    Ok(())
}

/// `timer_pro.exe --voice-listen <секунды>`: слушает микрофон и печатает события.
pub fn run_listen_test(secs: u64, device: Option<String>) {
    println!("Микрофоны: {:?}", input_devices());
    let voice = Voice::start(eframe::egui::Context::default(), device);
    let until = Instant::now() + Duration::from_secs(secs);
    let mut peak = 0.0f32;
    let mut tick = Instant::now();
    while Instant::now() < until {
        for event in voice.poll() {
            println!("{event:?}");
        }
        peak = peak.max(voice.level());
        if tick.elapsed() >= Duration::from_secs(1) {
            println!("уровень: {peak:.3}");
            (peak, tick) = (0.0, Instant::now());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Option<VoiceCommand> {
        parse_command(s)
    }

    fn timer(secs: u64, name: Option<&str>) -> Option<VoiceCommand> {
        Some(VoiceCommand::AddTimer { secs, name: name.map(str::to_owned) })
    }

    #[test]
    fn wake_word_variants() {
        assert!(is_wake_word("джарвис"));
        assert!(is_wake_word("джарвиз"));
        assert!(is_wake_word("джервис"));
        assert!(!is_wake_word("дарвин"));
        assert!(!is_wake_word("таймер"));
        assert!(!is_wake_word("джаз"));
    }

    #[test]
    fn classify_wake_and_armed() {
        assert_eq!(classify("джарвис", false), Heard::Armed);
        assert_eq!(classify("джарвис пауза", false), Heard::Command("пауза".into()));
        assert_eq!(classify("ну джарвис следующий трек", false), Heard::Command("следующий трек".into()));
        assert_eq!(classify("какая погода", false), Heard::Ignore);
        assert_eq!(classify("пауза", true), Heard::Command("пауза".into()));
    }

    #[test]
    fn music_commands() {
        assert_eq!(cmd("пауза"), Some(VoiceCommand::MusicPause));
        assert_eq!(cmd("поставь музыку на паузу"), Some(VoiceCommand::MusicPause));
        assert_eq!(cmd("стоп"), Some(VoiceCommand::MusicPause));
        assert_eq!(cmd("играй"), Some(VoiceCommand::MusicPlay));
        assert_eq!(cmd("играть"), Some(VoiceCommand::MusicPlay));
        assert_eq!(cmd("поиграй"), Some(VoiceCommand::MusicPlay));
        assert_eq!(cmd("включи музыку"), Some(VoiceCommand::MusicPlay));
        assert_eq!(cmd("продолжи"), Some(VoiceCommand::MusicPlay));
        assert_eq!(cmd("следующий трек"), Some(VoiceCommand::MusicNext));
        assert_eq!(cmd("дальше"), Some(VoiceCommand::MusicNext));
        assert_eq!(cmd("назад"), Some(VoiceCommand::MusicPrev));
        assert_eq!(cmd("предыдущая песня"), Some(VoiceCommand::MusicPrev));
    }

    #[test]
    fn timer_with_duration_and_name() {
        assert_eq!(cmd("поставь таймер на двадцать пять минут работа"), timer(1500, Some("Работа")));
        assert_eq!(cmd("таймер на полтора часа чтение"), timer(5400, Some("Чтение")));
        assert_eq!(cmd("таймер на пять минут"), timer(300, None));
        assert_eq!(cmd("добавь таймер на час двадцать с названием домашка"), timer(4800, Some("Домашка")));
        assert_eq!(cmd("таймер на полчаса"), timer(1800, None));
        assert_eq!(cmd("таймер на минуту"), timer(60, None));
        assert_eq!(cmd("таймер на две минуты тридцать секунд чай"), timer(150, Some("Чай")));
        assert_eq!(cmd("таймер на час с половиной"), timer(5400, None));
        assert_eq!(cmd("засеки десять минут"), timer(600, None));
        assert_eq!(cmd("поставь таймер на 15 минут английский язык"), timer(900, Some("Английский язык")));
        assert_eq!(cmd("таймер сорок пять"), timer(2700, None));
    }

    #[test]
    fn stopwatch_and_timer_control() {
        assert_eq!(cmd("запусти секундомер обед"), Some(VoiceCommand::AddStopwatch { name: Some("Обед".into()) }));
        assert_eq!(cmd("секундомер"), Some(VoiceCommand::AddStopwatch { name: None }));
        assert_eq!(cmd("останови таймер"), Some(VoiceCommand::StopTimer));
        assert_eq!(cmd("останови секундомер"), Some(VoiceCommand::StopTimer));
        assert_eq!(cmd("поставь таймер на паузу"), Some(VoiceCommand::PauseTimer));
        assert_eq!(cmd("продолжи таймер"), Some(VoiceCommand::ResumeTimer));
        assert_eq!(cmd("тихо"), Some(VoiceCommand::Silence));
        assert_eq!(cmd("выключи будильник"), Some(VoiceCommand::Silence));
    }

    #[test]
    fn window_commands() {
        assert_eq!(cmd("скрой виджет"), Some(VoiceCommand::HideWidget));
        assert_eq!(cmd("покажи виджет"), Some(VoiceCommand::ShowWidget));
        assert_eq!(cmd("открой меню"), Some(VoiceCommand::OpenMenu));
        assert_eq!(cmd("закрой настройки"), Some(VoiceCommand::CloseMenu));
        assert_eq!(cmd("какая погода"), None);
    }

    fn now() -> NaiveDateTime {
        NaiveDateTime::parse_from_str("2026-10-22 14:00", "%Y-%m-%d %H:%M").unwrap()
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn note(title: &str, folder: Option<&str>, reminder: Option<(&str, Repeat)>) -> Option<VoiceCommand> {
        Some(VoiceCommand::AddNote {
            title: title.into(),
            folders: folder.map(str::to_owned).into_iter().collect(),
            reminder: reminder.map(|(s, r)| (at(s), r)),
        })
    }

    #[test]
    fn note_commands() {
        let c = |s: &str| parse_command_at(s, now());
        assert_eq!(c("заметка купить молоко"), note("Купить молоко", None, None));
        assert_eq!(c("добавь заметку купить молоко и хлеб"), note("Купить молоко и хлеб", None, None));
        assert_eq!(c("запиши в папку работа подготовить отчет"), note("Подготовить отчет", Some("Работа"), None));
        assert_eq!(
            c("напомни завтра в девять позвонить ивану"),
            note("Позвонить ивану", None, Some(("2026-10-23 09:00", Repeat::Once)))
        );
        assert_eq!(
            c("напомни через двадцать минут проверить духовку"),
            note("Проверить духовку", None, Some(("2026-10-22 14:20", Repeat::Once)))
        );
        assert_eq!(
            c("напоминай каждый день в восемь выпить таблетки"),
            note("Выпить таблетки", None, Some(("2026-10-23 08:00", Repeat::Daily)))
        );
        assert_eq!(
            c("заметка по будням в десять стендап в папку работа"),
            note("Стендап", Some("Работа"), Some(("2026-10-23 10:00", Repeat::Weekdays)))
        );
        assert_eq!(c("напомни в шесть вечера"), note("Напоминание", None, Some(("2026-10-22 18:00", Repeat::Once))));
        // «заметка сделать…» — не отметка.
        assert_eq!(c("заметка сделать отчет"), note("Сделать отчет", None, None));
    }

    #[test]
    fn folder_commands() {
        let c = |s: &str| parse_command_at(s, now());
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(c("создай папку учеба"), Some(VoiceCommand::CreateFolder { name: "Учеба".into() }));
        assert_eq!(c("новая папка домашние дела"), Some(VoiceCommand::CreateFolder { name: "Домашние дела".into() }));
        assert_eq!(c("удали папку учеба"), Some(VoiceCommand::DeleteFolder { name: "учеба".into() }));
        assert_eq!(
            c("переименуй папку учеба в университет"),
            Some(VoiceCommand::RenameFolder { from: "учеба".into(), to: "Университет".into() })
        );
        assert_eq!(c("покажи папку работа"), Some(VoiceCommand::ShowFolder { name: Some("работа".into()) }));
        assert_eq!(c("покажи все заметки"), Some(VoiceCommand::ShowFolder { name: None }));
        assert_eq!(
            c("положи заметку молоко в папку дом"),
            Some(VoiceCommand::NoteToFolder { query: "молоко".into(), folders: v(&["Дом"]), replace: false })
        );
        assert_eq!(
            c("назначь заметке отчет папки работа и учеба"),
            Some(VoiceCommand::NoteToFolder { query: "отчет".into(), folders: v(&["Работа", "Учеба"]), replace: false })
        );
        assert_eq!(
            c("перенеси отчет в папку работа"),
            Some(VoiceCommand::NoteToFolder { query: "отчет".into(), folders: v(&["Работа"]), replace: true })
        );
        assert_eq!(
            c("убери заметку молоко из папки дом"),
            Some(VoiceCommand::NoteFromFolder { query: "молоко".into(), folder: "дом".into() })
        );
        assert_eq!(
            c("запиши в папки работа и учеба подготовить доклад"),
            Some(VoiceCommand::AddNote { title: "Подготовить доклад".into(), folders: v(&["Работа", "Учеба"]), reminder: None })
        );
    }

    #[test]
    fn note_management_commands() {
        let c = |s: &str| parse_command_at(s, now());
        let check = |note: Option<&str>, target: Option<&str>, item_only: bool, done: bool| {
            Some(VoiceCommand::Check { note: note.map(Into::into), target: target.map(Into::into), item_only, done })
        };
        assert_eq!(c("отметь заметку молоко"), check(Some("молоко"), None, false, true));
        assert_eq!(c("отметь молоко выполненной"), check(None, Some("молоко"), false, true));
        assert_eq!(c("выполнено отчет"), check(None, Some("отчет"), false, true));
        assert_eq!(c("в заметке курсовая поставь выполнено контрольное"), check(Some("курсовая"), Some("контрольное"), false, true));
        assert_eq!(c("отметь контрольное в заметке курсовая"), check(Some("курсовая"), Some("контрольное"), false, true));
        assert_eq!(c("в заметке контрольная работа отметь вопрос три"), check(Some("контрольная работа"), Some("вопрос три"), false, true));
        assert_eq!(c("отметь пункт контрольное"), check(None, Some("контрольное"), true, true));
        assert_eq!(c("сними отметку с хлеб"), check(None, Some("хлеб"), false, false));
        assert_eq!(c("открой заметку курсовая"), Some(VoiceCommand::OpenNote { query: "курсовая".into() }));
        assert_eq!(c("добавь пункт хлеб"), Some(VoiceCommand::AddNoteItem { note: None, item: "Хлеб".into() }));
        assert_eq!(
            c("удали пункт хлеб в заметке покупки"),
            Some(VoiceCommand::DeleteItem { note: Some("покупки".into()), item: "хлеб".into() })
        );
        assert_eq!(c("удали заметку молоко"), Some(VoiceCommand::DeleteNote { query: "молоко".into() }));
        assert_eq!(
            c("добавь пункт хлеб в заметку покупки"),
            Some(VoiceCommand::AddNoteItem { note: Some("покупки".into()), item: "Хлеб".into() })
        );
        assert_eq!(
            c("добавь в заметку покупки пункт сыр"),
            Some(VoiceCommand::AddNoteItem { note: Some("покупки".into()), item: "Сыр".into() })
        );
        assert_eq!(c("покажи заметки"), Some(VoiceCommand::ShowNotes));
        assert_eq!(c("заметки"), Some(VoiceCommand::ShowNotes));
        assert_eq!(c("закрой заметки"), Some(VoiceCommand::HideNotes));
        assert_eq!(c("удали таймер чай"), Some(VoiceCommand::DeleteTimer { query: Some("чай".into()) }));
        assert_eq!(c("удали секундомер"), Some(VoiceCommand::DeleteTimer { query: None }));
        // Старые команды не сломались.
        assert_eq!(c("останови таймер"), Some(VoiceCommand::StopTimer));
        assert_eq!(c("пауза"), Some(VoiceCommand::MusicPause));
    }

    #[test]
    fn volume_and_seek_commands() {
        use crate::volume::VolumeChange::*;
        let v = |c| Some(VoiceCommand::Volume(c));
        assert_eq!(cmd("громче"), v(By(0.1)));
        assert_eq!(cmd("сделай погромче"), v(By(0.1)));
        assert_eq!(cmd("тише"), v(By(-0.1)));
        assert_eq!(cmd("громче на двадцать"), v(By(0.2)));
        assert_eq!(cmd("убавь звук на 30 процентов"), v(By(-0.3)));
        assert_eq!(cmd("громкость тридцать"), v(Set(0.3)));
        assert_eq!(cmd("поставь громкость на двадцать пять процентов"), v(Set(0.25)));
        assert_eq!(cmd("громкость на максимум"), v(Set(1.0)));
        assert_eq!(cmd("громкость на половину"), v(Set(0.5)));
        assert_eq!(cmd("выключи звук"), v(Mute(true)));
        assert_eq!(cmd("включи звук"), v(Mute(false)));
        // «тихо» — по-прежнему будильник.
        assert_eq!(cmd("тихо"), Some(VoiceCommand::Silence));

        assert_eq!(cmd("на десять секунд назад"), Some(VoiceCommand::SeekBy(-10.0)));
        assert_eq!(cmd("назад на 30"), Some(VoiceCommand::SeekBy(-30.0)));
        assert_eq!(cmd("вперед на минуту"), Some(VoiceCommand::SeekBy(60.0)));
        assert_eq!(cmd("перемотай на полторы минуты вперед"), Some(VoiceCommand::SeekBy(90.0)));
        assert_eq!(cmd("перемотай назад"), Some(VoiceCommand::SeekBy(-10.0)));
        assert_eq!(cmd("трек сначала"), Some(VoiceCommand::SeekStart));
        // Без времени — переключение трека, как раньше.
        assert_eq!(cmd("назад"), Some(VoiceCommand::MusicPrev));
        assert_eq!(cmd("вперед"), Some(VoiceCommand::MusicNext));
    }

    #[test]
    fn resampler_keeps_length_ratio() {
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        for _ in 0..10 {
            r.push(&vec![0.5; 4800], &mut out);
        }
        // 48000 отсчётов → ~16000.
        assert!((15_990..=16_010).contains(&out.len()), "{}", out.len());
        assert!(out.iter().all(|&s| (s - 16383).abs() <= 1));
    }
}
