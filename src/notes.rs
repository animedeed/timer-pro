//! Заметки-задачи: папки, чек-листы, разовые и повторяющиеся напоминания.
//!
//! Здесь же разбор русских выражений времени («завтра в 9», «через 20 минут»,
//! «каждый понедельник в 12», «по будням в 10») — он общий для голосовых
//! команд и поля «Напомнить» в панели заметок. Всё — чистые функции с тестами.

use chrono::{Datelike, Duration as CDuration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};
use serde::{Deserialize, Serialize};

use crate::voice::{number_word, parse_duration};

// ───────────────────────────── Данные ─────────────────────────────

/// Как повторяется напоминание.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    Once,
    Daily,
    /// Пн–Пт.
    Weekdays,
    /// Раз в неделю, в день недели из `at`.
    Weekly,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Reminder {
    /// Ближайшее срабатывание (местное время).
    pub at: NaiveDateTime,
    pub repeat: Repeat,
    /// Разовое напоминание уже сработало (заметка «просрочена», пока не выполнена).
    #[serde(default)]
    pub fired: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NoteItem {
    pub text: String,
    pub done: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Note {
    pub id: u64,
    pub title: String,
    /// Описание (необязательно).
    pub body: String,
    /// Пункты чек-листа.
    pub items: Vec<NoteItem>,
    pub done: bool,
    /// Папки заметки — их может быть несколько.
    pub folders: Vec<u64>,
    /// Старый формат: одна папка. Читается и переносится в `folders`.
    #[serde(skip_serializing)]
    pub folder: Option<u64>,
    pub reminder: Option<Reminder>,
    pub created: NaiveDateTime,
    pub done_at: Option<NaiveDateTime>,
}

impl Default for Note {
    fn default() -> Self {
        Self {
            id: new_id(),
            title: String::new(),
            body: String::new(),
            items: Vec::new(),
            done: false,
            folders: Vec::new(),
            folder: None,
            reminder: None,
            created: now(),
            done_at: None,
        }
    }
}

impl Note {
    pub fn new(title: String, folders: Vec<u64>) -> Self {
        Self { title, folders, ..Default::default() }
    }

    /// Перенос старого поля `folder` в `folders` после загрузки.
    pub fn migrate(&mut self) {
        if let Some(f) = self.folder.take() {
            if !self.folders.contains(&f) {
                self.folders.push(f);
            }
        }
    }

    pub fn in_folder(&self, id: u64) -> bool {
        self.folders.contains(&id)
    }

    /// Клик по папке в карточке: добавить или убрать.
    pub fn toggle_folder(&mut self, id: u64) {
        if self.in_folder(id) {
            self.folders.retain(|f| *f != id);
        } else {
            self.folders.push(id);
        }
    }

    /// Выполнено пунктов / всего, если чек-лист есть.
    pub fn progress(&self) -> Option<(usize, usize)> {
        (!self.items.is_empty()).then(|| (self.items.iter().filter(|i| i.done).count(), self.items.len()))
    }

    /// Разовое напоминание сработало, а заметка не выполнена.
    pub fn overdue(&self) -> bool {
        !self.done && self.reminder.as_ref().is_some_and(|r| r.fired)
    }

    pub fn set_done(&mut self, done: bool) {
        self.done = done;
        self.done_at = done.then(now);
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Folder {
    pub id: u64,
    pub name: String,
    pub color: [u8; 3],
}

/// Цвета папок по кругу.
pub const FOLDER_COLORS: [[u8; 3]; 6] = [
    [0x8B, 0x7C, 0xF6],
    [0x2E, 0xC4, 0x8D],
    [0xF5, 0x9E, 0x42],
    [0x4D, 0xA3, 0xFF],
    [0xF0, 0x6E, 0xA9],
    [0xC9, 0xC2, 0x4A],
];

/// Порядок списка заметок.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NotesSort {
    /// Новые сверху.
    #[default]
    Newest,
    /// Старые сверху.
    Oldest,
    /// Ближайшие напоминания сверху, без напоминания — в конце.
    Reminder,
    /// По алфавиту.
    Alpha,
    /// Группами по папкам (в порядке папок), без папки — в конце.
    Folder,
}

impl NotesSort {
    /// Порядок пунктов в выпадающем меню.
    pub const ALL: [NotesSort; 5] = [Self::Newest, Self::Oldest, Self::Reminder, Self::Alpha, Self::Folder];

    pub fn label(self) -> &'static str {
        match self {
            Self::Newest => "Сначала новые",
            Self::Oldest => "Сначала старые",
            Self::Reminder => "По напоминаниям",
            Self::Alpha => "По алфавиту",
            Self::Folder => "По папкам",
        }
    }
}

pub fn new_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

pub fn now() -> NaiveDateTime {
    Local::now().naive_local()
}

/// Сортирует номера заметок по выбранному порядку.
pub fn sort_indices(notes: &[Note], folders: &[Folder], idx: &mut [usize], sort: NotesSort) {
    match sort {
        NotesSort::Newest => idx.sort_by(|&a, &b| notes[b].created.cmp(&notes[a].created)),
        NotesSort::Oldest => idx.sort_by(|&a, &b| notes[a].created.cmp(&notes[b].created)),
        NotesSort::Folder => {
            // Место папки в списке папок; без папки — после всех.
            let pos = |n: &Note| {
                n.folders
                    .iter()
                    .filter_map(|f| folders.iter().position(|x| x.id == *f))
                    .min()
                    .unwrap_or(usize::MAX)
            };
            idx.sort_by(|&a, &b| {
                pos(&notes[a]).cmp(&pos(&notes[b])).then(notes[b].created.cmp(&notes[a].created))
            })
        }
        NotesSort::Reminder => idx.sort_by(|&a, &b| {
            let ka = notes[a].reminder.as_ref().map(|r| r.at);
            let kb = notes[b].reminder.as_ref().map(|r| r.at);
            match (ka, kb) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => notes[b].created.cmp(&notes[a].created),
            }
        }),
        NotesSort::Alpha => idx.sort_by_key(|&i| notes[i].title.to_lowercase()),
    }
}

// ─────────────────────────── Напоминания ───────────────────────────

fn repeat_matches(repeat: Repeat, t: NaiveDateTime) -> bool {
    match repeat {
        Repeat::Weekdays => t.weekday().num_days_from_monday() < 5,
        _ => true,
    }
}

/// Следующее срабатывание после `now` в том же времени суток.
pub fn next_after(at: NaiveDateTime, repeat: Repeat, now: NaiveDateTime) -> NaiveDateTime {
    let step = if repeat == Repeat::Weekly { CDuration::days(7) } else { CDuration::days(1) };
    let mut t = at;
    for _ in 0..2000 {
        t += step;
        if t > now && repeat_matches(repeat, t) {
            return t;
        }
    }
    t
}

impl Reminder {
    pub fn new(at: NaiveDateTime, repeat: Repeat) -> Self {
        Self { at, repeat, fired: false }
    }

    pub fn due(&self, now: NaiveDateTime) -> bool {
        !self.fired && now >= self.at
    }

    /// После срабатывания: разовое помечается, повторяющееся переносится вперёд.
    pub fn advance(&mut self, now: NaiveDateTime) {
        match self.repeat {
            Repeat::Once => self.fired = true,
            r => self.at = next_after(self.at, r, now),
        }
    }
}

const WEEKDAYS_SHORT: [&str; 7] = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];
const MONTHS_SHORT: [&str; 12] = ["янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек"];

/// Подпись напоминания: «сегодня 18:30», «завтра 9:00», «пт 12:00»,
/// «ежедневно 9:00», «по будням 10:00», «по пт 12:00».
pub fn format_when(r: &Reminder, now: NaiveDateTime) -> String {
    let time = format!("{}:{:02}", r.at.hour(), r.at.minute());
    let wd = WEEKDAYS_SHORT[r.at.weekday().num_days_from_monday() as usize];
    match r.repeat {
        Repeat::Daily => format!("ежедневно {time}"),
        Repeat::Weekdays => format!("по будням {time}"),
        Repeat::Weekly => format!("по {wd} {time}"),
        Repeat::Once => {
            let days = (r.at.date() - now.date()).num_days();
            match days {
                0 => format!("сегодня {time}"),
                1 => format!("завтра {time}"),
                -1 => format!("вчера {time}"),
                2..=6 => format!("{wd} {time}"),
                _ => {
                    let m = MONTHS_SHORT[r.at.month0() as usize];
                    format!("{} {m} {time}", r.at.day())
                }
            }
        }
    }
}



// ─────────────────────── Разбор времени ───────────────────────

fn weekday_word(w: &str) -> Option<(Weekday, bool)> {
    // bool — множественное «по понедельникам» (= каждую неделю).
    let wd = if w.starts_with("понедельн") {
        Weekday::Mon
    } else if w.starts_with("вторник") {
        Weekday::Tue
    } else if matches!(w, "среда" | "среду" | "среды" | "средам") {
        Weekday::Wed
    } else if w.starts_with("четверг") {
        Weekday::Thu
    } else if w.starts_with("пятниц") {
        Weekday::Fri
    } else if w.starts_with("суббот") {
        Weekday::Sat
    } else if w.starts_with("воскресен") {
        Weekday::Sun
    } else {
        return None;
    };
    Some((wd, w.ends_with("ам")))
}

fn is_duration_word(w: &str) -> bool {
    number_word(w).is_some()
        || matches!(w, "час" | "часа" | "часов" | "часик" | "полчаса" | "полминуты" | "с" | "половиной" | "и")
        || w.starts_with("минут")
        || w.starts_with("секунд")
}

/// Число из слов начиная с `j`: «двадцать три» → 23. Возвращает число и сколько слов заняло.
fn take_number(words: &[String], j: usize) -> Option<(u32, usize)> {
    let first = number_word(words.get(j)?)?;
    if first.fract() != 0.0 || first < 0.0 {
        return None;
    }
    let mut n = first as u32;
    let mut used = 1;
    if n >= 20 && n % 10 == 0 {
        if let Some(units) = words.get(j + 1).and_then(|w| number_word(w)) {
            if units >= 1.0 && units < 10.0 && units.fract() == 0.0 {
                n += units as u32;
                used = 2;
            }
        }
    }
    Some((n, used))
}

/// Результат разбора времени.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct When {
    pub at: NaiveDateTime,
    pub repeat: Repeat,
}

/// Ищет в словах выражение времени. Возвращает его и отметки слов, которые
/// в него вошли (чтобы убрать их из текста заметки).
pub fn parse_when(words: &[String], now: NaiveDateTime) -> Option<(When, Vec<bool>)> {
    let n = words.len();
    let mut used = vec![false; n];
    let today = now.date();
    let mut day: Option<NaiveDate> = None;
    let mut weekday: Option<Weekday> = None;
    let mut repeat = Repeat::Once;
    let mut time: Option<(u32, u32)> = None;
    let mut default_time = (9, 0);
    let mut relative: Option<u64> = None;
    let mut found = false;

    let mut i = 0;
    while i < n {
        let w = words[i].as_str();
        // «через 20 минут», «через полтора часа».
        if w == "через" {
            let k = words[i + 1..].iter().take_while(|w| is_duration_word(w)).count();
            if k > 0 {
                let (secs, marks) = parse_duration(&words[i + 1..i + 1 + k]);
                if let Some(secs) = secs {
                    relative = Some(secs);
                    used[i] = true;
                    for (m, flag) in marks.iter().enumerate() {
                        if *flag {
                            used[i + 1 + m] = true;
                        }
                    }
                    found = true;
                    i += 1 + k;
                    continue;
                }
            }
        }
        match w {
            "сегодня" => {
                day = Some(today);
                used[i] = true;
                found = true;
            }
            "завтра" => {
                day = Some(today + CDuration::days(1));
                used[i] = true;
                found = true;
            }
            "послезавтра" => {
                day = Some(today + CDuration::days(2));
                used[i] = true;
                found = true;
            }
            "ежедневно" => {
                repeat = Repeat::Daily;
                used[i] = true;
                found = true;
            }
            "каждый" | "каждое" | "каждую" | "каждого" => {
                match words.get(i + 1).map(String::as_str) {
                    Some("день" | "утро" | "вечер") => {
                        repeat = Repeat::Daily;
                        if words[i + 1] == "вечер" {
                            default_time = (19, 0);
                        }
                        used[i] = true;
                        used[i + 1] = true;
                        found = true;
                        i += 2;
                        continue;
                    }
                    Some("неделю") => {
                        repeat = Repeat::Weekly;
                        used[i] = true;
                        used[i + 1] = true;
                        found = true;
                        i += 2;
                        continue;
                    }
                    Some(next) if weekday_word(next).is_some() => {
                        repeat = Repeat::Weekly;
                        used[i] = true;
                    }
                    _ => {}
                }
            }
            "будням" | "будни" => {
                repeat = Repeat::Weekdays;
                used[i] = true;
                if i > 0 && words[i - 1] == "по" {
                    used[i - 1] = true;
                }
                found = true;
            }
            "утром" => {
                default_time = (9, 0);
                used[i] = true;
                found = true;
            }
            "днем" => {
                default_time = (13, 0);
                used[i] = true;
                found = true;
            }
            "вечером" => {
                default_time = (19, 0);
                used[i] = true;
                found = true;
            }
            "полдень" | "полночь" => {
                time = Some(if w == "полдень" { (12, 0) } else { (0, 0) });
                used[i] = true;
                if i > 0 && matches!(words[i - 1].as_str(), "в" | "к") {
                    used[i - 1] = true;
                }
                found = true;
            }
            "в" | "во" | "к" | "ко" => {
                // «в восемнадцать тридцать», «в 7 часов 15 минут», «в шесть вечера».
                if let Some((h, k)) = take_number(words, i + 1) {
                    let mut j = i + 1 + k;
                    let mut m = 0;
                    if words.get(j).is_some_and(|w| matches!(w.as_str(), "час" | "часа" | "часов")) {
                        j += 1;
                    }
                    if let Some((mm, k2)) = take_number(words, j) {
                        if mm < 60 {
                            m = mm;
                            j += k2;
                            if words.get(j).is_some_and(|w| w.starts_with("минут")) {
                                j += 1;
                            }
                        }
                    }
                    let mut h = h;
                    match words.get(j).map(String::as_str) {
                        Some("утра") => j += 1,
                        Some("дня" | "вечера") => {
                            if h < 12 {
                                h += 12;
                            }
                            j += 1;
                        }
                        Some("ночи") => {
                            if h == 12 {
                                h = 0;
                            }
                            j += 1;
                        }
                        _ => {}
                    }
                    if h <= 24 {
                        time = Some((h % 24, m));
                        for flag in &mut used[i..j] {
                            *flag = true;
                        }
                        found = true;
                        i = j;
                        continue;
                    }
                }
                // «в понедельник» — предлог уйдёт вместе с днём недели.
                if words.get(i + 1).is_some_and(|w| weekday_word(w).is_some()) {
                    used[i] = true;
                }
            }
            _ => {
                if let Some((wd, plural)) = weekday_word(w) {
                    weekday = Some(wd);
                    if plural {
                        repeat = Repeat::Weekly;
                        if i > 0 && words[i - 1] == "по" {
                            used[i - 1] = true;
                        }
                    }
                    used[i] = true;
                    found = true;
                }
            }
        }
        i += 1;
    }

    if !found {
        return None;
    }
    if let Some(secs) = relative {
        return Some((When { at: now + CDuration::seconds(secs as i64), repeat: Repeat::Once }, used));
    }

    let (h, m) = time.unwrap_or(default_time);
    let tod = NaiveTime::from_hms_opt(h, m, 0)?;
    let date = if let Some(d) = day {
        d
    } else if let Some(wd) = weekday {
        let ahead = (wd.num_days_from_monday() as i64 - today.weekday().num_days_from_monday() as i64).rem_euclid(7);
        let d = today + CDuration::days(ahead);
        // Сегодняшний день недели, но время уже прошло — через неделю.
        if ahead == 0 && d.and_time(tod) <= now { d + CDuration::days(7) } else { d }
    } else {
        today
    };
    let mut at = date.and_time(tod);
    if day.is_none() && (at <= now || !repeat_matches(repeat, at)) {
        at = next_after(at, if repeat == Repeat::Once { Repeat::Daily } else { repeat }, now);
    }
    Some((When { at, repeat }, used))
}

// ─────────────────────── Поиск по словам ───────────────────────

/// Слова совпадают с учётом окончаний: «молоко» ~ «молока», «иван» ~ «ивану».
pub fn word_match(a: &str, b: &str) -> bool {
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let shortest = a.chars().count().min(b.chars().count());
    common >= 4 || (common == shortest && shortest >= 2)
}

fn words_of(s: &str) -> Vec<String> {
    s.to_lowercase()
        .replace('ё', "е")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Сколько слов запроса нашлось в тексте.
pub fn match_score(query: &[String], text: &str) -> usize {
    let words = words_of(text);
    query.iter().filter(|q| words.iter().any(|w| word_match(q, w))).count()
}

/// Лучший кандидат `(id, текст)` по словам запроса. Нужно совпадение хотя бы
/// половины слов запроса; при равенстве побеждает более ранний в списке.
pub fn best_match<'a>(query: &str, candidates: impl Iterator<Item = (u64, &'a str)>) -> Option<u64> {
    let q: Vec<String> = words_of(query).into_iter().filter(|w| w.chars().count() >= 2).collect();
    if q.is_empty() {
        return None;
    }
    let need = q.len().div_ceil(2);
    let mut best: Option<(usize, u64)> = None;
    for (id, text) in candidates {
        let score = match_score(&q, text);
        if score >= need && best.is_none_or(|(s, _)| score > s) {
            best = Some((score, id));
        }
    }
    best.map(|(_, id)| id)
}

/// Заметка по запросу: сначала среди невыполненных, потом среди всех.
pub fn find_note(notes: &[Note], query: &str) -> Option<u64> {
    let mut order: Vec<&Note> = notes.iter().collect();
    order.sort_by_key(|n| (n.done, std::cmp::Reverse(n.created)));
    best_match(query, order.iter().map(|n| (n.id, n.title.as_str())))
}

/// Пункт чек-листа по словам. Сначала среди тех, что ещё не в нужном
/// состоянии: «отметь хлеб» найдёт неотмеченный «Хлеб», а не уже отмеченный.
pub fn find_item(note: &Note, query: &str, want_done: bool) -> Option<usize> {
    let mut order: Vec<usize> = (0..note.items.len()).collect();
    order.sort_by_key(|&k| note.items[k].done == want_done);
    best_match(query, order.iter().map(|&k| (k as u64, note.items[k].text.as_str()))).map(|k| k as usize)
}

/// Пункт во всех невыполненных заметках: (номер заметки, номер пункта).
/// Если пункт есть в нескольких заметках, но не отмечен только в одной — берётся она.
pub fn find_item_everywhere(notes: &[Note], query: &str, want_done: bool) -> Vec<(usize, usize)> {
    let hits: Vec<(usize, usize)> = notes
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.done)
        .filter_map(|(i, n)| find_item(n, query, want_done).map(|k| (i, k)))
        .collect();
    let pending: Vec<(usize, usize)> = hits.iter().copied().filter(|&(i, k)| notes[i].items[k].done != want_done).collect();
    if hits.len() > 1 && pending.len() == 1 { pending } else { hits }
}

/// Первая буква заглавная.
pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Разбор строки из поля «Напомнить»: «завтра в 9», «через 20 минут»…
pub fn parse_when_text(text: &str, now: NaiveDateTime) -> Option<When> {
    parse_when(&words_of(text), now).map(|(w, _)| w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    // 2026-10-22 — четверг.
    fn now() -> NaiveDateTime {
        at("2026-10-22 14:00")
    }

    fn when(s: &str) -> Option<(NaiveDateTime, Repeat)> {
        parse_when_text(s, now()).map(|w| (w.at, w.repeat))
    }

    #[test]
    fn relative_times() {
        assert_eq!(when("через 20 минут"), Some((at("2026-10-22 14:20"), Repeat::Once)));
        assert_eq!(when("через полтора часа"), Some((at("2026-10-22 15:30"), Repeat::Once)));
        assert_eq!(when("через час"), Some((at("2026-10-22 15:00"), Repeat::Once)));
        assert_eq!(when("через полчаса"), Some((at("2026-10-22 14:30"), Repeat::Once)));
    }

    #[test]
    fn clock_times() {
        assert_eq!(when("в восемнадцать тридцать"), Some((at("2026-10-22 18:30"), Repeat::Once)));
        assert_eq!(when("в 18 30"), Some((at("2026-10-22 18:30"), Repeat::Once)));
        assert_eq!(when("в шесть вечера"), Some((at("2026-10-22 18:00"), Repeat::Once)));
        // 9 утра уже прошло — завтра.
        assert_eq!(when("в девять утра"), Some((at("2026-10-23 09:00"), Repeat::Once)));
        assert_eq!(when("в двадцать три сорок пять"), Some((at("2026-10-22 23:45"), Repeat::Once)));
        assert_eq!(when("в 7 часов 15 минут"), Some((at("2026-10-23 07:15"), Repeat::Once)));
        assert_eq!(when("в полдень"), Some((at("2026-10-23 12:00"), Repeat::Once)));
    }

    #[test]
    fn days() {
        assert_eq!(when("завтра в девять"), Some((at("2026-10-23 09:00"), Repeat::Once)));
        assert_eq!(when("завтра"), Some((at("2026-10-23 09:00"), Repeat::Once)));
        assert_eq!(when("послезавтра вечером"), Some((at("2026-10-24 19:00"), Repeat::Once)));
        assert_eq!(when("в понедельник в 10"), Some((at("2026-10-26 10:00"), Repeat::Once)));
        assert_eq!(when("в пятницу"), Some((at("2026-10-23 09:00"), Repeat::Once)));
        // Четверг, 9:00 уже прошло — следующий четверг.
        assert_eq!(when("в четверг в 9"), Some((at("2026-10-29 09:00"), Repeat::Once)));
    }

    #[test]
    fn repeats() {
        assert_eq!(when("каждый день в восемь"), Some((at("2026-10-23 08:00"), Repeat::Daily)));
        assert_eq!(when("ежедневно в 20"), Some((at("2026-10-22 20:00"), Repeat::Daily)));
        assert_eq!(when("по будням в десять"), Some((at("2026-10-23 10:00"), Repeat::Weekdays)));
        assert_eq!(when("каждый понедельник в 12"), Some((at("2026-10-26 12:00"), Repeat::Weekly)));
        assert_eq!(when("по субботам"), Some((at("2026-10-24 09:00"), Repeat::Weekly)));
        assert_eq!(when("купить хлеб"), None);
    }

    #[test]
    fn repeat_advances() {
        let mut r = Reminder::new(at("2026-10-23 10:00"), Repeat::Weekdays); // пятница
        r.advance(at("2026-10-23 10:00"));
        assert_eq!(r.at, at("2026-10-26 10:00")); // понедельник
        let mut once = Reminder::new(at("2026-10-23 10:00"), Repeat::Once);
        assert!(once.due(at("2026-10-23 10:00")));
        once.advance(at("2026-10-23 10:00"));
        assert!(once.fired && !once.due(at("2026-10-24 10:00")));
        // Пропущенные (программа была закрыта) повторяющиеся — сразу на будущее.
        let mut daily = Reminder::new(at("2026-10-01 08:00"), Repeat::Daily);
        daily.advance(now());
        assert_eq!(daily.at, at("2026-10-23 08:00"));
    }

    #[test]
    fn formatting() {
        let f = |s: &str, r| format_when(&Reminder::new(at(s), r), now());
        assert_eq!(f("2026-10-22 18:30", Repeat::Once), "сегодня 18:30");
        assert_eq!(f("2026-10-23 09:00", Repeat::Once), "завтра 9:00");
        assert_eq!(f("2026-10-26 12:00", Repeat::Once), "пн 12:00");
        assert_eq!(f("2026-11-05 09:00", Repeat::Once), "5 ноя 9:00");
        assert_eq!(f("2026-10-23 08:00", Repeat::Daily), "ежедневно 8:00");
        assert_eq!(f("2026-10-26 12:00", Repeat::Weekly), "по пн 12:00");
    }

    #[test]
    fn old_single_folder_migrates() {
        let json = r#"{"id":5,"title":"x","folder":42,"created":"2026-10-22T10:00:00"}"#;
        let mut n: Note = serde_json::from_str(json).unwrap();
        n.migrate();
        assert_eq!(n.folders, vec![42]);
        assert!(!serde_json::to_string(&n).unwrap().contains("\"folder\""));
        n.toggle_folder(7);
        n.toggle_folder(42);
        assert_eq!(n.folders, vec![7]);
    }

    #[test]
    fn sorting() {
        let mk = |id: u64, title: &str, folder: Option<u64>, mins: i64| {
            let mut n = Note::new(title.into(), folder.into_iter().collect());
            n.id = id;
            n.created = at("2026-10-22 10:00") + CDuration::minutes(mins);
            n
        };
        let folders = vec![
            Folder { id: 100, name: "Работа".into(), color: FOLDER_COLORS[0] },
            Folder { id: 200, name: "Дом".into(), color: FOLDER_COLORS[1] },
        ];
        let notes = vec![mk(1, "Б", Some(200), 1), mk(2, "А", None, 2), mk(3, "В", Some(100), 3)];
        let order = |sort| {
            let mut idx = vec![0, 1, 2];
            sort_indices(&notes, &folders, &mut idx, sort);
            idx.iter().map(|&i| notes[i].id).collect::<Vec<_>>()
        };
        assert_eq!(order(NotesSort::Newest), vec![3, 2, 1]);
        assert_eq!(order(NotesSort::Oldest), vec![1, 2, 3]);
        assert_eq!(order(NotesSort::Alpha), vec![2, 1, 3]);
        assert_eq!(order(NotesSort::Folder), vec![3, 1, 2]);
    }

    #[test]
    fn item_search_prefers_pending_and_detects_ambiguity() {
        let item = |t: &str, done: bool| NoteItem { text: t.into(), done };
        let mut a = Note::new("МИНОВ".into(), vec![]);
        a.items = vec![item("Лекции", true), item("Контрольное", false)];
        let mut b = Note::new("Физика".into(), vec![]);
        b.items = vec![item("Контрольное", false)];
        let notes = vec![a, b];
        assert_eq!(find_item(&notes[0], "контрольное", true), Some(1));
        // В двух заметках, оба не отмечены — неоднозначно.
        assert_eq!(find_item_everywhere(&notes, "контрольное", true).len(), 2);
        // Лекции есть только в МИНОВ.
        assert_eq!(find_item_everywhere(&notes, "лекции", false), vec![(0, 0)]);
        let mut notes = notes;
        notes[1].items[0].done = true;
        // Во «Физике» уже отмечено — остаётся МИНОВ.
        assert_eq!(find_item_everywhere(&notes, "контрольное", true), vec![(0, 1)]);
    }

    #[test]
    fn fuzzy_note_search() {
        let mut a = Note::new("Купить молоко и хлеб".into(), vec![]);
        let mut b = Note::new("Позвонить Ивану".into(), vec![]);
        let c = Note::new("Подготовить отчёт".into(), vec![]);
        a.id = 1;
        b.id = 2;
        let mut c = c;
        c.id = 3;
        let notes = vec![a, b, c];
        assert_eq!(find_note(&notes, "молока"), Some(1));
        assert_eq!(find_note(&notes, "иван"), Some(2));
        assert_eq!(find_note(&notes, "отчет"), Some(3));
        assert_eq!(find_note(&notes, "погода"), None);
    }
}
