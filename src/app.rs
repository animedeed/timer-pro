//! Интерфейс по макету `design/Timer Pro.dc.html`: стопка «парящих» блоков
//! (виджет → плашка «Сейчас играет» → меню), растущая в ту сторону экрана,
//! где есть место. Виджет при этом остаётся на месте.

use crate::kit::{self, ButtonStyle, Content};
use crate::media::{Command as MediaCmd, MediaController, Snapshot};
use crate::models::{AppConfig, AppState, HistoryEntry};
use crate::notes::{self, Folder, Note, NoteItem, NotesSort, Reminder, Repeat};
use crate::storage;
use crate::system::{self, Action, Combo, Hotkeys, Tray, TrayCommand, TrayLook, TrayState};
use crate::theme::{self as t, ease_out};
use crate::timer_logic::{TimerInstance, TimerMode};
use crate::voice::{Voice, VoiceCommand, VoiceEvent, VoiceStatus};
use chrono::{Datelike, Local, NaiveDate, NaiveDateTime};
use eframe::egui::{
    self, pos2, text::LayoutJob, vec2, Align2, Color32, CursorIcon, Id, Pos2, Rect, Response,
    Sense, Stroke, StrokeKind, TextFormat, Ui, UiBuilder,
};
use egui_phosphor::regular as ph;
use rodio::{Decoder, OutputStream, Sink, Source};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const AUTOSAVE_EVERY: Duration = Duration::from_secs(30);

// Звук вшит в бинарник: относительный путь "resources/alarm.mp3" не находился
// после установки инсталлятором, потому что рабочая директория при запуске
// с ярлыка не совпадает с папкой программы.
const DEFAULT_ALARM: &[u8] = include_bytes!("../resources/alarm.mp3");

/// Страховка: проигрыш будильника не дольше двух минут, даже если выбран длинный файл.
const ALARM_LOOP_LIMIT: Duration = Duration::from_secs(120);

/// Сколько показывать мини-плашку после того, как музыка пропала.
const MUSIC_IDLE_GRACE: Duration = Duration::from_secs(3);

/// Перерыв — таймер не длиннее 15 минут: его старт музыку не трогает.
const BREAK_MAX: Duration = Duration::from_secs(15 * 60);

/// Сколько заметка, о которой говорили голосом, остаётся «текущей».
const VOICE_NOTE_CONTEXT: Duration = Duration::from_secs(5 * 60);

/// Сколько напоминание держится в строке виджета и сколько из них пульсирует обводка.
const NOTE_ALERT_HOLD: Duration = Duration::from_secs(60);
const NOTE_ALERT_PULSE: Duration = Duration::from_secs(10);

/// Действия со строками заметок — применяются после отрисовки списка.
enum NoteAct {
    Expand(u64),
    ToggleDone(u64),
    Delete(u64),
    ClearDone(Vec<u64>),
}

/// Действия с папками из строки чипов.
enum FolderAct {
    Select(Option<u64>),
    StartNew,
    Rename(u64),
    Recolor(u64),
    Delete(u64),
}

/// Поле ввода в стиле поля «Название» во вкладке таймеров.
fn text_field(ui: &mut Ui, r: Rect, id: Id, value: &mut String, hint: &str, accent: Color32) -> Response {
    let focused = ui.memory(|m| m.has_focus(id));
    ui.painter().rect(
        r, t::RADIUS_CTRL, t::SURFACE_2,
        Stroke::new(1.0, if focused { accent } else { t::BLOCK_STROKE }),
        StrokeKind::Inside,
    );
    let edit = egui::TextEdit::singleline(value)
        .id(id)
        .frame(egui::Frame::NONE)
        .hint_text(egui::RichText::new(hint).color(t::TEXT_TERTIARY))
        .font(t::regular(12.5))
        .text_color(t::TEXT_PRIMARY)
        .vertical_align(egui::Align::Center)
        .desired_width(r.width() - 20.0);
    ui.place(r.shrink2(vec2(10.0, 0.0)), edit)
}

/// Поле, растущее по высоте под текст (высоту считает `editor_layout`).
/// `newlines` — Enter переносит строку (описание); иначе только перенос по ширине,
/// а вставленные переводы строк заменяются пробелами (название, пункты).
#[allow(clippy::too_many_arguments)]
fn text_area(ui: &mut Ui, r: Rect, id: Id, value: &mut String, hint: &str, accent: Color32, font: egui::FontId, pad: egui::Vec2, newlines: bool) -> Response {
    let focused = ui.memory(|m| m.has_focus(id));
    ui.painter().rect(
        r, t::RADIUS_CTRL, t::SURFACE_2,
        Stroke::new(1.0, if focused { accent } else { t::BLOCK_STROKE }),
        StrokeKind::Inside,
    );
    let inner = r.shrink2(pad);
    let mut edit = egui::TextEdit::multiline(value)
        .id(id)
        .frame(egui::Frame::NONE)
        .margin(egui::Margin::ZERO)
        .hint_text(egui::RichText::new(hint).color(t::TEXT_TERTIARY))
        .font(font)
        .text_color(t::TEXT_PRIMARY)
        .desired_rows(1)
        .desired_width(inner.width());
    if !newlines {
        edit = edit.return_key(None);
    }
    let resp = ui.place(inner, edit);
    if !newlines && value.contains(['\n', '\r']) {
        *value = value.replace(['\r', '\n'], " ");
    }
    resp
}

/// Высота текста с переносом по ширине `wrap_w` (пустой — одна строка).
fn text_h(ui: &Ui, text: &str, font: egui::FontId, wrap_w: f32) -> f32 {
    let sample = if text.is_empty() { "Ag" } else { text };
    ui.painter().layout(sample.to_owned(), font, t::TEXT_PRIMARY, wrap_w.max(10.0)).size().y
}

/// Отступы полей карточки заметки.
const TITLE_PAD: egui::Vec2 = vec2(10.0, 6.0);
const BODY_PAD: egui::Vec2 = vec2(8.0, 6.0);
/// Слева от текста пункта — галочка (16 + 8), справа — крестик (26).
const ITEM_TEXT_INSET: f32 = 23.0;
const ITEM_TEXT_RIGHT: f32 = 26.0;

/// Высоты растущих полей раскрытой карточки при ширине содержимого `w`.
struct EditorLayout {
    title_h: f32,
    body_h: f32,
    items_h: Vec<f32>,
}

fn editor_layout(ui: &Ui, note: &Note, w: f32) -> EditorLayout {
    // Ширина переноса чуть меньше поля: курсору нужен пиксель-другой.
    let title_h = text_h(ui, &note.title, t::regular(12.5), w - 2.0 * TITLE_PAD.x - 2.0) + 2.0 * TITLE_PAD.y;
    let body_h = text_h(ui, &note.body, t::regular(12.0), w - 2.0 * BODY_PAD.x - 2.0) + 2.0 * BODY_PAD.y;
    let item_w = w - ITEM_TEXT_INSET - ITEM_TEXT_RIGHT - 2.0;
    EditorLayout {
        title_h: title_h.max(28.0),
        body_h: body_h.max(44.0),
        items_h: note.items.iter().map(|i| (text_h(ui, &i.text, t::regular(12.0), item_w) + 10.0).max(26.0)).collect(),
    }
}

/// Галочка задачи.
fn checkbox(ui: &Ui, rect: Rect, id: Id, checked: bool, accent: Color32) -> Response {
    let resp = ui.interact(rect.expand(3.0), id, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let p = ui.painter();
    if checked {
        p.rect_filled(rect, 5u8, accent);
        p.text(rect.center(), Align2::CENTER_CENTER, ph::CHECK, t::icon(rect.width() * 0.72, false), t::on_accent(accent));
    } else {
        let color = if resp.hovered() { accent } else { t::TEXT_TERTIARY };
        p.rect_stroke(rect, 5u8, Stroke::new(1.5, color), StrokeKind::Inside);
    }
    resp
}

/// Чип папки: подпись, цветная точка, выделение акцентом.
#[allow(clippy::too_many_arguments)]
fn folder_chip(ui: &Ui, rect: Rect, id: Id, label: &str, dot: Option<Color32>, selected: bool, accent: Color32, size: f32) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if selected {
        t::tint(accent, 0x2E)
    } else if resp.hovered() {
        t::SURFACE_3
    } else {
        t::SURFACE_2
    };
    let stroke = if selected { t::tint(accent, 0x80) } else { Color32::TRANSPARENT };
    ui.painter().rect(rect, t::RADIUS_CHIP, fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let mut x = rect.left() + 9.0;
    if let Some(c) = dot {
        ui.painter().circle_filled(pos2(x + 3.0, rect.center().y), 3.0, c);
        x += 11.0;
    }
    kit::text(ui, pos2(x, rect.center().y), Align2::LEFT_CENTER, label, t::regular(size), if selected { accent } else { t::TEXT_SECONDARY });
    resp
}

fn chip_width(ui: &Ui, label: &str, dot: bool, size: f32) -> f32 {
    kit::galley(ui, label, t::regular(size), t::TEXT_SECONDARY).size().x + 18.0 + if dot { 11.0 } else { 0.0 }
}

fn folder_color(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Высота раскрытой карточки заметки под строкой заголовка (см. draw_note_editor).
/// `folders_h` — высота рядов с папками (см. `flow`), `lay` — растущие поля.
fn note_editor_h(note: &Note, folders_h: f32, lay: &EditorLayout) -> f32 {
    4.0 + lay.title_h + 6.0 + lay.body_h + 6.0 + lay.items_h.iter().sum::<f32>() + 34.0
        + if note.reminder.is_some() { 28.0 } else { 56.0 }
        + 8.0 + folders_h + 8.0 + 26.0 + 8.0
}

/// Раскладка чипов по строкам с переносом: (x, номер строки) для каждого и число строк.
fn flow(widths: &[f32], max_w: f32, gap: f32) -> (Vec<(f32, usize)>, usize) {
    let mut out = Vec::with_capacity(widths.len());
    let (mut x, mut row) = (0.0, 0);
    for &w in widths {
        if x > 0.0 && x + w > max_w {
            row += 1;
            x = 0.0;
        }
        out.push((x, row));
        x += w + gap;
    }
    (out, row + 1)
}

/// Поле ввода названия папки прямо в строке чипов.
fn folder_edit_field(ui: &mut Ui, r: Rect, text: &mut String, done: &mut Option<bool>, accent: Color32) {
    let resp = text_field(ui, r, Id::new("folder_edit"), text, "Название папки", accent);
    if resp.lost_focus() {
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        *done = Some(!esc && !text.trim().is_empty());
    }
}

/// Сколько держать отклик голосовой команды в строке виджета.
const VOICE_FEEDBACK_HOLD: Duration = Duration::from_secs(3);

/// Группа «Голос» во вкладке «Система»: 14 + 4 + 32 × 2 + 22 + 10 + 22 + 18 × 4.
const VOICE_GROUP_H: f32 = 14.0 + 4.0 + 64.0 + 22.0 + 10.0 + 22.0 + 72.0;

/// Место под панель задач у нижнего края экрана.
const TASKBAR_ALLOWANCE: f32 = 48.0;

const MONTHS_RU: [&str; 12] = [
    "Январь", "Февраль", "Март", "Апрель", "Май", "Июнь",
    "Июль", "Август", "Сентябрь", "Октябрь", "Ноябрь", "Декабрь",
];
const MONTHS_GENITIVE: [&str; 12] = [
    "января", "февраля", "марта", "апреля", "мая", "июня",
    "июля", "августа", "сентября", "октября", "ноября", "декабря",
];
const WEEKDAYS: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];

/// Пресеты: минуты и имя создаваемого таймера.
const PRESETS: [(u64, &str); 4] = [(25, "Фокус 25"), (5, "Перерыв 5"), (15, "Перерыв 15"), (45, "Фокус 45")];

#[derive(PartialEq, Clone, Copy)]
enum Tab { Timers, Calendar, Settings, System }

impl Tab {
    fn index(self) -> usize {
        match self {
            Tab::Timers => 0,
            Tab::Calendar => 1,
            Tab::Settings => 2,
            Tab::System => 3,
        }
    }
}

/// Поле горячей клавиши: запись нового сочетания или конфликт.
enum HotkeyEdit {
    Recording { action: Action },
    /// Сочетание уже у другого действия — «Заменить» или «Отмена».
    TakenBy { action: Action, combo: Combo, other: Action },
    /// Windows не дала зарегистрировать сочетание. Видно 2,5 с, затем поле
    /// возвращается к прежнему значению.
    SystemConflict { action: Action, combo: Combo, since: Instant },
}

impl HotkeyEdit {
    fn action(&self) -> Action {
        match self {
            HotkeyEdit::Recording { action }
            | HotkeyEdit::TakenBy { action, .. }
            | HotkeyEdit::SystemConflict { action, .. } => *action,
        }
    }
}

/// Перемотка по полоске прогресса.
#[derive(Default)]
struct Seek {
    /// Бегунок тянут: позиция 0..1 под курсором.
    drag: Option<f32>,
    /// Перетаскивание отменено по Esc — отпускание игнорируем.
    cancelled: bool,
    /// Отправленная перемотка ждёт подтверждения от источника.
    pending: Option<(f32, Instant)>,
    /// До этого момента вместо исполнителя показывается время.
    label_until: Option<Instant>,
    /// Позиция для подписи времени, 0..1.
    label_fraction: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MarqueePhase { Idle, StartPause, Travel, EndPause, Return }

/// Бегущая строка названия и исполнителя в плашке.
struct Marquee {
    /// Трек, для которого уже отыграли цикл после смены.
    track: String,
    phase: MarqueePhase,
    since: Instant,
    /// Цикл запущен наведением — при уходе курсора прерывается.
    by_hover: bool,
    /// Смещения строк в момент начала возврата.
    from: (f32, f32),
}

#[derive(PartialEq, Clone, Copy)]
enum MusicMode { Hidden, Mini, Full }

#[derive(PartialEq, Clone, Copy)]
enum WidgetState { Idle, Running, Paused, Ringing, Expired }

/// Плашку тянут мышью: `y` — её верх относительно верха виджета,
/// `grab_dy` — где по высоте плашки её схватили.
struct MusicDrag {
    y: f32,
    grab_dy: f32,
}

enum RowAction {
    Pin(u64),
    Toggle(u64),
    Stop(u64),
    Remove(u64),
}

fn fmt_hms(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// «2 ч 02 мин» / «45 мин».
fn fmt_human(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 { format!("{} ч {:02} мин", h, m) } else { format!("{} мин", m) }
}

/// Время трека: «1:23», для треков от часа — «1:02:03».
fn fmt_track_time(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn shift_month(date: NaiveDate, delta: i32) -> NaiveDate {
    let mut year = date.year();
    let mut month = date.month() as i32 + delta;
    while month < 1 { month += 12; year -= 1; }
    while month > 12 { month -= 12; year += 1; }
    NaiveDate::from_ymd_opt(year, month as u32, 1).unwrap_or(date)
}

/// Сколько строк по 7 дней занимает месяц в сетке, начинающейся с понедельника.
fn calendar_rows(month: NaiveDate) -> usize {
    let offset = month.weekday().num_days_from_monday() as i64;
    let days = shift_month(month, 1).signed_duration_since(month).num_days();
    ((offset + days + 6) / 7) as usize
}

fn is_break(timer: &TimerInstance) -> bool {
    matches!(timer.mode, TimerMode::Timer(total) if total <= BREAK_MAX)
}

fn accent_of(config: &AppConfig) -> Color32 {
    Color32::from_rgb(config.primary_color[0], config.primary_color[1], config.primary_color[2])
}

/// Перетаскивание окна за блок: курсор grab, при зажатии grabbing.
fn handle_window_drag(ctx: &egui::Context, resp: &Response) {
    if resp.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if resp.is_pointer_button_down_on() || resp.dragged() {
        ctx.set_cursor_icon(CursorIcon::Grabbing);
    } else if resp.hovered() {
        ctx.set_cursor_icon(CursorIcon::Grab);
    }
}

/// Лежит ли точка рабочего стола (в физических пикселях) на каком-нибудь мониторе.
#[cfg(windows)]
fn point_on_any_monitor(x: i32, y: i32) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
    let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) };
    !monitor.is_invalid()
}

#[cfg(not(windows))]
fn point_on_any_monitor(_x: i32, _y: i32) -> bool {
    true
}

/// Положение системного курсора в физических пикселях рабочего стола.
#[cfg(windows)]
fn cursor_px() -> Option<Pos2> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some(pos2(p.x as f32, p.y as f32))
}

#[cfg(not(windows))]
fn cursor_px() -> Option<Pos2> {
    None
}

/// Убирает всё, что Windows рисует вокруг окна сама: тень, рамку цвета
/// акцента, полоску сверху и скругление углов. Для прозрачного окна с
/// «парящими» блоками это лишний прямоугольник вокруг всей стопки.
///
/// Откуда это берётся: egui-winit для окна без рамки принудительно включает
/// системную тень, а winit оставляет такому окну стиль WS_CAPTION и ради тени
/// сдвигает клиентскую область на 1 px вниз (`rgrc[0].top += 1` в WM_NCCALCSIZE).
/// Эта однопиксельная нерабочая зона и даёт DWM повод рисовать тень по контуру
/// окна, а сверху — чёрную полоску.
///
/// Решение — вообще не оставлять окну нерабочей зоны: на любой WM_NCCALCSIZE
/// отвечаем «клиентская область = всё окно». Отключать отрисовку DWM
/// (DWMWA_NCRENDERING_POLICY) нельзя: тогда Windows при активации окна рисует
/// поверх него старую рамку с заголовком и сжимает содержимое.
#[cfg(windows)]
static ORIGINAL_WNDPROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Перехватчик сообщений окна поверх обработчика winit.
#[cfg(windows)]
unsafe extern "system" fn borderless_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LPARAM, LRESULT};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, WM_KEYDOWN, WM_NCACTIVATE, WM_NCCALCSIZE, WM_SYSKEYDOWN, WNDPROC,
    };
    let original = ORIGINAL_WNDPROC.load(std::sync::atomic::Ordering::Relaxed);
    let original: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(original) };
    // Запись горячей клавиши: ловим нажатие здесь, до winit и egui.
    if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
        let down = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
        if system::capture::on_key_down(wparam.0 as u32, down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT)) {
            return LRESULT(0);
        }
    }
    match msg {
        // Клиентская область — всё окно, при любом wParam.
        WM_NCCALCSIZE => LRESULT(0),
        // lParam = -1: обработать активацию, но не перерисовывать рамку.
        WM_NCACTIVATE => unsafe { CallWindowProcW(original, hwnd, msg, wparam, LPARAM(-1)) },
        _ => unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) },
    }
}

#[cfg(windows)]
fn remove_system_frame(frame: &eframe::Frame) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE,
        DWMWCP_DONOTROUND,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWLP_WNDPROC, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    };
    let Ok(handle) = frame.window_handle() else { return };
    let RawWindowHandle::Win32(win) = handle.as_raw() else { return };
    let hwnd = HWND(win.hwnd.get() as *mut core::ffi::c_void);

    // Сначала запоминаем исходный обработчик, потом подменяем: так в нашем
    // перехватчике он уже известен к приходу первого сообщения.
    if ORIGINAL_WNDPROC.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        unsafe {
            let original = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
            if original != 0 {
                ORIGINAL_WNDPROC.store(original, std::sync::atomic::Ordering::Relaxed);
                let ours: windows::Win32::UI::WindowsAndMessaging::WNDPROC = Some(borderless_wndproc);
                SetWindowLongPtrW(hwnd, GWLP_WNDPROC, std::mem::transmute::<_, isize>(ours));
                // Заставляем Windows пересчитать рамку уже с новым обработчиком.
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    0, 0, 0, 0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }
    let no_border: u32 = DWMWA_COLOR_NONE;
    let square = DWMWCP_DONOTROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &no_border as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &square as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&square) as u32,
        );
    }
}

#[cfg(not(windows))]
fn remove_system_frame(_frame: &eframe::Frame) {}

/// Сохранённую позицию берём, только если виджет по ней реально будет виден:
/// и угол, и середина должны попасть на монитор. Иначе (монитор отключили,
/// старый конфиг с неверными координатами) — пусть Windows поставит окно сама.
fn visible_widget_pos(px: Pos2) -> Option<Pos2> {
    let (x, y) = (px.x.round() as i32, px.y.round() as i32);
    let corner = point_on_any_monitor(x + 20, y + 20);
    let middle = point_on_any_monitor(x + (t::WINDOW_W / 2.0) as i32, y + (t::WIDGET_H / 2.0) as i32);
    (corner && middle).then_some(px)
}

/// Иконка без интерактива — для кнопок, которые источник не поддерживает.
fn static_icon(ui: &Ui, rect: Rect, glyph: &str, size: f32, fill: bool, color: Color32) {
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, t::icon(size, fill), color);
}

pub struct TimerApp {
    config: AppConfig,
    state: AppState,
    selected_date: NaiveDate,
    cal_month: NaiveDate,
    tab: Tab,
    tab_changed_at: Instant,
    menu_open: bool,
    pinned_timer_id: u64,
    new_h: u64,
    new_m: u64,
    new_s: u64,
    new_name: String,
    last_autosave: Instant,

    // будильник
    alarm_stop: Arc<AtomicBool>,
    alarm_playing: Arc<AtomicBool>,
    /// Таймер, который сейчас звенит (до нажатия «Тихо»).
    ringing: Option<u64>,
    ringing_since: Instant,

    // геометрия окна
    /// Левый верхний угол виджета в физических пикселях рабочего стола.
    /// Точки egui зависят от масштаба монитора, поэтому якорь хранится в пикселях.
    anchor_px: Option<Pos2>,
    /// Меню раскрывается вверх: решается в момент открытия по месту на экране.
    menu_up: bool,
    prev_menu_open: bool,
    /// Плашку «Сейчас играет» тянут мышью: её верх относительно верха виджета
    /// и где именно её схватили.
    music_drag: Option<MusicDrag>,
    /// После отпускания плашка доезжает до своего места: откуда и когда отпустили.
    music_snap: Option<(f32, Instant)>,
    /// (высота стопки, сколько стопки над виджетом, масштаб), уже отправленные окну.
    applied: Option<(f32, f32, f32, f32, f32)>,
    /// Пока окно не дошло до запрошенной позиции (в физических пикселях),
    /// позицию из него не читаем.
    geometry_pending: Option<(Pos2, Instant)>,
    applied_on_top: Option<bool>,
    /// Сохранённая позиция виджета в физических пикселях, уже проверенная
    /// на попадание в монитор. Применяется в первом кадре.
    initial_pos_px: Option<Pos2>,
    placed: bool,
    system_frame_removed: bool,
    /// До первой расстановки окно ничего не рисует — иначе виджет на миг
    /// мелькнул бы там, куда Windows поставила окно по умолчанию.
    hold_until_placed: bool,

    // музыка
    media: MediaController,
    music_last: Option<Snapshot>,
    music_seen_at: Option<Instant>,
    music_slot_h: f32,
    music_last_h: f32,
    music_paused_by_alarm: bool,
    cover: Option<(u64, egui::TextureHandle)>,

    // анимации строк списка таймеров
    row_born: HashMap<u64, Instant>,
    row_dying: HashMap<u64, Instant>,

    accent_picker_open: bool,

    // система: горячие клавиши, трей, видимость виджета
    hotkeys: Hotkeys,
    hotkey_edit: Option<HotkeyEdit>,
    tray: Option<Tray>,
    widget_visible: bool,

    // плашка: перемотка и бегущая строка
    seek: Seek,
    marquee: Marquee,

    // голосовое управление
    voice: Option<Voice>,
    voice_status: Option<VoiceStatus>,
    /// Услышано «Джарвис» — в строке виджета «Слушаю…».
    voice_listening: bool,
    /// Отклик на команду: текст, понята ли, когда.
    voice_feedback: Option<(String, bool, Instant)>,
    /// Последняя услышанная фраза — для вкладки «Система».
    voice_last_heard: Option<String>,
    /// Микрофон, который реально открыт.
    voice_device_active: Option<String>,
    /// Микрофоны для выпадающего списка — обновляются при его открытии.
    voice_devices: Vec<String>,

    // заметки
    notes_open: bool,
    prev_notes_open: bool,
    /// Панель раскрывается вверх (снизу не хватает места).
    notes_up: bool,
    /// Слева от виджета нет места — панель справа.
    notes_right: bool,
    /// Выбранная папка; None — все заметки.
    notes_filter: Option<u64>,
    notes_search: String,
    notes_search_open: bool,
    notes_expanded: Option<u64>,
    notes_scroll_to: Option<u64>,
    notes_new: String,
    notes_done_open: bool,
    /// Создание (None) или переименование папки и вводимый текст.
    folder_edit: Option<(Option<u64>, String)>,
    reminder_input: String,
    reminder_error: Option<Instant>,
    item_input: String,
    /// Сработавшее напоминание в строке виджета.
    note_alert: Option<(u64, Instant)>,
    /// Заметка, о которой последней шла речь голосом: «отметь хлеб» ищет сначала в ней.
    voice_note_ctx: Option<(u64, Instant)>,
    /// Заметку правят в полях — сохранить чуть позже, а не на каждую букву.
    notes_dirty_at: Option<Instant>,
}

impl TimerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, config: AppConfig) -> Self {
        let mut state = storage::load_state(config.data_path.as_ref());
        for timer in &mut state.active_timers {
            timer.resume_after_load();
        }
        for note in &mut state.notes {
            note.migrate();
        }
        t::install_fonts(&cc.egui_ctx);
        t::apply_style(&cc.egui_ctx, accent_of(&config));

        let initial_id = state.active_timers.first().map(|t| t.id).unwrap_or(0);
        let today = Local::now().date_naive();
        // Старые конфиги хранили позицию в точках. Для монитора со 100% масштабом
        // это те же пиксели, поэтому пробуем их, но только после проверки на видимость.
        let saved_px = config
            .widget_pos_px
            .map(|[x, y]| pos2(x as f32, y as f32))
            .or_else(|| config.window_pos.map(|[x, y]| pos2(x, y)));
        let initial_pos_px = saved_px.and_then(visible_widget_pos);
        let long_ago = Instant::now() - Duration::from_secs(5);

        let mut hotkeys = Hotkeys::new(&cc.egui_ctx);
        hotkeys.apply(config.hotkeys_enabled, &config.hotkeys);
        let tray = if config.tray_enabled { Tray::new(&cc.egui_ctx) } else { None };
        let voice = config.voice_enabled.then(|| Voice::start(cc.egui_ctx.clone(), config.voice_device.clone()));

        Self {
            config,
            state,
            selected_date: today,
            cal_month: NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today),
            tab: Tab::Timers,
            tab_changed_at: long_ago,
            menu_open: false,
            pinned_timer_id: initial_id,
            new_h: 0,
            new_m: 25,
            new_s: 0,
            new_name: String::new(),
            last_autosave: Instant::now(),
            alarm_stop: Arc::new(AtomicBool::new(false)),
            alarm_playing: Arc::new(AtomicBool::new(false)),
            ringing: None,
            ringing_since: Instant::now(),
            anchor_px: None,
            menu_up: false,
            prev_menu_open: false,
            music_drag: None,
            music_snap: None,
            applied: None,
            geometry_pending: None,
            applied_on_top: None,
            hold_until_placed: initial_pos_px.is_some(),
            initial_pos_px,
            placed: false,
            system_frame_removed: false,
            media: MediaController::start(cc.egui_ctx.clone()),
            music_last: None,
            music_seen_at: None,
            music_slot_h: t::MUSIC_H,
            music_last_h: t::MUSIC_H,
            music_paused_by_alarm: false,
            cover: None,
            row_born: HashMap::new(),
            row_dying: HashMap::new(),
            accent_picker_open: false,
            hotkeys,
            hotkey_edit: None,
            tray,
            widget_visible: true,
            seek: Seek::default(),
            marquee: Marquee {
                track: String::new(),
                phase: MarqueePhase::Idle,
                since: Instant::now(),
                by_hover: false,
                from: (0.0, 0.0),
            },
            voice,
            voice_status: None,
            voice_listening: false,
            voice_feedback: None,
            voice_last_heard: None,
            voice_device_active: None,
            voice_devices: Vec::new(),
            notes_open: false,
            prev_notes_open: false,
            notes_up: false,
            notes_right: false,
            notes_filter: None,
            notes_search: String::new(),
            notes_search_open: false,
            notes_expanded: None,
            notes_scroll_to: None,
            notes_new: String::new(),
            notes_done_open: false,
            folder_edit: None,
            reminder_input: String::new(),
            reminder_error: None,
            item_input: String::new(),
            note_alert: None,
            voice_note_ctx: None,
            notes_dirty_at: None,
        }
    }

    // ------------------------------------------------------------ данные

    fn save_to_disk(&self) {
        storage::save_config(&self.config);
        storage::save_state(&self.state, self.config.data_path.as_ref());
    }

    /// Переносит время идущих таймеров в accumulated_time перед записью на диск.
    fn sync_all(&mut self) {
        for timer in &mut self.state.active_timers {
            timer.sync();
        }
    }

    fn sync_and_save(&mut self) {
        self.sync_all();
        self.save_to_disk();
    }

    fn timer_index(&self, id: u64) -> Option<usize> {
        self.state.active_timers.iter().position(|t| t.id == id)
    }

    /// Пишет сессию в историю (если таймер вообще шёл).
    fn record_history(&mut self, idx: usize) {
        if let Some(timer) = self.state.active_timers.get(idx) {
            let elapsed = timer.elapsed_secs();
            if elapsed > 0 {
                let entry = HistoryEntry {
                    id: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64,
                    date: Local::now(),
                    mode: format!("{}: {}",
                        timer.name,
                        if matches!(timer.mode, TimerMode::Stopwatch) { "С-метр" } else { "Таймер" }
                    ),
                    duration_secs: elapsed,
                    comment: String::new(),
                };
                self.state.history.push(entry);
            }
        }
    }

    /// Stop пишет в историю, корзина — нет. Если строка видна в меню, она
    /// сначала гаснет и схлопывается, и только потом исчезает из списка.
    fn remove_timer(&mut self, id: u64, record: bool) {
        let Some(idx) = self.timer_index(id) else { return };
        if self.row_dying.contains_key(&id) {
            return;
        }
        if record {
            self.record_history(idx);
        }
        if self.ringing == Some(id) {
            self.silence(false);
        }
        let visible = self.menu_open && self.tab == Tab::Timers;
        if visible {
            let timer = &mut self.state.active_timers[idx];
            if timer.is_running() {
                timer.toggle();
            }
            self.row_dying.insert(id, Instant::now());
        } else {
            self.state.active_timers.remove(idx);
        }
        self.save_to_disk();
    }

    /// Убирает из списка строки, чья анимация удаления закончилась.
    fn finish_dying_rows(&mut self) {
        let total = t::ANIM_ROW_FADE + t::ANIM_ROW_COLLAPSE;
        let done: Vec<u64> = self
            .row_dying
            .iter()
            .filter(|(_, at)| at.elapsed().as_secs_f32() >= total)
            .map(|(id, _)| *id)
            .collect();
        if done.is_empty() {
            return;
        }
        for id in done {
            self.row_dying.remove(&id);
            self.state.active_timers.retain(|t| t.id != id);
        }
        self.save_to_disk();
    }

    fn add_timer(&mut self, name: String, mode: TimerMode, start: bool) {
        let mut timer = TimerInstance::new(name, mode);
        if start {
            timer.toggle();
        }
        let id = timer.id;
        let breaks = is_break(&timer);
        self.state.active_timers.insert(0, timer);
        self.pinned_timer_id = id;
        self.row_born.insert(id, Instant::now());
        if start && !breaks {
            self.resume_music_for_focus();
        }
        self.save_to_disk();
    }

    /// Play/pause. Истёкший таймер по play запускается заново с полного времени —
    /// иначе он тут же снова «истекал» бы и звонил.
    fn toggle_timer(&mut self, id: u64) {
        let Some(idx) = self.timer_index(id) else { return };
        let timer = &mut self.state.active_timers[idx];
        if timer.is_finished() && !timer.is_running() {
            timer.accumulated_time = Duration::ZERO;
        }
        timer.toggle();
        let started = timer.is_running();
        let breaks = is_break(timer);
        if self.ringing == Some(id) {
            self.silence(false);
        }
        if started && !breaks {
            self.resume_music_for_focus();
        }
        self.save_to_disk();
    }

    // ------------------------------------------------------------ звук

    fn stop_alarm(&self) {
        self.alarm_stop.store(true, Ordering::Relaxed);
    }

    fn alarm_is_playing(&self) -> bool {
        self.alarm_playing.load(Ordering::Relaxed)
    }

    /// `looping` — будильник по кругу до «Тихо»; без него — один проигрыш (тест).
    fn play_alarm(&mut self, looping: bool) {
        // Глушим предыдущий сигнал, иначе два будильника наложатся друг на друга.
        self.stop_alarm();

        let stop = Arc::new(AtomicBool::new(false));
        let playing = Arc::new(AtomicBool::new(true));
        self.alarm_stop = stop.clone();
        self.alarm_playing = playing.clone();

        let volume = self.config.alarm_volume;
        let custom = self.config.alarm_sound.clone()
            .filter(|p| !p.trim().is_empty() && Path::new(p).exists());

        std::thread::spawn(move || {
            if let Ok((_stream, stream_handle)) = OutputStream::try_default() {
                if let Ok(sink) = Sink::try_new(&stream_handle) {
                    sink.set_volume(volume.clamp(0.0, 1.0));

                    let mut queued = false;
                    if let Some(path) = custom {
                        if let Ok(file) = File::open(path) {
                            if let Ok(source) = Decoder::new(BufReader::new(file)) {
                                if looping { sink.append(source.repeat_infinite()); } else { sink.append(source); }
                                queued = true;
                            }
                        }
                    }
                    if !queued {
                        if let Ok(source) = Decoder::new(Cursor::new(DEFAULT_ALARM)) {
                            if looping { sink.append(source.repeat_infinite()); } else { sink.append(source); }
                        }
                    }

                    // Ждём конца воспроизведения, но остаёмся отзывчивыми к кнопке "заглушить".
                    let started = Instant::now();
                    while !sink.empty()
                        && !stop.load(Ordering::Relaxed)
                        && started.elapsed() < ALARM_LOOP_LIMIT
                    {
                        std::thread::sleep(Duration::from_millis(80));
                    }
                    sink.stop();
                }
            }
            playing.store(false, Ordering::Relaxed);
        });
    }

    /// Таймер дошёл до нуля: звонок, пульс обводки, пауза музыки.
    fn start_ringing(&mut self, ctx: &egui::Context, id: u64) {
        self.ringing = Some(id);
        self.ringing_since = Instant::now();
        // Мелодия звучит один раз; обводка пульсирует, пока не нажато «Тихо».
        self.play_alarm(false);
        // Будильник при скрытом виджете показывает его снова.
        if !self.widget_visible {
            self.set_widget_visible(ctx, true);
        }
        if self.config.notify_enabled {
            if let Some(tm) = self.timer_index(id).map(|i| &self.state.active_timers[i]) {
                let body = match tm.mode {
                    TimerMode::Timer(total) => format!("{} · {}", tm.name, fmt_human(total.as_secs())),
                    TimerMode::Stopwatch => tm.name.clone(),
                };
                system::notify("Время вышло", &body);
            }
        }
        // Мигаем в панели задач: виджет может быть перекрыт другим окном.
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
        let music_playing = self.music_last.as_ref().map(|s| s.playing).unwrap_or(false);
        if self.config.music_enabled && self.config.music_pause_on_alarm && music_playing {
            self.media.send(MediaCmd::Pause);
            self.music_paused_by_alarm = true;
        }
    }

    /// «Тихо». `resume` — продолжить музыку, если её ставил на паузу будильник.
    fn silence(&mut self, resume: bool) {
        self.stop_alarm();
        self.ringing = None;
        if resume {
            self.resume_music_for_focus();
        }
    }

    fn resume_music_for_focus(&mut self) {
        if self.music_paused_by_alarm && self.config.music_resume_on_focus {
            self.media.send(MediaCmd::Play);
            self.music_paused_by_alarm = false;
        }
    }

    fn tick_timers(&mut self, ctx: &egui::Context) {
        let mut finished = None;
        for timer in &mut self.state.active_timers {
            if timer.is_running() && timer.time_left().as_secs() == 0 {
                if let TimerMode::Timer(_) = timer.mode {
                    timer.toggle();
                    finished = Some(timer.id);
                }
            }
        }
        if let Some(id) = finished {
            self.start_ringing(ctx, id);
            self.save_to_disk();
        }
    }

    // ------------------------------------------------------------ система

    fn set_widget_visible(&mut self, ctx: &egui::Context, visible: bool) {
        self.widget_visible = visible;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
        if visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    /// Таймер, который показывает виджет: закреплённый или первый в списке.
    fn shown_timer_id(&self) -> Option<u64> {
        self.timer_index(self.pinned_timer_id)
            .or_else(|| (!self.state.active_timers.is_empty()).then_some(0))
            .map(|i| self.state.active_timers[i].id)
    }

    /// Перерегистрирует горячие клавиши по текущим настройкам.
    /// Возвращает действия, сочетания которых заняты Windows или другой программой.
    fn apply_hotkeys(&mut self) -> Vec<Action> {
        self.hotkeys.apply(self.config.hotkeys_enabled, &self.config.hotkeys)
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::StartPause => {
                if let Some(id) = self.ringing {
                    self.toggle_timer(id);
                } else if let Some(id) = self.shown_timer_id() {
                    self.toggle_timer(id);
                }
            }
            Action::StopRecord => {
                if let Some(id) = self.ringing.or_else(|| self.shown_timer_id()) {
                    self.remove_timer(id, true);
                }
            }
            Action::Silence => {
                self.note_alert = None;
                if self.ringing.is_some() {
                    self.silence(true);
                } else {
                    self.stop_alarm();
                }
            }
            Action::ToggleWidget => self.set_widget_visible(ctx, !self.widget_visible),
            Action::ToggleMenu => {
                if !self.widget_visible {
                    self.set_widget_visible(ctx, true);
                    self.menu_open = true;
                } else {
                    self.menu_open = !self.menu_open;
                }
                self.accent_picker_open = false;
            }
            Action::MusicPlayPause => {
                self.music_paused_by_alarm = false;
                self.media.send(MediaCmd::TogglePlayPause);
            }
            Action::MusicNext => self.media.send(MediaCmd::Next),
            Action::MusicPrev => self.media.send(MediaCmd::Previous),
            Action::ToggleNotes => {
                if !self.widget_visible {
                    self.set_widget_visible(ctx, true);
                    self.notes_open = true;
                } else {
                    self.toggle_notes();
                }
            }
        }
    }

    // ------------------------------------------------------------ заметки: данные

    fn toggle_notes(&mut self) {
        self.notes_open = !self.notes_open;
        if !self.notes_open {
            self.folder_edit = None;
        }
    }

    /// Открыть панель на конкретной заметке (клик по напоминанию в виджете).
    fn open_note(&mut self, id: u64) {
        self.notes_open = true;
        self.notes_filter = None;
        self.notes_search.clear();
        self.notes_expanded = Some(id);
        self.notes_scroll_to = Some(id);
        self.note_alert = None;
        if self.state.notes.iter().any(|n| n.id == id && n.done) {
            self.notes_done_open = true;
        }
        self.reminder_input.clear();
        self.item_input.clear();
    }

    fn note_index(&self, id: u64) -> Option<usize> {
        self.state.notes.iter().position(|n| n.id == id)
    }

    /// Текущая заметка для голосовых команд без названия: раскрытая в панели
    /// или та, о которой говорили в последние минуты.
    fn context_note(&self) -> Option<usize> {
        let open = self.notes_open.then_some(self.notes_expanded).flatten();
        let spoken = self.voice_note_ctx.filter(|(_, at)| at.elapsed() < VOICE_NOTE_CONTEXT).map(|(id, _)| id);
        open.or(spoken).and_then(|id| self.note_index(id))
    }

    fn set_voice_note(&mut self, id: u64) {
        self.voice_note_ctx = Some((id, Instant::now()));
    }

    /// Заметка по названию или текущая. Для команд «добавь пункт …» и т. п.
    fn note_or_context(&self, note: &Option<String>) -> Result<usize, String> {
        match note {
            Some(q) => notes::find_note(&self.state.notes, q)
                .and_then(|id| self.note_index(id))
                .ok_or_else(|| format!("Не нашёл заметку «{q}»")),
            None => self
                .context_note()
                .ok_or_else(|| "В какую заметку? Скажи «в заметку …» или открой её".to_string()),
        }
    }

    /// Пункт: в названной заметке; иначе — в текущей, потом во всех.
    /// Если пункт с таким названием нашёлся в нескольких заметках — просим уточнить.
    fn locate_item(&self, note: &Option<String>, item: &str, want_done: bool) -> Result<(usize, usize), String> {
        if note.is_some() {
            let i = self.note_or_context(note)?;
            let n = &self.state.notes[i];
            return notes::find_item(n, item, want_done)
                .map(|k| (i, k))
                .ok_or_else(|| format!("В «{}» нет пункта «{item}»", n.title));
        }
        if let Some(i) = self.context_note() {
            if let Some(k) = notes::find_item(&self.state.notes[i], item, want_done) {
                return Ok((i, k));
            }
        }
        let hits = notes::find_item_everywhere(&self.state.notes, item, want_done);
        match hits.as_slice() {
            [] => Err(format!("Не нашёл «{item}»")),
            [one] => Ok(*one),
            many => {
                let titles: Vec<String> = many.iter().map(|&(i, _)| self.state.notes[i].title.clone()).collect();
                Err(format!("«{item}» есть в: {} — скажи «в заметке …»", titles.join(", ")))
            }
        }
    }

    fn mark_notes_dirty(&mut self) {
        self.notes_dirty_at = Some(Instant::now());
    }

    fn add_note(&mut self, title: String, folders: Vec<u64>, reminder: Option<(NaiveDateTime, Repeat)>) -> u64 {
        let mut note = Note::new(title, folders);
        note.reminder = reminder.map(|(at, r)| Reminder::new(at, r));
        let id = note.id;
        self.state.notes.insert(0, note);
        self.save_to_disk();
        id
    }

    fn delete_note(&mut self, id: u64) {
        self.state.notes.retain(|n| n.id != id);
        if self.notes_expanded == Some(id) {
            self.notes_expanded = None;
        }
        if self.note_alert.is_some_and(|(a, _)| a == id) {
            self.note_alert = None;
        }
        self.save_to_disk();
    }

    /// Существующая папка по названию с учётом окончаний: «работе» → «Работа».
    fn find_folder(&self, name: &str) -> Option<u64> {
        let q = name.to_lowercase();
        self.state.folders.iter().find(|f| notes::word_match(&f.name.to_lowercase(), &q)).map(|f| f.id)
    }

    /// Папка по названию; нет — создаётся.
    fn folder_by_name(&mut self, name: &str) -> u64 {
        match self.find_folder(name) {
            Some(id) => id,
            None => self.create_folder(name),
        }
    }

    fn folder_name(&self, id: u64) -> String {
        self.state.folders.iter().find(|f| f.id == id).map(|f| f.name.clone()).unwrap_or_default()
    }

    fn folder_list(&self, ids: &[u64]) -> String {
        ids.iter().map(|&id| self.folder_name(id)).collect::<Vec<_>>().join(", ")
    }

    /// Удаляет папку. Заметки остаются — просто без этой папки.
    fn remove_folder(&mut self, id: u64) {
        self.state.folders.retain(|f| f.id != id);
        for n in &mut self.state.notes {
            n.folders.retain(|f| *f != id);
        }
        if self.notes_filter == Some(id) {
            self.notes_filter = None;
        }
        self.save_to_disk();
    }

    fn create_folder(&mut self, name: &str) -> u64 {
        let color = notes::FOLDER_COLORS[self.state.folders.len() % notes::FOLDER_COLORS.len()];
        let folder = Folder { id: notes::new_id(), name: notes::capitalize(name.trim()), color };
        let id = folder.id;
        self.state.folders.push(folder);
        self.save_to_disk();
        id
    }

    /// Срабатывание напоминаний: уведомление Windows и строка в виджете.
    fn tick_reminders(&mut self, ctx: &egui::Context) {
        let now = notes::now();
        let mut fired: Vec<(u64, String)> = Vec::new();
        for note in &mut self.state.notes {
            if note.done {
                continue;
            }
            if let Some(r) = &mut note.reminder {
                if r.due(now) {
                    r.advance(now);
                    fired.push((note.id, note.title.clone()));
                }
            }
        }
        let Some((last_id, _)) = fired.last().cloned() else { return };
        if self.config.notify_enabled {
            for (_, title) in &fired {
                system::notify("Напоминание", title);
            }
        }
        self.note_alert = Some((last_id, Instant::now()));
        if !self.widget_visible {
            self.set_widget_visible(ctx, true);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
        self.save_to_disk();
    }

    /// Включает или выключает распознавание по настройке и разбирает его события.
    fn sync_voice(&mut self, ctx: &egui::Context) {
        if self.config.voice_enabled && self.voice.is_none() {
            self.voice_status = None;
            self.voice = Some(Voice::start(ctx.clone(), self.config.voice_device.clone()));
        } else if !self.config.voice_enabled && self.voice.is_some() {
            self.voice = None;
            self.voice_status = None;
            self.voice_listening = false;
        }
        let events = self.voice.as_ref().map(|v| v.poll()).unwrap_or_default();
        for event in events {
            match event {
                VoiceEvent::Status(status) => {
                    if matches!(status, VoiceStatus::Error(_)) {
                        // Поток распознавания завершился — при повторном включении стартует заново.
                        self.voice = None;
                        self.config.voice_enabled = false;
                        self.voice_listening = false;
                        self.save_to_disk();
                    }
                    self.voice_status = Some(status);
                }
                VoiceEvent::Listening(on) => self.voice_listening = on,
                VoiceEvent::Text(text) => self.voice_last_heard = Some(text),
                VoiceEvent::Device(name) => self.voice_device_active = Some(name),
                VoiceEvent::Heard { text, command } => {
                    match command {
                        Some(cmd) => {
                            let feedback = cmd.feedback();
                            self.voice_feedback = Some(match self.handle_voice(ctx, cmd) {
                                Ok(custom) => (custom.unwrap_or(feedback), true, Instant::now()),
                                Err(e) => (e, false, Instant::now()),
                            });
                        }
                        None => {
                            self.voice_feedback = Some((format!("Не понял: «{text}»"), false, Instant::now()));
                        }
                    }
                }
            }
        }
        if let Some((_, _, at)) = &self.voice_feedback {
            let left = VOICE_FEEDBACK_HOLD.saturating_sub(at.elapsed());
            if left.is_zero() {
                self.voice_feedback = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
    }

    /// Выполняет голосовую команду. `Ok(Some)` — свой текст отклика, `Err` — не вышло.
    fn handle_voice(&mut self, ctx: &egui::Context, cmd: VoiceCommand) -> Result<Option<String>, String> {
        match cmd {
            VoiceCommand::MusicPause => {
                // «Стоп» во время звонка — это «Тихо».
                if self.ringing.is_some() {
                    self.handle_action(ctx, Action::Silence);
                } else {
                    self.music_paused_by_alarm = false;
                    self.media.send(MediaCmd::Pause);
                }
            }
            VoiceCommand::MusicPlay => {
                self.music_paused_by_alarm = false;
                self.media.send(MediaCmd::Play);
            }
            VoiceCommand::MusicNext => self.handle_action(ctx, Action::MusicNext),
            VoiceCommand::MusicPrev => self.handle_action(ctx, Action::MusicPrev),
            VoiceCommand::AddTimer { secs, name } => {
                let name = name.unwrap_or_else(|| "Таймер".into());
                self.add_timer(name, TimerMode::Timer(Duration::from_secs(secs.max(1))), true);
            }
            VoiceCommand::AddStopwatch { name } => {
                let name = name.unwrap_or_else(|| "Секундомер".into());
                self.add_timer(name, TimerMode::Stopwatch, true);
            }
            VoiceCommand::PauseTimer => {
                if let Some(id) = self.shown_timer_id() {
                    if self.timer_index(id).is_some_and(|i| self.state.active_timers[i].is_running()) {
                        self.toggle_timer(id);
                    }
                }
            }
            VoiceCommand::ResumeTimer => {
                if let Some(id) = self.shown_timer_id() {
                    if self.timer_index(id).is_some_and(|i| !self.state.active_timers[i].is_running()) {
                        self.toggle_timer(id);
                    }
                }
            }
            VoiceCommand::StopTimer => self.handle_action(ctx, Action::StopRecord),
            VoiceCommand::Silence => self.handle_action(ctx, Action::Silence),
            VoiceCommand::ShowWidget => {
                if !self.widget_visible {
                    self.set_widget_visible(ctx, true);
                }
            }
            VoiceCommand::HideWidget => {
                if self.widget_visible {
                    self.set_widget_visible(ctx, false);
                }
            }
            VoiceCommand::OpenMenu => {
                if !self.widget_visible {
                    self.set_widget_visible(ctx, true);
                }
                self.menu_open = true;
                self.accent_picker_open = false;
            }
            VoiceCommand::CloseMenu => {
                self.menu_open = false;
                self.accent_picker_open = false;
            }
            VoiceCommand::AddNote { title, folders, reminder } => {
                let ids: Vec<u64> = folders.iter().map(|f| self.folder_by_name(f)).collect();
                // «Добавь заметку молоко в папку дом» про уже существующую заметку —
                // это назначение папки, а не дубликат.
                if !ids.is_empty() && reminder.is_none() {
                    let lower = title.to_lowercase();
                    if let Some(i) = self.state.notes.iter().position(|n| !n.done && n.title.to_lowercase() == lower) {
                        for id in &ids {
                            if !self.state.notes[i].in_folder(*id) {
                                self.state.notes[i].folders.push(*id);
                            }
                        }
                        let id = self.state.notes[i].id;
                        self.set_voice_note(id);
                        self.save_to_disk();
                        return Ok(Some(format!("«{title}» → {}", self.folder_list(&ids))));
                    }
                }
                let id = self.add_note(title, ids, reminder);
                self.set_voice_note(id);
            }
            VoiceCommand::CreateFolder { name } => {
                if let Some(id) = self.find_folder(&name) {
                    return Ok(Some(format!("Папка «{}» уже есть", self.folder_name(id))));
                }
                self.create_folder(&name);
            }
            VoiceCommand::DeleteFolder { name } => {
                let id = self.find_folder(&name).ok_or_else(|| format!("Нет папки «{name}»"))?;
                let title = self.folder_name(id);
                self.remove_folder(id);
                return Ok(Some(format!("Папка «{title}» удалена, заметки остались")));
            }
            VoiceCommand::RenameFolder { from, to } => {
                let id = self.find_folder(&from).ok_or_else(|| format!("Нет папки «{from}»"))?;
                if let Some(f) = self.state.folders.iter_mut().find(|f| f.id == id) {
                    f.name = to;
                }
                self.save_to_disk();
            }
            VoiceCommand::ShowFolder { name } => {
                let filter = match name {
                    Some(n) => Some(self.find_folder(&n).ok_or_else(|| format!("Нет папки «{n}»"))?),
                    None => None,
                };
                if !self.widget_visible {
                    self.set_widget_visible(ctx, true);
                }
                self.notes_open = true;
                self.notes_filter = filter;
                self.notes_expanded = None;
            }
            VoiceCommand::NoteToFolder { query, folders, replace } => {
                let id = notes::find_note(&self.state.notes, &query).ok_or_else(|| format!("Не нашёл заметку «{query}»"))?;
                let ids: Vec<u64> = folders.iter().map(|f| self.folder_by_name(f)).collect();
                let i = self.note_index(id).ok_or("Заметка пропала")?;
                let note = &mut self.state.notes[i];
                if replace {
                    note.folders = ids.clone();
                } else {
                    for fid in &ids {
                        if !note.in_folder(*fid) {
                            note.folders.push(*fid);
                        }
                    }
                }
                let title = note.title.clone();
                self.save_to_disk();
                return Ok(Some(format!("«{title}» → {}", self.folder_list(&ids))));
            }
            VoiceCommand::NoteFromFolder { query, folder } => {
                let id = notes::find_note(&self.state.notes, &query).ok_or_else(|| format!("Не нашёл заметку «{query}»"))?;
                let fid = self.find_folder(&folder).ok_or_else(|| format!("Нет папки «{folder}»"))?;
                let i = self.note_index(id).ok_or("Заметка пропала")?;
                self.state.notes[i].folders.retain(|f| *f != fid);
                let title = self.state.notes[i].title.clone();
                self.save_to_disk();
                return Ok(Some(format!("«{title}» убрана из «{}»", self.folder_name(fid))));
            }
            VoiceCommand::Check { note, target, item_only, done } => {
                let verb = if done { "Выполнено" } else { "Снята отметка" };
                // Сама заметка: «отметь заметку молоко», или без явной заметки — по названию.
                let whole_note = match (&note, &target) {
                    (Some(q), None) => Some(
                        notes::find_note(&self.state.notes, q)
                            .and_then(|id| self.note_index(id))
                            .ok_or_else(|| format!("Не нашёл заметку «{q}»"))?,
                    ),
                    (None, Some(t)) if !item_only => {
                        // Пункт текущей заметки важнее похожего названия другой заметки.
                        let in_context = self.context_note().and_then(|i| notes::find_item(&self.state.notes[i], t, done));
                        if in_context.is_none() {
                            notes::find_note(&self.state.notes, t).and_then(|id| self.note_index(id))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(i) = whole_note {
                    let id = self.state.notes[i].id;
                    self.state.notes[i].set_done(done);
                    let title = self.state.notes[i].title.clone();
                    if done && self.note_alert.is_some_and(|(a, _)| a == id) {
                        self.note_alert = None;
                    }
                    self.set_voice_note(id);
                    self.save_to_disk();
                    return Ok(Some(format!("{verb}: «{title}»")));
                }
                let item = target.ok_or("Что отметить?")?;
                let (i, k) = self.locate_item(&note, &item, done)?;
                self.state.notes[i].items[k].done = done;
                let id = self.state.notes[i].id;
                let (title, text) = (self.state.notes[i].title.clone(), self.state.notes[i].items[k].text.clone());
                self.set_voice_note(id);
                self.save_to_disk();
                return Ok(Some(format!("{verb}: «{text}» в «{title}»")));
            }
            VoiceCommand::DeleteItem { note, item } => {
                let (i, k) = self.locate_item(&note, &item, true)?;
                let removed = self.state.notes[i].items.remove(k);
                let id = self.state.notes[i].id;
                let title = self.state.notes[i].title.clone();
                self.set_voice_note(id);
                self.save_to_disk();
                return Ok(Some(format!("Удалён пункт «{}» из «{title}»", removed.text)));
            }
            VoiceCommand::OpenNote { query } => {
                let id = notes::find_note(&self.state.notes, &query).ok_or_else(|| format!("Не нашёл заметку «{query}»"))?;
                if !self.widget_visible {
                    self.set_widget_visible(ctx, true);
                }
                self.open_note(id);
                self.set_voice_note(id);
                let title = self.note_index(id).map(|i| self.state.notes[i].title.clone()).unwrap_or_default();
                return Ok(Some(format!("Открыта «{title}»")));
            }
            VoiceCommand::DeleteNote { query } => {
                let id = notes::find_note(&self.state.notes, &query).ok_or_else(|| format!("Не нашёл заметку «{query}»"))?;
                let title = self.note_index(id).map(|i| self.state.notes[i].title.clone()).unwrap_or_default();
                self.delete_note(id);
                return Ok(Some(format!("Удалена заметка «{title}»")));
            }
            VoiceCommand::AddNoteItem { note, item } => {
                let i = self.note_or_context(&note)?;
                self.state.notes[i].items.push(NoteItem { text: item.clone(), done: false });
                let id = self.state.notes[i].id;
                let title = self.state.notes[i].title.clone();
                self.set_voice_note(id);
                self.save_to_disk();
                return Ok(Some(format!("«{item}» → «{title}»")));
            }
            VoiceCommand::ShowNotes => {
                if !self.widget_visible {
                    self.set_widget_visible(ctx, true);
                }
                self.notes_open = true;
            }
            VoiceCommand::HideNotes => {
                self.notes_open = false;
                self.folder_edit = None;
            }
            VoiceCommand::DeleteTimer { query } => {
                let id = match &query {
                    Some(q) => notes::best_match(q, self.state.active_timers.iter().map(|tm| (tm.id, tm.name.as_str())))
                        .ok_or_else(|| format!("Не нашёл таймер «{q}»"))?,
                    None => self.shown_timer_id().ok_or("Нет таймера, который можно удалить")?,
                };
                let name = self.timer_index(id).map(|i| self.state.active_timers[i].name.clone()).unwrap_or_default();
                if self.ringing == Some(id) {
                    self.silence(false);
                }
                self.remove_timer(id, false);
                return Ok(Some(format!("Удалён «{name}»")));
            }
        }
        Ok(None)
    }

    fn handle_tray(&mut self, ctx: &egui::Context, cmd: TrayCommand) {
        match cmd {
            TrayCommand::ToggleWidget => self.handle_action(ctx, Action::ToggleWidget),
            TrayCommand::StartPause => self.handle_action(ctx, Action::StartPause),
            TrayCommand::StopRecord => self.handle_action(ctx, Action::StopRecord),
            TrayCommand::Silence => self.handle_action(ctx, Action::Silence),
            TrayCommand::ToggleOnTop => {
                self.config.always_on_top = !self.config.always_on_top;
                self.save_to_disk();
            }
            TrayCommand::Exit => {
                self.sync_and_save();
                if !self.widget_visible {
                    self.set_widget_visible(ctx, true);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Создаёт или убирает значок по настройке и обновляет его состояние.
    fn sync_tray(&mut self, ctx: &egui::Context) {
        if self.config.tray_enabled && self.tray.is_none() {
            self.tray = Tray::new(ctx);
        } else if !self.config.tray_enabled && self.tray.is_some() {
            self.tray = None;
            // Без значка до скрытого виджета было бы не добраться.
            if !self.widget_visible {
                self.set_widget_visible(ctx, true);
            }
        }
        let shown = self.ringing.or_else(|| self.shown_timer_id()).and_then(|id| self.timer_index(id));
        let timer = shown.map(|i| &self.state.active_timers[i]);
        let running = timer.is_some_and(|tm| tm.is_running());
        let tooltip = match timer {
            Some(tm) => format!("Timer Pro — {} {}", tm.name, fmt_hms(tm.time_left().as_secs())),
            None => "Timer Pro".to_string(),
        };
        let look = if self.ringing.is_some() {
            TrayLook::Ringing
        } else if running {
            TrayLook::Running
        } else {
            TrayLook::Idle
        };
        let state = TrayState {
            widget_visible: self.widget_visible,
            timer_name: timer.map(|tm| tm.name.clone()),
            running,
            ringing: self.ringing.is_some(),
            on_top: self.config.always_on_top,
            tooltip,
            look,
            accent: accent_of(&self.config),
        };
        if let Some(tray) = &mut self.tray {
            tray.update(state);
        }
    }

    // ------------------------------------------------------------ музыка

    fn music_mode(&mut self) -> MusicMode {
        if !self.config.music_enabled {
            return MusicMode::Hidden;
        }
        let snap = self.media.snapshot();
        if let Some(s) = &snap {
            self.music_seen_at = Some(Instant::now());
            // Пользователь сам включил музыку — авто-возобновление больше не нужно.
            if s.playing && self.ringing.is_none() {
                self.music_paused_by_alarm = false;
            }
        }
        let has = snap.is_some();
        if has {
            self.music_last = snap;
        }
        if has {
            MusicMode::Full
        } else if !self.config.music_hide_idle {
            MusicMode::Mini
        } else if self.music_seen_at.is_some_and(|at| at.elapsed() < MUSIC_IDLE_GRACE) {
            MusicMode::Mini
        } else {
            MusicMode::Hidden
        }
    }

    fn update_cover_texture(&mut self, ctx: &egui::Context) {
        let Some(cover) = self.music_last.as_ref().and_then(|s| s.cover.clone()) else {
            self.cover = None;
            return;
        };
        if self.cover.as_ref().map(|(k, _)| *k) == Some(cover.key) {
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(cover.size, &cover.rgba);
        let tex = ctx.load_texture("music-cover", image, egui::TextureOptions::LINEAR);
        self.cover = Some((cover.key, tex));
    }

    // ------------------------------------------------------------ геометрия окна

    /// Куда раскрывать меню: вверх, если снизу не хватает места под виджет,
    /// плашку (если она снизу) и меню, а сверху места больше.
    fn choose_menu_up(&self, ctx: &egui::Context, need_below: f32, need_above: f32) -> bool {
        let (Some(anchor_px), Some(monitor)) = (self.anchor_px, ctx.input(|i| i.viewport().monitor_size)) else {
            return false;
        };
        // Мониторы стоят в ряд с верхом на y = 0, поэтому вертикаль в точках
        // текущего монитора — это пиксели, делённые на его масштаб.
        let anchor_y = anchor_px.y / ctx.pixels_per_point();
        let room_below = monitor.y - TASKBAR_ALLOWANCE - anchor_y;
        let room_above = anchor_y;
        // Вниз не помещается — уходим вверх, если там помещается или хотя бы просторнее.
        room_below < need_below && (room_above >= need_above || room_above > room_below)
    }

    /// Держит окно по размеру стопки, а виджет — на месте.
    ///
    /// Логические точки egui привязаны к масштабу текущего монитора, поэтому
    /// между мониторами с разным масштабом они несопоставимы. Всё, что
    /// сохраняется или сравнивается между кадрами, хранится в физических
    /// пикселях; в точки переводим масштабом текущего кадра — тем же, которым
    /// egui переводит OuterPosition обратно в пиксели.
    fn sync_geometry(&mut self, ctx: &egui::Context, stack_h: f32, above: f32, left: f32, right: f32) {
        let ppp = ctx.pixels_per_point();
        let outer = ctx.input(|i| i.viewport().outer_rect);

        if !self.placed {
            self.placed = true;
            if let Some(px) = self.initial_pos_px {
                self.anchor_px = Some(px);
                self.applied = None;
            }
        }

        // Окно дошло до запрошенной позиции — снова можно читать её из окна.
        if let Some((expected_px, since)) = self.geometry_pending {
            let arrived = outer.is_some_and(|r| (r.min.to_vec2() * ppp - expected_px.to_vec2()).length() < 4.0);
            if arrived || since.elapsed() > Duration::from_millis(600) {
                self.geometry_pending = None;
            }
        }
        if self.geometry_pending.is_none() {
            let pads = |above: f32, left: f32| vec2(t::PAD_X + left, t::PAD_TOP + above) * ppp;
            if let (Some(r), Some((_, above, _, left, _))) = (outer, self.applied) {
                let observed = (r.min.to_vec2() * ppp + pads(above, left)).to_pos2();
                // Расхождение в пару пикселей — это погрешность Windows при установке
                // позиции (особенно при переезде между мониторами с разным масштабом),
                // а не перетаскивание. Если принимать его, виджет уползал бы на 1–2 px
                // при каждом запуске и каждом открытии меню.
                let jitter = self.anchor_px.is_some_and(|a| (observed - a).length() < 4.0);
                if !jitter {
                    self.anchor_px = Some(observed);
                }
            } else if self.anchor_px.is_none() {
                self.anchor_px = outer.map(|r| (r.min.to_vec2() * ppp + pads(0.0, 0.0)).to_pos2());
            }
        }
        if let Some(a) = self.anchor_px {
            self.config.window_pos = Some([a.x / ppp, a.y / ppp]);
            self.config.widget_pos_px = Some([a.x.round() as i32, a.y.round() as i32]);
        }

        // Переезд на монитор с другим масштабом тоже требует заново выставить размер.
        let changed = self.applied.is_none_or(|(h, a, p, l, r)| {
            (h - stack_h).abs() > 0.5
                || (a - above).abs() > 0.5
                || (p - ppp).abs() > 0.001
                || (l - left).abs() > 0.5
                || (r - right).abs() > 0.5
        });
        if changed {
            if let Some(anchor_px) = self.anchor_px {
                let window_min = pos2(anchor_px.x / ppp - t::PAD_X - left, anchor_px.y / ppp - t::PAD_TOP - above);
                let window_h = t::PAD_TOP + stack_h + t::PAD_BOTTOM;
                let window_w = t::WINDOW_W + 2.0 * t::PAD_X + left + right;
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(window_min));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(window_w, window_h)));
                self.geometry_pending = Some((pos2(window_min.x * ppp, window_min.y * ppp), Instant::now()));
                self.applied = Some((stack_h, above, ppp, left, right));
            }
        }
    }

    // ------------------------------------------------------------ виджет

    fn widget_state(&self) -> (WidgetState, Option<usize>) {
        if let Some(i) = self.ringing.and_then(|id| self.timer_index(id)) {
            return (WidgetState::Ringing, Some(i));
        }
        let pinned = self.timer_index(self.pinned_timer_id)
            .or_else(|| if self.state.active_timers.is_empty() { None } else { Some(0) });
        match pinned {
            None => (WidgetState::Idle, None),
            Some(i) => {
                let tm = &self.state.active_timers[i];
                let st = if tm.is_finished() {
                    WidgetState::Expired
                } else if tm.is_running() {
                    WidgetState::Running
                } else {
                    WidgetState::Paused
                };
                (st, Some(i))
            }
        }
    }

    fn draw_widget(&mut self, ui: &mut Ui, rect: Rect, accent: Color32) {
        let ctx = ui.ctx().clone();
        let (state, idx) = self.widget_state();
        let timer = idx.map(|i| self.state.active_timers[i].clone());
        let ringing = state == WidgetState::Ringing;
        let alarmed = ringing || state == WidgetState::Expired;

        let stroke = if ringing {
            let phase = self.ringing_since.elapsed().as_secs_f32() % 1.0;
            let k = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
            t::danger_stroke((0x66 as f32 + (0xCC - 0x66) as f32 * k) as u8)
        } else if self.voice_listening {
            // Джарвис слушает: мягкая пульсация акцентом, виджет не растёт.
            let phase = (ctx.input(|i| i.time) % 1.4 / 1.4) as f32;
            let k = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
            t::tint(accent, (0x40 as f32 + 0x90 as f32 * k) as u8)
        } else if let Some((_, since)) = self.note_alert {
            // Напоминание: первые секунды обводка пульсирует акцентом.
            if since.elapsed() < NOTE_ALERT_PULSE {
                let phase = since.elapsed().as_secs_f32() % 1.2 / 1.2;
                let k = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
                t::tint(accent, (0x50 as f32 + 0x80 as f32 * k) as u8)
            } else {
                t::tint(accent, 0x60)
            }
        } else {
            t::BLOCK_STROKE
        };
        kit::paint_block(ui.painter(), rect, stroke);

        // Drag-зона выделяется ДО кнопок: в egui при наложении выигрывает виджет,
        // добавленный позже, поэтому кнопки забирают клик, а всё остальное тащит окно.
        let drag = ui.interact(rect, Id::new("widget_drag"), Sense::click_and_drag());
        handle_window_drag(&ctx, &drag);

        let cy = rect.center().y;

        // Слева — кнопка заметок: панель открывается с этой же стороны.
        // Перетаскивать виджет можно за любое место, поэтому ручка-точки больше не нужна.
        let notes_r = Rect::from_min_size(pos2(rect.left() + 4.0, cy - 20.0), vec2(28.0, 40.0));
        let notes_color = if self.notes_open { accent } else { t::TEXT_TERTIARY };
        let notes_style = ButtonStyle {
            hover_color: if self.notes_open { accent } else { t::TEXT_PRIMARY },
            ..ButtonStyle::ghost(notes_color, t::RADIUS_CTRL)
        };
        if kit::button(ui, notes_r, Id::new("w_notes"), Content::Icon { glyph: ph::NOTEPAD, size: 18.0, fill: self.notes_open }, notes_style, true)
            .on_hover_text("Заметки")
            .clicked()
        {
            self.toggle_notes();
        }
        // Красная точка — есть просроченные напоминания.
        if self.state.notes.iter().any(|n| n.overdue()) {
            ui.painter().circle_filled(pos2(notes_r.right() - 6.0, notes_r.top() + 9.0), 3.0, t::DANGER);
        }

        // Справа налево: закрыть, разделитель, меню, группа play/stop или «Тихо».
        let right = rect.right() - 12.0;
        let close_r = Rect::from_center_size(pos2(right - 14.0, cy), vec2(28.0, 28.0));
        let sep_x = close_r.left() - 8.5;
        ui.painter().line_segment([pos2(sep_x, cy - 12.0), pos2(sep_x, cy + 12.0)], Stroke::new(1.0, t::LINE));
        let menu_r = Rect::from_min_size(pos2(sep_x - 8.5 - 40.0, cy - 20.0), vec2(40.0, 40.0));
        // Play, stop и меню — с одинаковым шагом 4 px. В макете до меню было 8,
        // и тройка кнопок выглядела несимметрично.
        let group_right = menu_r.left() - 4.0;
        let group_left = group_right - 84.0;

        // Текст: метка с иконкой режима над крупным временем.
        let text_left = notes_r.right() + 4.0;
        let text_w = group_left - 8.0 - text_left;
        let top = cy - 23.0;
        let label_color = if alarmed { t::DANGER } else { t::TEXT_SECONDARY };
        let (mode_icon, mode_fill) = if ringing {
            (ph::BELL_RINGING, true)
        } else if timer.as_ref().is_some_and(|tm| matches!(tm.mode, TimerMode::Stopwatch)) {
            (ph::TIMER, false)
        } else {
            (ph::HOURGLASS_MEDIUM, false)
        };
        let label = match (&timer, state) {
            (None, _) => "Нет закреплённого таймера".to_string(),
            (Some(tm), WidgetState::Ringing | WidgetState::Expired) => format!("Время вышло · {}", tm.name),
            (Some(tm), _) => tm.name.clone(),
        };
        // Голос: пока слушаем — «Слушаю…», после команды — отклик на 3 с.
        let voice_line = if self.voice_listening {
            // Точки после «Слушаю» бегут, пока идёт речь.
            let dots = (ctx.input(|i| i.time) * 3.0) as usize % 4;
            Some((format!("Слушаю{}", ".".repeat(dots)), accent))
        } else {
            self.voice_feedback.as_ref().map(|(s, ok, _)| (s.clone(), if *ok { accent } else { t::TEXT_TERTIARY }))
        };
        let alert = self.note_alert.and_then(|(id, _)| {
            self.state.notes.iter().find(|n| n.id == id).map(|n| (id, n.title.clone()))
        });
        if let Some((line, color)) = voice_line {
            static_icon(ui, Rect::from_center_size(pos2(text_left + 6.0, top + 7.0), vec2(12.0, 14.0)), ph::MICROPHONE, 12.0, false, color);
            kit::text_trunc(ui, pos2(text_left + 16.0, top + 7.0), Align2::LEFT_CENTER, &line, t::regular(11.0), color, text_w - 16.0);
        } else if let Some((id, title)) = alert {
            // Напоминание: клик открывает заметку.
            static_icon(ui, Rect::from_center_size(pos2(text_left + 6.0, top + 7.0), vec2(12.0, 14.0)), ph::BELL_RINGING, 12.0, true, accent);
            let line = format!("Напоминание · {title}");
            kit::text_trunc(ui, pos2(text_left + 16.0, top + 7.0), Align2::LEFT_CENTER, &line, t::regular(11.0), accent, text_w - 16.0);
            let hit = Rect::from_min_size(pos2(text_left, top - 2.0), vec2(text_w, 18.0));
            let resp = ui.interact(hit, Id::new("w_note_alert"), Sense::click()).on_hover_text("Открыть заметку");
            if resp.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.open_note(id);
            }
        } else {
            static_icon(ui, Rect::from_center_size(pos2(text_left + 6.0, top + 7.0), vec2(12.0, 14.0)), mode_icon, 12.0, mode_fill, label_color);
            kit::text_trunc(ui, pos2(text_left + 16.0, top + 7.0), Align2::LEFT_CENTER, &label, t::regular(11.0), label_color, text_w - 16.0);
        }
        let time = timer.as_ref().map(|tm| fmt_hms(tm.time_left().as_secs())).unwrap_or_else(|| "00:00:00".into());
        let time_color = if alarmed {
            t::DANGER
        } else if state == WidgetState::Idle {
            t::TEXT_TERTIARY
        } else {
            t::TEXT_PRIMARY
        };
        kit::text(ui, pos2(text_left, top + 16.0 + 15.0), Align2::LEFT_CENTER, &time, t::mono_medium(26.0), time_color);

        // Кнопки.
        if ringing {
            let quiet_r = Rect::from_min_size(pos2(group_left, cy - 20.0), vec2(84.0, 40.0));
            let resp = kit::button(
                ui, quiet_r, Id::new("w_quiet"),
                Content::IconText { glyph: ph::BELL_SLASH, size: 16.0, fill: false, label: "Тихо", font: t::medium(13.0), gap: 6.0 },
                ButtonStyle::danger_solid(), true,
            );
            if resp.clicked() {
                self.silence(true);
            }
        } else if let Some(tm) = &timer {
            let play_r = Rect::from_min_size(pos2(group_left, cy - 20.0), vec2(40.0, 40.0));
            let stop_r = Rect::from_min_size(pos2(group_left + 44.0, cy - 20.0), vec2(40.0, 40.0));
            let can_play = matches!(state, WidgetState::Running | WidgetState::Paused);
            let play_style = if can_play { ButtonStyle::accent(accent, t::RADIUS_BTN_LG) } else { ButtonStyle::secondary(t::RADIUS_BTN_LG) };
            let glyph = if state == WidgetState::Running { ph::PAUSE } else { ph::PLAY };
            if kit::button(ui, play_r, Id::new("w_play"), Content::Icon { glyph, size: 20.0, fill: true }, play_style, can_play).clicked() {
                self.toggle_timer(tm.id);
            }
            if kit::button(ui, stop_r, Id::new("w_stop"), Content::Icon { glyph: ph::STOP, size: 18.0, fill: true }, ButtonStyle::secondary(t::RADIUS_BTN_LG), true)
                .on_hover_text("Остановить и записать в историю")
                .clicked()
            {
                self.remove_timer(tm.id, true);
            }
        } else {
            let play_r = Rect::from_min_size(pos2(group_left, cy - 20.0), vec2(40.0, 40.0));
            let stop_r = Rect::from_min_size(pos2(group_left + 44.0, cy - 20.0), vec2(40.0, 40.0));
            kit::button(ui, play_r, Id::new("w_play"), Content::Icon { glyph: ph::PLAY, size: 20.0, fill: true }, ButtonStyle::secondary(t::RADIUS_BTN_LG), false);
            kit::button(ui, stop_r, Id::new("w_stop"), Content::Icon { glyph: ph::STOP, size: 18.0, fill: true }, ButtonStyle::secondary(t::RADIUS_BTN_LG), false);
        }

        let (menu_glyph, menu_style) = if self.menu_open {
            (if self.menu_up { ph::CARET_UP } else { ph::CARET_DOWN }, ButtonStyle::accent(accent, t::RADIUS_BTN_LG))
        } else {
            (ph::LIST, ButtonStyle::secondary(t::RADIUS_BTN_LG))
        };
        if kit::button(ui, menu_r, Id::new("w_menu"), Content::Icon { glyph: menu_glyph, size: 20.0, fill: false }, menu_style, true).clicked() {
            self.menu_open = !self.menu_open;
            self.accent_picker_open = false;
        }

        let to_tray = self.tray.is_some() && self.config.close_to_tray;
        if kit::button(ui, close_r, Id::new("w_close"), Content::Icon { glyph: ph::X, size: 16.0, fill: false }, ButtonStyle::ghost(t::TEXT_TERTIARY, t::RADIUS_CTRL), true)
            .on_hover_text(if to_tray { "Свернуть в трей" } else { "Закрыть программу" })
            .clicked()
        {
            if to_tray {
                self.set_widget_visible(&ctx, false);
            } else {
                self.sync_and_save();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    // ------------------------------------------------------------ плашка «Сейчас играет»

    fn draw_music(&mut self, ui: &mut Ui, rect: Rect, accent: Color32) {
        let ctx = ui.ctx().clone();
        // Перетаскивание плашки между местами над и под виджетом обрабатывается
        // в раскладке стопки (см. ui()): там известно положение виджета.
        kit::paint_block(ui.painter(), rect, t::BLOCK_STROKE);

        // Плашка ↔ мини: высота анимируется, текст сменяется кросс-фейдом.
        let full_alpha = ((rect.height() - t::MUSIC_MINI_H) / (t::MUSIC_H - t::MUSIC_MINI_H)).clamp(0.0, 1.0);
        let mut content = ui.new_child(UiBuilder::new().max_rect(rect).id_salt("music_content"));
        content.set_clip_rect(rect.intersect(ui.clip_rect()));

        if full_alpha < 1.0 {
            let mut mini = content.new_child(UiBuilder::new().max_rect(rect).id_salt("music_mini"));
            mini.multiply_opacity(1.0 - full_alpha);
            let cy = rect.center().y;
            static_icon(&mini, Rect::from_center_size(pos2(rect.left() + 19.0, cy), vec2(14.0, 14.0)), ph::MUSIC_NOTES_SIMPLE, 14.0, false, t::TEXT_TERTIARY);
            kit::text(&mini, pos2(rect.left() + 34.0, cy), Align2::LEFT_CENTER, "Ничего не играет", t::regular(12.0), t::TEXT_TERTIARY);
        }

        let Some(snap) = self.music_last.clone() else { return };
        if full_alpha <= 0.0 {
            return;
        }
        let full_rect = Rect::from_min_size(rect.min, vec2(rect.width(), t::MUSIC_H));
        let mut full = content.new_child(UiBuilder::new().max_rect(full_rect).id_salt("music_full"));
        full.multiply_opacity(full_alpha);
        let cy = full_rect.center().y;

        // Обложка 40×40 или заглушка.
        let cover_r = Rect::from_min_size(pos2(full_rect.left() + 8.0, cy - 20.0), vec2(40.0, 40.0));
        match &self.cover {
            Some((_, tex)) if snap.cover.is_some() => {
                egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), vec2(40.0, 40.0)))
                    .corner_radius(t::RADIUS_CTRL)
                    .paint_at(&full, cover_r);
            }
            _ => {
                full.painter().rect_filled(cover_r, t::RADIUS_CTRL, t::SURFACE_2);
                static_icon(&full, cover_r, ph::MUSIC_NOTE, 18.0, false, t::TEXT_TERTIARY);
            }
        }

        // Перемотка. Зона захвата 298×16 задевает низ кнопок справа, поэтому
        // регистрируется до них: там, где зоны пересекаются, клик забирают кнопки.
        let track_l = full_rect.left() + 14.0;
        let track_w = full_rect.width() - 28.0;
        let frac_at = |x: f32| ((x - track_l) / track_w).clamp(0.0, 1.0);
        let can_seek = snap.can_seek && snap.progress.is_some() && snap.duration_secs > 0.0;
        let mut seek_hover = false;
        if can_seek {
            let zone = Rect::from_min_size(pos2(track_l, full_rect.top() + 36.0), vec2(track_w, 16.0));
            let resp = full.interact(zone, Id::new("music_seek"), Sense::click_and_drag());
            seek_hover = resp.hovered() || resp.dragged();
            if seek_hover {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.drag_started() {
                self.seek.cancelled = false;
            }
            if resp.dragged() && !self.seek.cancelled {
                if let Some(p) = resp.interact_pointer_pos() {
                    let f = frac_at(p.x);
                    self.seek.drag = Some(f);
                    self.seek.label_fraction = f;
                }
                // Esc — отмена: бегунок возвращается к позиции источника.
                if full.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.seek.drag = None;
                    self.seek.cancelled = true;
                }
            }
            if resp.drag_stopped() {
                // Команда уходит один раз — при отпускании.
                if let Some(f) = self.seek.drag.take() {
                    self.send_seek(f, snap.duration_secs);
                }
                self.seek.cancelled = false;
            } else if resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    self.send_seek(frac_at(p.x), snap.duration_secs);
                }
            }
        } else {
            self.seek.drag = None;
        }
        let seeking = self.seek.drag.is_some();

        // Кнопки: назад 28×32, play 32×32, вперёд 28×32, зазор 2.
        let right = full_rect.right() - 8.0;
        let next_r = Rect::from_min_size(pos2(right - 28.0, cy - 16.0), vec2(28.0, 32.0));
        let play_r = Rect::from_min_size(pos2(next_r.left() - 2.0 - 32.0, cy - 16.0), vec2(32.0, 32.0));
        let prev_r = Rect::from_min_size(pos2(play_r.left() - 2.0 - 28.0, cy - 16.0), vec2(28.0, 32.0));

        let mut command = None;
        if snap.can_prev {
            if kit::button(&full, prev_r, Id::new("m_prev"), Content::Icon { glyph: ph::SKIP_BACK, size: 16.0, fill: true }, ButtonStyle::ghost(t::TEXT_SECONDARY, t::RADIUS_CTRL), true).clicked() {
                command = Some(MediaCmd::Previous);
            }
        } else {
            static_icon(&full, prev_r, ph::SKIP_BACK, 16.0, true, t::TEXT_DISABLED);
        }
        if snap.can_toggle {
            let style = if snap.playing { ButtonStyle::accent(accent, t::RADIUS_CTRL) } else { ButtonStyle::secondary_bright(t::RADIUS_CTRL) };
            let glyph = if snap.playing { ph::PAUSE } else { ph::PLAY };
            if kit::button(&full, play_r, Id::new("m_play"), Content::Icon { glyph, size: 16.0, fill: true }, style, true).clicked() {
                command = Some(MediaCmd::TogglePlayPause);
            }
        } else {
            full.painter().rect_filled(play_r, t::RADIUS_CTRL, t::SURFACE_2);
            static_icon(&full, play_r, if snap.playing { ph::PAUSE } else { ph::PLAY }, 16.0, true, t::TEXT_DISABLED);
        }
        if snap.can_next {
            if kit::button(&full, next_r, Id::new("m_next"), Content::Icon { glyph: ph::SKIP_FORWARD, size: 16.0, fill: true }, ButtonStyle::ghost(t::TEXT_SECONDARY, t::RADIUS_CTRL), true).clicked() {
                command = Some(MediaCmd::Next);
            }
        } else {
            static_icon(&full, next_r, ph::SKIP_FORWARD, 16.0, true, t::TEXT_DISABLED);
        }
        if let Some(cmd) = command {
            if matches!(cmd, MediaCmd::TogglePlayPause) {
                self.music_paused_by_alarm = false;
            }
            self.media.send(cmd);
        }

        // Название и «исполнитель · источник».
        let text_left = cover_r.right() + 10.0;
        let text_w = (prev_r.left() - 10.0 - text_left).max(10.0);
        let title_color = if snap.playing { t::TEXT_PRIMARY } else { t::TEXT_SECONDARY };
        let title = if snap.title.trim().is_empty() { snap.source.clone() } else { snap.title.clone() };
        let sub_job = |max_width: f32| {
            let mut job = LayoutJob::default();
            let artist_fmt = TextFormat { font_id: t::regular(11.0), color: t::TEXT_SECONDARY, ..Default::default() };
            let source_fmt = TextFormat { font_id: t::regular(11.0), color: t::TEXT_TERTIARY, ..Default::default() };
            if snap.artist.trim().is_empty() {
                job.append(&snap.source, 0.0, source_fmt);
            } else {
                job.append(&snap.artist, 0.0, artist_fmt);
                job.append(&format!(" · {}", snap.source), 0.0, source_fmt);
            }
            job.wrap.max_width = max_width;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            job.wrap.overflow_character = Some('…');
            job
        };

        // Бегущая строка: только если текст не влезает в колонку.
        let title_full = full.painter().layout_no_wrap(title.clone(), t::medium(13.0), title_color);
        let sub_full = full.painter().layout_job(sub_job(f32::INFINITY));
        let d_title = (title_full.size().x - text_w).max(0.0);
        let d_sub = (sub_full.size().x - text_w).max(0.0);
        let hovered = full.rect_contains_pointer(full_rect);
        let track_key = format!("{}\u{1}{}\u{1}{}", title, snap.artist, snap.source);
        let marquee = self.marquee_step(&ctx, &track_key, d_title, d_sub, hovered, seeking);

        // Подпись времени вместо второй строки: при перетаскивании и 800 мс после.
        let now = Instant::now();
        if self.seek.label_until.is_some_and(|u| u <= now) {
            self.seek.label_until = None;
        }
        if let Some(u) = self.seek.label_until {
            ctx.request_repaint_after(u - now);
        }
        let show_time = seeking || self.seek.label_until.is_some();
        let time_a = ctx.animate_value_with_time(
            Id::new("seek_label"),
            if show_time { 1.0 } else { 0.0 },
            if show_time { t::ANIM_TIME_LABEL_IN } else { t::ANIM_TIME_LABEL_OUT },
        );

        let title_row = Rect::from_min_size(pos2(text_left, cy - 17.0), vec2(text_w, 18.0));
        let sub_row = Rect::from_min_size(pos2(text_left, cy + 3.0), vec2(text_w, 14.0));
        match marquee {
            None => {
                kit::text_trunc(&full, pos2(text_left, title_row.center().y), Align2::LEFT_CENTER, &title, t::medium(13.0), title_color, text_w);
            }
            Some((off, _)) => {
                self.paint_marquee_line(&full, title_row, title_full, off, d_title, "mq_title");
            }
        }
        if time_a < 1.0 {
            let mut sub_ui = full.new_child(UiBuilder::new().max_rect(sub_row).id_salt("music_sub"));
            sub_ui.multiply_opacity(1.0 - time_a);
            match marquee {
                None => {
                    let g = sub_ui.painter().layout_job(sub_job(text_w));
                    kit::paint_galley(sub_ui.painter(), pos2(text_left, sub_row.center().y), Align2::LEFT_CENTER, g, t::TEXT_SECONDARY);
                }
                Some((_, off)) => {
                    self.paint_marquee_line(&sub_ui, sub_row, sub_full, off, d_sub, "mq_sub");
                }
            }
        }
        if time_a > 0.0 {
            let mut time_ui = full.new_child(UiBuilder::new().max_rect(sub_row).id_salt("music_time"));
            time_ui.multiply_opacity(time_a);
            let fraction = self.seek.drag.unwrap_or(self.seek.label_fraction) as f64;
            let mut job = LayoutJob::default();
            job.append(
                &fmt_track_time(fraction * snap.duration_secs),
                0.0,
                TextFormat { font_id: t::mono_medium(11.0), color: t::TEXT_PRIMARY, ..Default::default() },
            );
            job.append(
                &format!(" / {}", fmt_track_time(snap.duration_secs)),
                0.0,
                TextFormat { font_id: t::mono(11.0), color: t::TEXT_TERTIARY, ..Default::default() },
            );
            let g = time_ui.painter().layout_job(job);
            kit::paint_galley(time_ui.painter(), pos2(text_left, sub_row.center().y), Align2::LEFT_CENTER, g, t::TEXT_PRIMARY);
        }

        // Полоска прогресса: покой — высота 2; наведение и перетаскивание — 4,
        // растёт вверх, низ на 4 от края. Бегунок ⌀8 при наведении, ⌀10 при захвате.
        if let Some(p) = snap.progress {
            if let Some((target, at)) = self.seek.pending {
                if (p - target).abs() < 0.02 || at.elapsed() > Duration::from_millis(2500) {
                    self.seek.pending = None;
                }
            }
            let progress_id = Id::new("music_progress");
            let shown = if let Some(f) = self.seek.drag {
                ctx.animate_value_with_time(progress_id, f, 0.0001)
            } else if let Some((target, _)) = self.seek.pending {
                ctx.animate_value_with_time(progress_id, target, t::ANIM_SEEK_CLICK)
            } else {
                ctx.animate_value_with_time(progress_id, p, t::ANIM_PROGRESS)
            };
            let active = can_seek && (seek_hover || seeking);
            let hover_t = ctx.animate_value_with_time(
                Id::new("seek_hover"),
                if active { 1.0 } else { 0.0 },
                if active { t::ANIM_SEEK_ENTER } else { t::ANIM_SEEK_LEAVE },
            );
            let grab_t = ctx.animate_bool_with_time(Id::new("seek_grab"), seeking, t::ANIM_SEEK_GRAB);
            let bar_h = 2.0 + 2.0 * hover_t;
            let bottom = full_rect.bottom() - 4.0;
            let track = Rect::from_min_max(pos2(track_l, bottom - bar_h), pos2(track_l + track_w, bottom));
            let radius: u8 = if bar_h > 3.0 { 2 } else { 1 };
            full.painter().rect_filled(track, radius, t::LINE);
            let fill_r = Rect::from_min_max(track.min, pos2(track_l + track_w * shown.clamp(0.0, 1.0), bottom));
            full.painter().rect_filled(fill_r, radius, if snap.playing { accent } else { t::TEXT_TERTIARY });
            let knob_d = 8.0 * hover_t + 2.0 * grab_t;
            if knob_d > 0.5 {
                let center = pos2(track_l + track_w * shown.clamp(0.0, 1.0), full_rect.top() + 50.0);
                full.painter().circle_filled(center, knob_d / 2.0, t::KNOB);
            }
        }
    }

    fn send_seek(&mut self, fraction: f32, duration_secs: f64) {
        self.media.send(MediaCmd::Seek(fraction as f64 * duration_secs));
        self.seek.pending = Some((fraction, Instant::now()));
        self.seek.label_fraction = fraction;
        self.seek.label_until = Some(Instant::now() + Duration::from_secs_f32(t::SEEK_LABEL_HOLD));
    }

    /// Бегущая строка. Возвращает сдвиги строк (название, вторая строка) или
    /// `None`, если строки стоят на месте и показываются с многоточием.
    ///
    /// Цикл: пауза 1200 мс → проезд 30 px/с → пауза 1000 мс → возврат 300 мс.
    /// Один цикл после смены трека и по кругу, пока курсор над плашкой.
    /// Перерисовка на частоте монитора — только во время движения, на паузах
    /// следующий кадр заказывается ровно на их конец.
    fn marquee_step(&mut self, ctx: &egui::Context, key: &str, d_title: f32, d_sub: f32, hovered: bool, paused: bool) -> Option<(f32, f32)> {
        let overflow = d_title > 0.5 || d_sub > 0.5;
        let now = Instant::now();
        let m = &mut self.marquee;
        if m.track != key {
            m.track = key.to_owned();
            m.phase = if overflow { MarqueePhase::StartPause } else { MarqueePhase::Idle };
            m.since = now;
            m.by_hover = false;
        }
        if !overflow {
            m.phase = MarqueePhase::Idle;
            return None;
        }
        // Во время перемотки строка замирает на месте.
        if paused && m.phase != MarqueePhase::Idle {
            let dt = ctx.input(|i| i.unstable_dt).clamp(0.0, 0.1);
            m.since += Duration::from_secs_f32(dt);
        }
        let travel = d_title.max(d_sub) / t::MARQUEE_SPEED;
        let offsets_at = |phase: MarqueePhase, el: f32| match phase {
            MarqueePhase::Travel => ((el * t::MARQUEE_SPEED).min(d_title), (el * t::MARQUEE_SPEED).min(d_sub)),
            MarqueePhase::EndPause => (d_title, d_sub),
            _ => (0.0, 0.0),
        };

        let el = now.saturating_duration_since(m.since).as_secs_f32();
        match m.phase {
            MarqueePhase::Idle => {
                if hovered && !paused {
                    m.phase = MarqueePhase::StartPause;
                    m.since = now;
                    m.by_hover = true;
                }
            }
            MarqueePhase::StartPause if el >= t::MARQUEE_START_PAUSE => {
                m.phase = MarqueePhase::Travel;
                m.since = now;
            }
            MarqueePhase::Travel if el >= travel => {
                m.phase = MarqueePhase::EndPause;
                m.since = now;
            }
            MarqueePhase::EndPause if el >= t::MARQUEE_END_PAUSE => {
                m.from = (d_title, d_sub);
                m.phase = MarqueePhase::Return;
                m.since = now;
            }
            MarqueePhase::Return if el >= t::MARQUEE_RETURN => {
                if hovered {
                    m.phase = MarqueePhase::StartPause;
                    m.by_hover = true;
                } else {
                    m.phase = MarqueePhase::Idle;
                }
                m.since = now;
            }
            _ => {}
        }
        // Курсор ушёл посреди цикла, запущенного наведением, — сразу возврат.
        if m.by_hover && !hovered && matches!(m.phase, MarqueePhase::StartPause | MarqueePhase::Travel | MarqueePhase::EndPause) {
            let el = now.saturating_duration_since(m.since).as_secs_f32();
            m.from = offsets_at(m.phase, el);
            m.phase = MarqueePhase::Return;
            m.since = now;
            m.by_hover = false;
        }

        let el = now.saturating_duration_since(m.since).as_secs_f32();
        let offsets = match m.phase {
            MarqueePhase::Idle => return None,
            MarqueePhase::Return => {
                let k = ease_out(el / t::MARQUEE_RETURN);
                (m.from.0 * (1.0 - k), m.from.1 * (1.0 - k))
            }
            phase => offsets_at(phase, el),
        };
        match m.phase {
            MarqueePhase::Travel | MarqueePhase::Return => ctx.request_repaint(),
            MarqueePhase::StartPause => ctx.request_repaint_after(Duration::from_secs_f32((t::MARQUEE_START_PAUSE - el).max(0.0))),
            MarqueePhase::EndPause => ctx.request_repaint_after(Duration::from_secs_f32((t::MARQUEE_END_PAUSE - el).max(0.0))),
            MarqueePhase::Idle => {}
        }
        Some(offsets)
    }

    /// Строка бегущего текста: сдвиг влево на `off`, обрезка по колонке и
    /// растворение у краёв на 16 px (слева — когда текст сдвинут, справа —
    /// пока конец текста не доехал). Края появляются и гаснут за 120 мс.
    fn paint_marquee_line(&self, ui: &Ui, row: Rect, galley: Arc<egui::Galley>, off: f32, overflow: f32, id: &str) {
        let painter = ui.painter().with_clip_rect(row);
        let g_h = galley.size().y;
        // Цвета уже заданы в самом тексте, третий аргумент — только запасной.
        painter.galley(pos2(row.left() - off, row.center().y - g_h / 2.0), galley, t::TEXT_PRIMARY);
        if overflow <= 0.5 {
            return;
        }
        let left_a = ui.ctx().animate_bool_with_time(Id::new((id, "l")), off > 0.5, t::ANIM_EDGE_FADE);
        let right_a = ui.ctx().animate_bool_with_time(Id::new((id, "r")), off < overflow - 0.5, t::ANIM_EDGE_FADE);
        let fade = t::MARQUEE_FADE;
        let edge = |rect: Rect, solid_left: bool, alpha: f32| {
            if alpha <= 0.0 {
                return;
            }
            let solid = t::BLOCK_BG.gamma_multiply(alpha);
            let (l, r) = if solid_left { (solid, Color32::TRANSPARENT) } else { (Color32::TRANSPARENT, solid) };
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(rect.left_top(), l);
            mesh.colored_vertex(rect.right_top(), r);
            mesh.colored_vertex(rect.right_bottom(), r);
            mesh.colored_vertex(rect.left_bottom(), l);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(mesh);
        };
        edge(Rect::from_min_size(row.min, vec2(fade, row.height())), true, left_a);
        edge(Rect::from_min_size(pos2(row.right() - fade, row.top()), vec2(fade, row.height())), false, right_a);
    }

    // ------------------------------------------------------------ меню

    // ------------------------------------------------------------ заметки: панель

    /// Помещается ли панель слева от виджета (не уходит за край мониторов).
    fn notes_fit_left(&self, ctx: &egui::Context, side: f32) -> bool {
        let Some(a) = self.anchor_px else { return true };
        let x = a.x - side * ctx.pixels_per_point();
        point_on_any_monitor(x as i32, a.y as i32 + 8)
    }

    fn draw_notes(&mut self, ui: &mut Ui, rect: Rect, accent: Color32) {
        let ctx = ui.ctx().clone();
        kit::paint_block(ui.painter(), rect, t::BLOCK_STROKE);
        let inner = rect.shrink(t::BLOCK_PAD);
        let (left, w) = (inner.left(), inner.width());
        let mut y = inner.top();

        // Шапка — за неё тоже можно тащить окно.
        let head = Rect::from_min_size(pos2(left, y), vec2(w, 28.0));
        let drag = ui.interact(head, Id::new("notes_drag"), Sense::click_and_drag());
        handle_window_drag(&ctx, &drag);
        let title_r = kit::text(ui, pos2(left, head.center().y), Align2::LEFT_CENTER, "Заметки", t::medium(15.0), t::TEXT_PRIMARY);
        let open_count = self.state.notes.iter().filter(|n| !n.done).count();
        if open_count > 0 {
            kit::text(ui, pos2(title_r.right() + 8.0, head.center().y + 1.0), Align2::LEFT_CENTER, &open_count.to_string(), t::mono(12.0), t::TEXT_TERTIARY);
        }
        let btn = |x: f32| Rect::from_min_size(pos2(x, head.top()), vec2(28.0, 28.0));
        let mut bx = head.right() - 28.0;
        if kit::button(ui, btn(bx), Id::new("notes_close"), Content::Icon { glyph: ph::X, size: 14.0, fill: false }, ButtonStyle::ghost(t::TEXT_TERTIARY, t::RADIUS_CTRL), true)
            .on_hover_text("Скрыть заметки")
            .clicked()
        {
            self.notes_open = false;
            self.folder_edit = None;
        }
        bx -= 30.0;
        // Сортировка — выпадающее меню под кнопкой. Не по умолчанию — иконка в акценте.
        let sort_color = if self.config.notes_sort == NotesSort::default() { t::TEXT_SECONDARY } else { accent };
        let sort_resp = kit::button(ui, btn(bx), Id::new("notes_sort"), Content::Icon { glyph: ph::SORT_ASCENDING, size: 16.0, fill: false }, ButtonStyle::ghost(sort_color, t::RADIUS_CTRL), true)
            .on_hover_text(format!("Сортировка: {}", self.config.notes_sort.label()));
        if let Some(chosen) = self.sort_menu(&sort_resp, accent) {
            if chosen != self.config.notes_sort {
                self.config.notes_sort = chosen;
                self.save_to_disk();
            }
        }
        bx -= 30.0;
        let search_color = if self.notes_search_open { accent } else { t::TEXT_SECONDARY };
        if kit::button(ui, btn(bx), Id::new("notes_search_btn"), Content::Icon { glyph: ph::MAGNIFYING_GLASS, size: 16.0, fill: false }, ButtonStyle::ghost(search_color, t::RADIUS_CTRL), true)
            .on_hover_text("Поиск")
            .clicked()
        {
            self.notes_search_open = !self.notes_search_open;
            if self.notes_search_open {
                ctx.memory_mut(|m| m.request_focus(Id::new("notes_search")));
            } else {
                self.notes_search.clear();
            }
        }
        y += 28.0 + 8.0;

        if self.notes_search_open {
            let r = Rect::from_min_size(pos2(left, y), vec2(w, 30.0));
            text_field(ui, r, Id::new("notes_search"), &mut self.notes_search, "Найти в заметках", accent);
            y += 30.0 + 8.0;
        }

        y += self.draw_folder_chips(ui, left, y, w, accent) + 10.0;

        let input_h = 36.0;
        let list_r = Rect::from_min_max(pos2(left, y), pos2(inner.right(), inner.bottom() - input_h - 10.0));
        self.draw_notes_list(ui, list_r, accent);

        let input_r = Rect::from_min_size(pos2(left, inner.bottom() - input_h), vec2(w, input_h));
        self.draw_new_note(ui, input_r, accent);
    }

    /// Выпадающее меню сортировки под кнопкой. Возвращает выбранный порядок.
    fn sort_menu(&self, button: &Response, accent: Color32) -> Option<NotesSort> {
        const W: f32 = 200.0;
        let current = self.config.notes_sort;
        let frame = egui::Frame::NONE
            .fill(t::SURFACE_1)
            .stroke(Stroke::new(1.0, t::LINE))
            .corner_radius(t::RADIUS_CTRL)
            .inner_margin(6.0);
        let mut chosen = None;
        egui::Popup::menu(button)
            .align(egui::RectAlign::BOTTOM_END)
            .gap(4.0)
            .width(W)
            .frame(frame)
            .show(|ui| {
                let (head, _) = ui.allocate_exact_size(vec2(W - 12.0, 22.0), Sense::hover());
                kit::overline(ui, pos2(head.left() + 6.0, head.center().y), "Сортировка");
                for sort in NotesSort::ALL {
                    let (r, resp) = ui.allocate_exact_size(vec2(W - 12.0, 30.0), Sense::click());
                    let selected = sort == current;
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(r, t::RADIUS_CHIP, t::SURFACE_3);
                    } else if selected {
                        ui.painter().rect_filled(r, t::RADIUS_CHIP, t::tint(accent, 0x22));
                    }
                    let glyph = match sort {
                        NotesSort::Newest => ph::ARROW_DOWN,
                        NotesSort::Oldest => ph::ARROW_UP,
                        NotesSort::Reminder => ph::BELL,
                        NotesSort::Alpha => ph::TEXT_AA,
                        NotesSort::Folder => ph::FOLDER_SIMPLE,
                    };
                    let color = if selected { accent } else { t::TEXT_PRIMARY };
                    static_icon(ui, Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), vec2(14.0, 14.0)), glyph, 14.0, false, if selected { accent } else { t::TEXT_SECONDARY });
                    kit::text(ui, pos2(r.left() + 30.0, r.center().y), Align2::LEFT_CENTER, sort.label(), t::regular(13.0), color);
                    if selected {
                        static_icon(ui, Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(14.0, 14.0)), ph::CHECK, 14.0, false, accent);
                    }
                    if resp.clicked() {
                        chosen = Some(sort);
                        ui.close();
                    }
                }
            });
        chosen
    }

    /// Вид папок для карточки заметки: «Без папки» и все папки.
    fn editor_folder_options(&self) -> Vec<(Option<u64>, String, Option<Color32>)> {
        std::iter::once((None, "Без папки".to_string(), None))
            .chain(self.state.folders.iter().map(|f| (Some(f.id), f.name.clone(), Some(folder_color(f.color)))))
            .collect()
    }

    /// Высота рядов папок в карточке заметки при ширине `w`.
    fn editor_folders_h(&self, ui: &Ui, w: f32) -> f32 {
        let widths: Vec<f32> = self
            .editor_folder_options()
            .iter()
            .map(|(_, name, dot)| chip_width(ui, name, dot.is_some(), 11.0))
            .collect();
        let (_, rows) = flow(&widths, w, 4.0);
        rows as f32 * 28.0 - 4.0
    }

    /// Папки над списком: «Все», папки, «+». Не помещаются в строку — переносятся.
    /// Правый клик по папке — меню. Возвращает занятую высоту.
    fn draw_folder_chips(&mut self, ui: &mut Ui, left: f32, y: f32, w: f32, accent: Color32) -> f32 {
        const H: f32 = 26.0;
        const GAP: f32 = 6.0;
        const EDIT_W: f32 = 130.0;
        let size = 12.0;
        let count = |folder: Option<u64>| {
            self.state.notes.iter().filter(|n| !n.done && folder.is_none_or(|f| n.in_folder(f))).count()
        };
        let editing = self.folder_edit.as_ref().map(|(target, _)| *target);
        // (папка, подпись, цвет точки)
        let mut chips: Vec<(Option<u64>, String, Option<Color32>)> = vec![(None, format!("Все {}", count(None)), None)];
        for f in &self.state.folders {
            let n = count(Some(f.id));
            let label = if n > 0 { format!("{} {n}", f.name) } else { f.name.clone() };
            chips.push((Some(f.id), label, Some(folder_color(f.color))));
        }
        let mut widths: Vec<f32> = chips
            .iter()
            .map(|(fid, label, dot)| {
                if fid.is_some() && editing == Some(*fid) { EDIT_W } else { chip_width(ui, label, dot.is_some(), size) }
            })
            .collect();
        widths.push(if editing == Some(None) { EDIT_W } else { 28.0 });
        let (pos, rows) = flow(&widths, w, GAP);
        let rect_of = |k: usize| {
            let (x, row) = pos[k];
            Rect::from_min_size(pos2(left + x, y + row as f32 * (H + GAP)), vec2(widths[k], H))
        };

        let mut acts: Vec<FolderAct> = Vec::new();
        let mut edit_text = self.folder_edit.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        let mut edit_done: Option<bool> = None; // Some(true) — сохранить, Some(false) — отмена
        let filter = self.notes_filter;
        for (k, (fid, label, dot)) in chips.iter().enumerate() {
            let r = rect_of(k);
            if fid.is_some() && editing == Some(*fid) {
                folder_edit_field(ui, r, &mut edit_text, &mut edit_done, accent);
                continue;
            }
            let resp = folder_chip(ui, r, Id::new(("folder_chip", k)), label, *dot, filter == *fid, accent, size);
            if resp.clicked() {
                acts.push(FolderAct::Select(*fid));
            }
            if let Some(id) = fid {
                resp.on_hover_text("Правый клик — переименовать, цвет, удалить").context_menu(|ui| {
                    if ui.button("Переименовать").clicked() {
                        acts.push(FolderAct::Rename(*id));
                        ui.close();
                    }
                    if ui.button("Сменить цвет").clicked() {
                        acts.push(FolderAct::Recolor(*id));
                        ui.close();
                    }
                    if ui.button("Удалить папку").clicked() {
                        acts.push(FolderAct::Delete(*id));
                        ui.close();
                    }
                });
            }
        }
        let tail = rect_of(chips.len());
        if editing == Some(None) {
            folder_edit_field(ui, tail, &mut edit_text, &mut edit_done, accent);
        } else if kit::button(ui, tail, Id::new("folder_add"), Content::Icon { glyph: ph::PLUS, size: 14.0, fill: false }, ButtonStyle::ghost(t::TEXT_SECONDARY, t::RADIUS_CHIP), true)
            .on_hover_text("Новая папка")
            .clicked()
        {
            acts.push(FolderAct::StartNew);
        }

        // Ввод названия папки.
        if let Some(edit) = &mut self.folder_edit {
            edit.1 = edit_text;
        }
        if let Some(ok) = edit_done {
            if let Some((target, text)) = self.folder_edit.take() {
                let name = text.trim().to_string();
                if ok {
                    match target {
                        None => {
                            let id = self.create_folder(&name);
                            self.notes_filter = Some(id);
                        }
                        Some(id) => {
                            if let Some(f) = self.state.folders.iter_mut().find(|f| f.id == id) {
                                f.name = notes::capitalize(&name);
                            }
                            self.save_to_disk();
                        }
                    }
                }
            }
        }
        for act in acts {
            match act {
                FolderAct::Select(fid) => {
                    self.notes_filter = fid;
                    self.notes_expanded = None;
                }
                FolderAct::StartNew => {
                    self.folder_edit = Some((None, String::new()));
                    ui.ctx().memory_mut(|m| m.request_focus(Id::new("folder_edit")));
                }
                FolderAct::Rename(id) => {
                    self.folder_edit = Some((Some(id), self.folder_name(id)));
                    ui.ctx().memory_mut(|m| m.request_focus(Id::new("folder_edit")));
                }
                FolderAct::Recolor(id) => {
                    if let Some(f) = self.state.folders.iter_mut().find(|f| f.id == id) {
                        let i = notes::FOLDER_COLORS.iter().position(|c| *c == f.color).unwrap_or(0);
                        f.color = notes::FOLDER_COLORS[(i + 1) % notes::FOLDER_COLORS.len()];
                    }
                    self.save_to_disk();
                }
                FolderAct::Delete(id) => self.remove_folder(id),
            }
        }
        rows as f32 * H + (rows - 1) as f32 * GAP
    }

    fn draw_notes_list(&mut self, ui: &mut Ui, area: Rect, accent: Color32) {
        let now = notes::now();
        let query = self.notes_search.trim().to_lowercase();
        let visible = |n: &Note| {
            self.notes_filter.is_none_or(|f| n.in_folder(f))
                && (query.is_empty()
                    || n.title.to_lowercase().contains(&query)
                    || n.body.to_lowercase().contains(&query)
                    || n.items.iter().any(|i| i.text.to_lowercase().contains(&query)))
        };
        let mut open: Vec<usize> = (0..self.state.notes.len()).filter(|&i| !self.state.notes[i].done && visible(&self.state.notes[i])).collect();
        notes::sort_indices(&self.state.notes, &self.state.folders, &mut open, self.config.notes_sort);
        let mut done: Vec<usize> = (0..self.state.notes.len()).filter(|&i| self.state.notes[i].done && visible(&self.state.notes[i])).collect();
        done.sort_by(|&a, &b| self.state.notes[b].done_at.cmp(&self.state.notes[a].done_at));

        if open.is_empty() && done.is_empty() {
            let c = area.center();
            let title = if !query.is_empty() {
                "Ничего не найдено"
            } else if self.notes_filter.is_some() {
                "В этой папке пусто"
            } else {
                "Пока пусто"
            };
            kit::text(ui, c - vec2(0.0, 22.0), Align2::CENTER_CENTER, title, t::medium(13.0), t::TEXT_SECONDARY);
            kit::text(ui, c, Align2::CENTER_CENTER, "Напишите заметку ниже", t::regular(12.0), t::TEXT_TERTIARY);
            kit::text(ui, c + vec2(0.0, 18.0), Align2::CENTER_CENTER, "или скажите «Джарвис, заметка …»", t::regular(12.0), t::TEXT_TERTIARY);
            return;
        }

        let mut acts: Vec<NoteAct> = Vec::new();
        let mut list = ui.new_child(UiBuilder::new().max_rect(area).id_salt("notes_list"));
        egui::ScrollArea::vertical()
            .id_salt("notes_scroll")
            .auto_shrink([false, false])
            .max_height(area.height())
            .show(&mut list, |ui| {
                // Высоты строк — по фактической ширине (ряды папок в карточке переносятся).
                let avail = ui.available_width();
                let folders_h = self.editor_folders_h(ui, avail - 20.0);
                // Раскрытие и сворачивание карточки — плавное изменение высоты строки.
                let row_h = |me: &Self, i: usize| {
                    let n = &me.state.notes[i];
                    let target = 36.0
                        + if me.notes_expanded == Some(n.id) {
                            note_editor_h(n, folders_h, &editor_layout(ui, n, avail - 20.0))
                        } else {
                            0.0
                        };
                    ui.ctx().animate_value_with_time(Id::new(("note_row_h", n.id)), target, t::ANIM_ROW_ADD)
                };
                let open: Vec<(usize, f32)> = open.iter().map(|&i| (i, row_h(self, i))).collect();
                let done: Vec<(usize, f32)> = done.iter().map(|&i| (i, row_h(self, i))).collect();
                let mut total_h: f32 = open.iter().map(|(_, h)| h + 4.0).sum();
                if !done.is_empty() {
                    total_h += 4.0 + 28.0 + 4.0;
                    if self.notes_done_open {
                        total_h += done.iter().map(|(_, h)| h + 4.0).sum::<f32>();
                    }
                }
                let (r, _) = ui.allocate_exact_size(vec2(avail, total_h), Sense::hover());
                let mut y = r.top();
                for &(i, h) in &open {
                    let row = Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), h));
                    self.draw_note_row(ui, row, i, accent, now, folders_h, &mut acts);
                    y += h + 4.0;
                }
                if !done.is_empty() {
                    y += 4.0;
                    let hr = Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), 28.0));
                    let resp = ui.interact(hr, Id::new("notes_done_head"), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        self.notes_done_open = !self.notes_done_open;
                    }
                    let caret = if self.notes_done_open { ph::CARET_DOWN } else { ph::CARET_RIGHT };
                    static_icon(ui, Rect::from_center_size(pos2(hr.left() + 8.0, hr.center().y), vec2(12.0, 12.0)), caret, 12.0, false, t::TEXT_TERTIARY);
                    kit::text(ui, pos2(hr.left() + 20.0, hr.center().y), Align2::LEFT_CENTER, &format!("Выполнено · {}", done.len()), t::regular(12.0), t::TEXT_SECONDARY);
                    let clear_r = Rect::from_min_size(pos2(hr.right() - 92.0, hr.top()), vec2(92.0, 28.0));
                    if kit::button(
                        ui, clear_r, Id::new("notes_clear_done"),
                        Content::IconText { glyph: ph::BROOM, size: 13.0, fill: false, label: "Очистить", font: t::regular(12.0), gap: 5.0 },
                        ButtonStyle::ghost(t::TEXT_TERTIARY, t::RADIUS_CTRL), true,
                    )
                    .on_hover_text("Удалить выполненные из этого списка")
                    .clicked()
                    {
                        acts.push(NoteAct::ClearDone(done.iter().map(|&(i, _)| self.state.notes[i].id).collect()));
                    }
                    y += 28.0 + 4.0;
                    if self.notes_done_open {
                        for &(i, h) in &done {
                            let row = Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), h));
                            self.draw_note_row(ui, row, i, accent, now, folders_h, &mut acts);
                            y += h + 4.0;
                        }
                    }
                }
            });

        for act in acts {
            match act {
                NoteAct::Expand(id) => {
                    self.notes_expanded = if self.notes_expanded == Some(id) { None } else { Some(id) };
                    self.reminder_input.clear();
                    self.item_input.clear();
                }
                NoteAct::ToggleDone(id) => {
                    if let Some(i) = self.note_index(id) {
                        let done = !self.state.notes[i].done;
                        self.state.notes[i].set_done(done);
                        if done && self.note_alert.is_some_and(|(a, _)| a == id) {
                            self.note_alert = None;
                        }
                    }
                    self.save_to_disk();
                }
                NoteAct::Delete(id) => self.delete_note(id),
                NoteAct::ClearDone(ids) => {
                    self.state.notes.retain(|n| !ids.contains(&n.id));
                    if self.notes_expanded.is_some_and(|e| ids.contains(&e)) {
                        self.notes_expanded = None;
                    }
                    self.save_to_disk();
                }
            }
        }
    }

    /// Строка заметки: галочка, точка папки, название, напоминание или прогресс.
    /// Раскрытая — с карточкой редактирования под строкой.
    #[allow(clippy::too_many_arguments)]
    fn draw_note_row(&mut self, ui: &mut Ui, row: Rect, idx: usize, accent: Color32, now: NaiveDateTime, folders_h: f32, acts: &mut Vec<NoteAct>) {
        let id = self.state.notes[idx].id;
        // Карточка видна и пока раскрывается или сворачивается.
        let open_part = row.height() > 36.5;
        let expanded = self.notes_expanded == Some(id) || open_part;
        let painter = ui.painter().clone();
        let stroke = if expanded { t::tint(accent, 0x55) } else { Color32::TRANSPARENT };
        painter.rect(row, t::RADIUS_CTRL, t::SURFACE_1, Stroke::new(1.0, stroke), StrokeKind::Inside);

        let head = Rect::from_min_size(row.min, vec2(row.width(), 36.0));
        // Клик по строке раскрывает её. Регистрируется до галочки, чтобы та забирала свой клик.
        let head_resp = ui.interact(head, Id::new(("note_head", id)), Sense::click());
        let hover = ui.ctx().animate_bool_with_time(Id::new(("note_hover", id)), head_resp.hovered() && !expanded, t::ANIM_HOVER);
        if hover > 0.01 {
            painter.rect_filled(head, t::RADIUS_CTRL, t::tint(t::TEXT_PRIMARY, (hover * 10.0) as u8));
        }
        if head_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if head_resp.clicked() {
            acts.push(NoteAct::Expand(id));
        }
        if self.notes_scroll_to == Some(id) {
            head_resp.scroll_to_me(Some(egui::Align::Center));
            self.notes_scroll_to = None;
        }

        let note = &self.state.notes[idx];
        let cy = head.center().y;
        let cb = Rect::from_center_size(pos2(head.left() + 18.0, cy), vec2(17.0, 17.0));
        if checkbox(ui, cb, Id::new(("note_done", id)), note.done, accent)
            .on_hover_text(if note.done { "Вернуть в работу" } else { "Выполнено" })
            .clicked()
        {
            acts.push(NoteAct::ToggleDone(id));
        }
        let mut x = cb.right() + 9.0;
        // Точки папок (кроме открытой сейчас), не больше трёх.
        let dots: Vec<Color32> = note
            .folders
            .iter()
            .filter(|&&f| Some(f) != self.notes_filter)
            .filter_map(|fid| self.state.folders.iter().find(|f| f.id == *fid))
            .map(|f| folder_color(f.color))
            .take(3)
            .collect();
        for (k, c) in dots.iter().enumerate() {
            painter.circle_filled(pos2(x + 3.0 + k as f32 * 7.0, cy), 3.0, *c);
        }
        if !dots.is_empty() {
            x += 6.0 + 7.0 * (dots.len() - 1) as f32 + 6.0;
        }

        // Справа: напоминание или прогресс чек-листа.
        let mut right = head.right() - 10.0;
        if let Some(r) = &note.reminder {
            let color = if note.done {
                t::TEXT_TERTIARY
            } else if note.overdue() {
                t::DANGER
            } else {
                accent
            };
            let tr = kit::text(ui, pos2(right, cy), Align2::RIGHT_CENTER, &notes::format_when(r, now), t::regular(11.0), color);
            let glyph = if r.repeat == Repeat::Once { ph::BELL } else { ph::REPEAT };
            static_icon(ui, Rect::from_center_size(pos2(tr.left() - 8.0, cy), vec2(12.0, 12.0)), glyph, 11.0, false, color);
            right = tr.left() - 20.0;
        } else if let Some((d, n)) = note.progress() {
            let tr = kit::text(ui, pos2(right, cy), Align2::RIGHT_CENTER, &format!("{d}/{n}"), t::mono(11.0), t::TEXT_SECONDARY);
            right = tr.left() - 8.0;
        }

        let color = if note.done { t::TEXT_TERTIARY } else { t::TEXT_PRIMARY };
        let title = if note.title.trim().is_empty() { "Без названия" } else { note.title.as_str() };
        let tr = kit::text_trunc(ui, pos2(x, cy), Align2::LEFT_CENTER, title, t::regular(13.0), color, (right - x).max(24.0));
        if note.done {
            painter.line_segment([pos2(tr.left(), cy + 0.5), pos2(tr.right(), cy + 0.5)], Stroke::new(1.0, t::TEXT_TERTIARY));
        }
        // Полоска прогресса чек-листа под свёрнутой строкой.
        if let (Some((d, n)), false) = (note.progress(), expanded) {
            let bar = Rect::from_min_size(pos2(x, head.bottom() - 5.0), vec2(head.right() - 10.0 - x, 2.0));
            painter.rect_filled(bar, 1u8, t::LINE);
            painter.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * d as f32 / n as f32, 2.0)), 1u8, accent);
        }

        if open_part {
            // Карточка рисуется в полный рост, а видна — в пределах текущей высоты строки.
            let lay = editor_layout(ui, &self.state.notes[idx], row.width() - 20.0);
            let full = note_editor_h(&self.state.notes[idx], folders_h, &lay);
            let area = Rect::from_min_size(pos2(row.left(), head.bottom()), vec2(row.width(), full));
            let mut eui = ui.new_child(UiBuilder::new().max_rect(area).id_salt(("note_editor", id)));
            eui.set_clip_rect(row.intersect(ui.clip_rect()));
            self.draw_note_editor(&mut eui, area, idx, accent, now, &lay, acts);
        }
    }

    /// Карточка раскрытой заметки. Высоты строк должны совпадать с `note_editor_h`.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn draw_note_editor(&mut self, ui: &mut Ui, area: Rect, idx: usize, accent: Color32, now: NaiveDateTime, lay: &EditorLayout, acts: &mut Vec<NoteAct>) {
        let id = self.state.notes[idx].id;
        let left = area.left() + 10.0;
        let w = area.width() - 20.0;
        let mut y = area.top() + 4.0;
        let mut changed = false;
        let enter = |ui: &Ui, resp: &Response| resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

        {
            let note = &mut self.state.notes[idx];
            // Название и описание — растут по высоте под текст.
            let r = Rect::from_min_size(pos2(left, y), vec2(w, lay.title_h));
            changed |= text_area(ui, r, Id::new(("note_title", id)), &mut note.title, "Название", accent, t::regular(12.5), TITLE_PAD, false).changed();
            y += lay.title_h + 6.0;
            let r = Rect::from_min_size(pos2(left, y), vec2(w, lay.body_h));
            changed |= text_area(ui, r, Id::new(("note_body", id)), &mut note.body, "Описание", accent, t::regular(12.0), BODY_PAD, true).changed();
            y += lay.body_h + 6.0;

            // Пункты чек-листа: длинный текст переносится, строка растёт.
            let mut toggle = None;
            let mut remove = None;
            for (k, item) in note.items.iter_mut().enumerate() {
                let h = lay.items_h.get(k).copied().unwrap_or(26.0);
                let cy = y + 13.0;
                let cb = Rect::from_center_size(pos2(left + 8.0, cy), vec2(14.0, 14.0));
                if checkbox(ui, cb, Id::new(("item_cb", id, k)), item.done, accent).clicked() {
                    toggle = Some(k);
                }
                let tr = Rect::from_min_max(pos2(left + ITEM_TEXT_INSET, y + 5.0), pos2(left + w - ITEM_TEXT_RIGHT, y + h - 5.0));
                let edit = egui::TextEdit::multiline(&mut item.text)
                    .id(Id::new(("item_text", id, k)))
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::ZERO)
                    .return_key(None)
                    .font(t::regular(12.0))
                    .text_color(if item.done { t::TEXT_TERTIARY } else { t::TEXT_PRIMARY })
                    .desired_rows(1)
                    .desired_width(tr.width());
                changed |= ui.place(tr, edit).changed();
                if item.text.contains(['\n', '\r']) {
                    item.text = item.text.replace(['\r', '\n'], " ");
                }
                let xr = Rect::from_center_size(pos2(left + w - 11.0, cy), vec2(22.0, 22.0));
                if kit::button(ui, xr, Id::new(("item_x", id, k)), Content::Icon { glyph: ph::X, size: 12.0, fill: false }, ButtonStyle::ghost(t::TEXT_TERTIARY, t::RADIUS_CHIP), true)
                    .on_hover_text("Удалить пункт")
                    .clicked()
                {
                    remove = Some(k);
                }
                y += h;
            }
            if let Some(k) = toggle {
                note.items[k].done = !note.items[k].done;
                changed = true;
            }
            if let Some(k) = remove {
                note.items.remove(k);
                changed = true;
            }
        }

        // Новый пункт: Enter добавляет и оставляет курсор в поле.
        let r = Rect::from_min_size(pos2(left, y), vec2(w, 26.0));
        let resp = text_field(ui, r, Id::new(("item_add", id)), &mut self.item_input, "+ Пункт чек-листа (Enter)", accent);
        if enter(ui, &resp) {
            let text = self.item_input.trim().to_string();
            if !text.is_empty() {
                self.state.notes[idx].items.push(NoteItem { text, done: false });
                self.item_input.clear();
                changed = true;
                resp.request_focus();
            }
        }
        y += 26.0 + 8.0;

        // Напоминание.
        let row = Rect::from_min_size(pos2(left, y), vec2(w, 28.0));
        if let Some(r) = self.state.notes[idx].reminder.clone() {
            let color = if self.state.notes[idx].overdue() { t::DANGER } else { accent };
            let glyph = if r.repeat == Repeat::Once { ph::BELL } else { ph::REPEAT };
            static_icon(ui, Rect::from_center_size(pos2(left + 8.0, row.center().y), vec2(14.0, 14.0)), glyph, 14.0, false, color);
            let mut label = notes::format_when(&r, now);
            if self.state.notes[idx].overdue() {
                label.push_str(" · просрочено");
            }
            kit::text(ui, pos2(left + 22.0, row.center().y), Align2::LEFT_CENTER, &label, t::regular(12.0), color);
            let xr = Rect::from_min_size(pos2(row.right() - 72.0, row.top()), vec2(72.0, 28.0));
            if kit::button(ui, xr, Id::new(("rem_clear", id)), Content::Text { label: "Убрать", font: t::regular(12.0) }, ButtonStyle::ghost(t::TEXT_SECONDARY, t::RADIUS_CTRL), true).clicked() {
                self.state.notes[idx].reminder = None;
                changed = true;
            }
            y += 28.0;
        } else {
            let failed = self.reminder_error.is_some_and(|at| at.elapsed() < Duration::from_secs(3));
            if failed {
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
            let hint = if failed { "Не понял — например «завтра в 9»" } else { "Напомнить: завтра в 9, через 20 минут…" };
            let resp = text_field(ui, row, Id::new(("rem_input", id)), &mut self.reminder_input, hint, if failed { t::DANGER } else { accent });
            if enter(ui, &resp) && !self.reminder_input.trim().is_empty() {
                match notes::parse_when_text(&self.reminder_input, now) {
                    Some(when) => {
                        self.state.notes[idx].reminder = Some(Reminder::new(when.at, when.repeat));
                        self.reminder_input.clear();
                        self.reminder_error = None;
                        changed = true;
                    }
                    None => {
                        self.reminder_input.clear();
                        self.reminder_error = Some(Instant::now());
                        resp.request_focus();
                    }
                }
            }
            y += 28.0 + 4.0;
            const QUICK: [(&str, &str); 3] = [("Через 1 ч", "через час"), ("Вечером", "вечером"), ("Завтра 9:00", "завтра в 9")];
            let qw = (w - 12.0) / 3.0;
            for (k, (label, phrase)) in QUICK.into_iter().enumerate() {
                let r = Rect::from_min_size(pos2(left + k as f32 * (qw + 6.0), y), vec2(qw, 24.0));
                if kit::button(ui, r, Id::new(("rem_quick", id, k)), Content::Text { label, font: t::regular(11.0) }, ButtonStyle::chip(), true).clicked() {
                    if let Some(when) = notes::parse_when_text(phrase, now) {
                        self.state.notes[idx].reminder = Some(Reminder::new(when.at, when.repeat));
                        changed = true;
                    }
                }
            }
            y += 24.0;
        }
        y += 8.0;

        // Папки: клик добавляет или убирает папку, «Без папки» снимает все.
        let options = self.editor_folder_options();
        let widths: Vec<f32> = options.iter().map(|(_, name, dot)| chip_width(ui, name, dot.is_some(), 11.0)).collect();
        let (pos, rows) = flow(&widths, w, 4.0);
        for (k, (fid, name, dot)) in options.iter().enumerate() {
            let (fx, row) = pos[k];
            let r = Rect::from_min_size(pos2(left + fx, y + row as f32 * 28.0), vec2(widths[k], 24.0));
            let note = &self.state.notes[idx];
            let selected = match fid {
                None => note.folders.is_empty(),
                Some(f) => note.in_folder(*f),
            };
            if folder_chip(ui, r, Id::new(("note_folder", id, k)), name, *dot, selected, accent, 11.0).clicked() {
                let note = &mut self.state.notes[idx];
                match fid {
                    None => note.folders.clear(),
                    Some(f) => note.toggle_folder(*f),
                }
                changed = true;
            }
        }
        y += rows as f32 * 28.0 - 4.0 + 8.0;

        // Низ: дата создания и «Удалить».
        let created = self.state.notes[idx].created;
        let date = format!("Создано {} {}", created.day(), MONTHS_GENITIVE[created.month0() as usize]);
        kit::text(ui, pos2(left, y + 13.0), Align2::LEFT_CENTER, &date, t::regular(11.0), t::TEXT_TERTIARY);
        let del = Rect::from_min_size(pos2(left + w - 92.0, y), vec2(92.0, 26.0));
        if kit::button(
            ui, del, Id::new(("note_del", id)),
            Content::IconText { glyph: ph::TRASH, size: 13.0, fill: false, label: "Удалить", font: t::regular(12.0), gap: 5.0 },
            ButtonStyle::danger_ghost(t::RADIUS_CTRL), true,
        )
        .clicked()
        {
            acts.push(NoteAct::Delete(id));
        }

        if changed {
            self.mark_notes_dirty();
        }
    }

    /// Поле «Новая заметка» внизу панели. Фраза с «напомни» разбирается
    /// так же, как голосом: «позвонить маме напомни завтра в 10».
    fn draw_new_note(&mut self, ui: &mut Ui, r: Rect, accent: Color32) {
        let field = Rect::from_min_max(r.min, pos2(r.right() - 36.0 - 6.0, r.bottom()));
        let resp = text_field(ui, field, Id::new("notes_new"), &mut self.notes_new, "Новая заметка…", accent);
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let add_r = Rect::from_min_size(pos2(r.right() - 36.0, r.top()), vec2(36.0, 36.0));
        let click = kit::button(ui, add_r, Id::new("notes_add"), Content::Icon { glyph: ph::PLUS, size: 18.0, fill: false }, ButtonStyle::accent(accent, t::RADIUS_CTRL), true)
            .on_hover_text("Добавить заметку")
            .clicked();
        let text = self.notes_new.trim().to_string();
        if (enter || click) && !text.is_empty() {
            let mut folders: Vec<u64> = self.notes_filter.into_iter().collect();
            let mut title = text.clone();
            let mut reminder = None;
            if text.to_lowercase().contains("напом") {
                if let Some(VoiceCommand::AddNote { title: t2, folders: f2, reminder: r2 }) = crate::voice::parse_command_at(&text, notes::now()) {
                    title = t2;
                    reminder = r2;
                    for f in f2 {
                        let fid = self.folder_by_name(&f);
                        if !folders.contains(&fid) {
                            folders.push(fid);
                        }
                    }
                }
            }
            self.add_note(title, folders, reminder);
            self.notes_new.clear();
            if enter {
                resp.request_focus();
            }
        }
    }

    fn draw_menu(&mut self, ui: &mut Ui, rect: Rect, accent: Color32) {
        kit::paint_block(ui.painter(), rect, t::BLOCK_STROKE);

        // Переключатель вкладок: трек 326×36, четыре вкладки по 78,5 px,
        // подложка активной вкладки едет по x с шагом 80,5.
        let track = Rect::from_min_size(rect.min + vec2(t::BLOCK_PAD, t::BLOCK_PAD), vec2(rect.width() - 2.0 * t::BLOCK_PAD, 36.0));
        ui.painter().rect_filled(track, t::RADIUS_BTN_LG, t::SURFACE_SUNKEN);
        let tab_w = (track.width() - 6.0 - 6.0) / 4.0;
        let slide = ui.ctx().animate_value_with_time(Id::new("tab_slide"), self.tab.index() as f32, t::ANIM_TAB);
        let pill = Rect::from_min_size(pos2(track.left() + 3.0 + slide * (tab_w + 2.0), track.top() + 3.0), vec2(tab_w, 30.0));
        ui.painter().rect_filled(pill, t::RADIUS_CTRL, t::SURFACE_3);
        let tabs = [
            (Tab::Timers, "Таймеры", ph::HOURGLASS_MEDIUM),
            (Tab::Calendar, "История", ph::CALENDAR_BLANK),
            (Tab::Settings, "Опции", ph::SLIDERS_HORIZONTAL),
            (Tab::System, "Система", ph::KEYBOARD),
        ];
        for (i, (tab, label, glyph)) in tabs.into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(track.left() + 3.0 + i as f32 * (tab_w + 2.0), track.top() + 3.0), vec2(tab_w, 30.0));
            let resp = ui.interact(r, Id::new(("tab", i)), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let it = kit::interaction(ui, &resp, true);
            let active = self.tab == tab;
            let color = if active { t::TEXT_PRIMARY } else { kit::lerp(t::TEXT_SECONDARY, t::TEXT_PRIMARY, it.hover) };
            // v2: иконка 14 и подпись 12, зазор 5.
            let ig = kit::galley(ui, glyph, t::icon(14.0, false), color);
            let tg = kit::galley(ui, label, t::medium(12.0), color);
            let total = ig.size().x + 5.0 + tg.size().x;
            let x0 = r.center().x - total / 2.0;
            let ix = ig.size().x;
            kit::paint_galley(ui.painter(), pos2(x0, r.center().y), Align2::LEFT_CENTER, ig, color);
            kit::paint_galley(ui.painter(), pos2(x0 + ix + 5.0, r.center().y), Align2::LEFT_CENTER, tg, color);
            if resp.clicked() && !active {
                self.tab = tab;
                self.tab_changed_at = Instant::now();
                self.accent_picker_open = false;
                self.cancel_hotkey_edit();
            }
        }

        // Содержимое вкладки: alpha 0 → 1 и сдвиг 4 → 0 при смене вкладки.
        let fade = kit::tween(self.tab_changed_at, t::ANIM_TAB);
        let area = Rect::from_min_max(
            pos2(rect.left() + t::BLOCK_PAD, track.bottom() + 12.0),
            pos2(rect.right() - t::BLOCK_PAD, rect.bottom() - t::BLOCK_PAD),
        );
        let shifted = area.translate(vec2(0.0, 4.0 * (1.0 - fade)));
        let mut content = ui.new_child(UiBuilder::new().max_rect(shifted).id_salt("menu_content"));
        content.multiply_opacity(fade);
        content.set_clip_rect(Rect::from_min_max(pos2(rect.left(), track.bottom()), rect.max).intersect(ui.clip_rect()));
        match self.tab {
            Tab::Timers => self.draw_timers_tab(&mut content, shifted, accent),
            Tab::Calendar => self.draw_history_tab(&mut content, shifted, accent),
            Tab::Settings => self.draw_options_tab(&mut content, shifted, accent),
            Tab::System => self.draw_system_tab(&mut content, shifted, accent),
        }
    }

    fn draw_timers_tab(&mut self, ui: &mut Ui, area: Rect, accent: Color32) {
        let left = area.left();
        let w = area.width();
        let mut y = area.top();

        // Название (необязательно).
        let name_id = Id::new("new_timer_name");
        let name_r = Rect::from_min_size(pos2(left, y), vec2(w, 36.0));
        let name_focused = ui.memory(|m| m.has_focus(name_id));
        ui.painter().rect(
            name_r, t::RADIUS_CTRL, t::SURFACE_2,
            Stroke::new(1.0, if name_focused { accent } else { t::BLOCK_STROKE }),
            StrokeKind::Inside,
        );
        let edit = egui::TextEdit::singleline(&mut self.new_name)
            .id(name_id)
            .frame(egui::Frame::NONE)
            .hint_text(egui::RichText::new("Название (необязательно)").color(t::TEXT_TERTIARY))
            .font(t::regular(13.0))
            .text_color(t::TEXT_PRIMARY)
            .vertical_align(egui::Align::Center)
            .desired_width(w - 22.0);
        ui.place(name_r.shrink2(vec2(11.0, 0.0)), edit);
        y += 36.0 + 8.0;

        // Поля Ч / М / С: 3 × 100.67×52, разделитель «:» шириной 12.
        let field_w = (w - 24.0) / 3.0;
        let fields: [(&str, u64); 3] = [("ч", 99), ("мин", 59), ("сек", 59)];
        for (i, (unit, max)) in fields.into_iter().enumerate() {
            let x = left + i as f32 * (field_w + 12.0);
            let r = Rect::from_min_size(pos2(x, y), vec2(field_w, 52.0));
            let value = match i { 0 => &mut self.new_h, 1 => &mut self.new_m, _ => &mut self.new_s };
            kit::hms_field(ui, r, Id::new(("hms", i)), value, max, unit, accent);
            if i < 2 {
                kit::text(ui, pos2(r.right() + 6.0, r.center().y), Align2::CENTER_CENTER, ":", t::mono(18.0), t::TEXT_DISABLED);
            }
        }
        y += 52.0 + 8.0;

        // Пресеты: клик сразу создаёт и запускает таймер.
        let chip_w = (w - 18.0) / 4.0;
        for (i, (mins, name)) in PRESETS.into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(left + i as f32 * (chip_w + 6.0), y), vec2(chip_w, 28.0));
            let label = format!("{} мин", mins);
            if kit::button(ui, r, Id::new(("preset", i)), Content::Text { label: &label, font: t::regular(12.0) }, ButtonStyle::chip(), true).clicked() {
                self.add_timer(name.to_string(), TimerMode::Timer(Duration::from_secs(mins * 60)), true);
            }
        }
        y += 28.0 + 8.0;

        // + Таймер / + Секундомер.
        let total_secs = self.new_h * 3600 + self.new_m * 60 + self.new_s;
        let half = (w - 8.0) / 2.0;
        let add_r = Rect::from_min_size(pos2(left, y), vec2(half, 36.0));
        let sw_r = Rect::from_min_size(pos2(left + half + 8.0, y), vec2(half, 36.0));
        let add = kit::button(
            ui, add_r, Id::new("add_timer"),
            Content::IconText { glyph: ph::PLUS, size: 16.0, fill: false, label: "Таймер", font: t::medium(13.0), gap: 6.0 },
            ButtonStyle::accent_outline(accent, t::RADIUS_CTRL), total_secs > 0,
        );
        if total_secs == 0 {
            add.on_hover_text("Задайте ненулевое время");
        } else if add.clicked() {
            let name = if self.new_name.is_empty() { "Таймер".into() } else { self.new_name.clone() };
            self.add_timer(name, TimerMode::Timer(Duration::from_secs(total_secs)), false);
        }
        if kit::button(
            ui, sw_r, Id::new("add_stopwatch"),
            Content::IconText { glyph: ph::PLUS, size: 16.0, fill: false, label: "Секундомер", font: t::medium(13.0), gap: 6.0 },
            ButtonStyle::secondary_bright(t::RADIUS_CTRL), true,
        ).clicked() {
            let name = if self.new_name.is_empty() { "Секундомер".into() } else { self.new_name.clone() };
            self.add_timer(name, TimerMode::Stopwatch, false);
        }
        y += 36.0 + 12.0;

        // АКТИВНЫЕ · N
        let alive = self.state.active_timers.iter().filter(|tm| !self.row_dying.contains_key(&tm.id)).count();
        kit::overline(ui, pos2(left, y + 14.0), &format!("Активные · {}", alive));
        y += 28.0;

        let list_rect = Rect::from_min_max(pos2(left, y), area.max);
        if self.state.active_timers.is_empty() {
            let cy = list_rect.center().y - 20.0;
            ui.painter().circle_filled(pos2(list_rect.center().x, cy - 33.0), 24.0, t::SURFACE_1);
            static_icon(ui, Rect::from_center_size(pos2(list_rect.center().x, cy - 33.0), vec2(48.0, 48.0)), ph::HOURGLASS_MEDIUM, 24.0, false, t::TEXT_TERTIARY);
            kit::text(ui, pos2(list_rect.center().x, cy + 8.0), Align2::CENTER_CENTER, "Нет таймеров", t::medium(13.0), t::TEXT_PRIMARY);
            let g = ui.painter().layout(
                "Задайте время выше или выберите пресет — таймер появится здесь".to_owned(),
                t::regular(12.0), t::TEXT_TERTIARY, 220.0,
            );
            let mut job = (*g.job).clone();
            job.halign = egui::Align::Center;
            let g = ui.painter().layout_job(job);
            kit::paint_galley_centered(ui.painter(), pos2(list_rect.center().x, cy + 26.0), g, t::TEXT_TERTIARY);
            return;
        }

        // Строки: появление 0 → 44 и alpha 0 → 1 за 180 мс; удаление — alpha за 120 мс,
        // затем высота 44 → 0 за 150 мс.
        struct RowView {
            id: u64,
            name: String,
            time: String,
            stopwatch: bool,
            running: bool,
            finished: bool,
            pinned: bool,
            alpha: f32,
            height: f32,
            dying: bool,
        }
        let pinned_shown = self.timer_index(self.pinned_timer_id).map(|_| self.pinned_timer_id)
            .or_else(|| self.state.active_timers.first().map(|t| t.id));
        let rows: Vec<RowView> = self.state.active_timers.iter().map(|tm| {
            let (alpha, height, dying) = if let Some(at) = self.row_dying.get(&tm.id) {
                let s = at.elapsed().as_secs_f32();
                let a = 1.0 - ease_out(s / t::ANIM_ROW_FADE);
                let h = 1.0 - ease_out((s - t::ANIM_ROW_FADE).max(0.0) / t::ANIM_ROW_COLLAPSE);
                (a, h, true)
            } else if let Some(at) = self.row_born.get(&tm.id) {
                let k = kit::tween(*at, t::ANIM_ROW_ADD);
                (k, k, false)
            } else {
                (1.0, 1.0, false)
            };
            RowView {
                id: tm.id,
                name: tm.name.clone(),
                time: fmt_hms(tm.time_left().as_secs()),
                stopwatch: matches!(tm.mode, TimerMode::Stopwatch),
                running: tm.is_running(),
                finished: tm.is_finished(),
                pinned: pinned_shown == Some(tm.id),
                alpha,
                height,
                dying,
            }
        }).collect();
        self.row_born.retain(|_, at| at.elapsed().as_secs_f32() < t::ANIM_ROW_ADD);

        let content_h: f32 = rows.iter().map(|r| (44.0 + 4.0) * r.height).sum();
        let overflow = content_h > list_rect.height() + 0.5;
        let mut actions: Vec<RowAction> = Vec::new();
        let mut list_ui = ui.new_child(UiBuilder::new().max_rect(list_rect).id_salt("timers_list"));
        egui::ScrollArea::vertical()
            .id_salt("t_scroll")
            .auto_shrink([false, false])
            .max_height(list_rect.height())
            .show(&mut list_ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let row_w = ui.available_width() - if overflow { 7.0 } else { 0.0 };
                for r in &rows {
                    let (slot, _) = ui.allocate_exact_size(vec2(ui.available_width(), (44.0 + 4.0) * r.height), Sense::hover());
                    let h = 44.0 * r.height;
                    if h < 0.5 {
                        continue;
                    }
                    let full = Rect::from_min_size(slot.min, vec2(row_w, 44.0));
                    let mut row_ui = ui.new_child(UiBuilder::new().max_rect(full).id_salt(("row", r.id)));
                    row_ui.multiply_opacity(r.alpha);
                    row_ui.set_clip_rect(Rect::from_min_size(slot.min, vec2(row_w, h)).intersect(ui.clip_rect()));
                    let painter = row_ui.painter().clone();
                    painter.rect_filled(full, t::RADIUS_CTRL, t::SURFACE_1);
                    let cy = full.center().y;
                    let enabled = !r.dying;

                    let pin_r = Rect::from_min_size(pos2(full.left() + 4.0, cy - 14.0), vec2(28.0, 28.0));
                    let trash_r = Rect::from_min_size(pos2(full.right() - 4.0 - 28.0, cy - 14.0), vec2(28.0, 28.0));
                    let stop_r = trash_r.translate(vec2(-32.0, 0.0));
                    let play_r = stop_r.translate(vec2(-32.0, 0.0));

                    let pin_color = if r.pinned { accent } else { t::TEXT_TERTIARY };
                    let pin_style = ButtonStyle { hover_color: if r.pinned { accent } else { t::TEXT_PRIMARY }, ..ButtonStyle::ghost(pin_color, t::RADIUS_CTRL) };
                    if kit::button(&row_ui, pin_r, Id::new(("pin", r.id)), Content::Icon { glyph: ph::PUSH_PIN, size: 16.0, fill: r.pinned }, pin_style, enabled)
                        .on_hover_text("Показывать в виджете")
                        .clicked()
                    {
                        actions.push(RowAction::Pin(r.id));
                    }

                    let play_style = if r.running { ButtonStyle::accent(accent, t::RADIUS_CTRL) } else { ButtonStyle::secondary_bright(t::RADIUS_CTRL) };
                    let glyph = if r.running { ph::PAUSE } else { ph::PLAY };
                    if kit::button(&row_ui, play_r, Id::new(("play", r.id)), Content::Icon { glyph, size: 14.0, fill: true }, play_style, enabled).clicked() {
                        actions.push(RowAction::Toggle(r.id));
                    }
                    if kit::button(&row_ui, stop_r, Id::new(("stop", r.id)), Content::Icon { glyph: ph::STOP, size: 14.0, fill: true }, ButtonStyle::ghost(t::TEXT_SECONDARY, t::RADIUS_CTRL), enabled)
                        .on_hover_text("Остановить и записать в историю")
                        .clicked()
                    {
                        actions.push(RowAction::Stop(r.id));
                    }
                    if kit::button(&row_ui, trash_r, Id::new(("trash", r.id)), Content::Icon { glyph: ph::TRASH, size: 14.0, fill: false }, ButtonStyle::danger_ghost(t::RADIUS_CTRL), enabled)
                        .on_hover_text("Удалить без записи")
                        .clicked()
                    {
                        actions.push(RowAction::Remove(r.id));
                    }

                    let tx = pin_r.right() + 4.0;
                    let tw = play_r.left() - 4.0 - tx;
                    let top = full.top() + 4.0;
                    static_icon(&row_ui, Rect::from_center_size(pos2(tx + 5.5, top + 8.0), vec2(11.0, 16.0)),
                        if r.stopwatch { ph::TIMER } else { ph::HOURGLASS_MEDIUM }, 11.0, false, t::TEXT_SECONDARY);
                    kit::text_trunc(&row_ui, pos2(tx + 15.0, top + 8.0), Align2::LEFT_CENTER, &r.name, t::regular(12.0), t::TEXT_SECONDARY, tw - 15.0);
                    kit::text(&row_ui, pos2(tx, top + 16.0 + 10.0), Align2::LEFT_CENTER, &r.time, t::mono_medium(15.0),
                        if r.finished { t::DANGER } else { t::TEXT_PRIMARY });
                }
            });

        for action in actions {
            match action {
                RowAction::Pin(id) => {
                    self.pinned_timer_id = id;
                    self.save_to_disk();
                }
                RowAction::Toggle(id) => self.toggle_timer(id),
                RowAction::Stop(id) => self.remove_timer(id, true),
                RowAction::Remove(id) => self.remove_timer(id, false),
            }
        }
    }

    fn draw_history_tab(&mut self, ui: &mut Ui, area: Rect, accent: Color32) {
        let left = area.left();
        let w = area.width();
        let mut y = area.top();
        let today = Local::now().date_naive();
        let month = self.cal_month;
        let on_accent = t::on_accent(accent);

        // Заголовок месяца и стрелки.
        kit::text(ui, pos2(left, y + 14.0), Align2::LEFT_CENTER,
            &format!("{} {}", MONTHS_RU[(month.month() - 1) as usize], month.year()), t::medium(15.0), t::TEXT_PRIMARY);
        let next_r = Rect::from_min_size(pos2(area.right() - 28.0, y), vec2(28.0, 28.0));
        let prev_r = next_r.translate(vec2(-32.0, 0.0));
        if kit::button(ui, prev_r, Id::new("cal_prev"), Content::Icon { glyph: ph::CARET_LEFT, size: 16.0, fill: false }, ButtonStyle::ghost(t::TEXT_SECONDARY, t::RADIUS_CTRL), true).clicked() {
            self.cal_month = shift_month(month, -1);
        }
        if kit::button(ui, next_r, Id::new("cal_next"), Content::Icon { glyph: ph::CARET_RIGHT, size: 16.0, fill: false }, ButtonStyle::ghost(t::TEXT_SECONDARY, t::RADIUS_CTRL), true).clicked() {
            self.cal_month = shift_month(month, 1);
        }
        y += 28.0 + 8.0;

        // Дни недели и сетка: ячейки 44×30, зазор 3.
        let cell_w = (w - 18.0) / 7.0;
        for (i, d) in WEEKDAYS.iter().enumerate() {
            kit::text(ui, pos2(left + i as f32 * (cell_w + 3.0) + cell_w / 2.0, y + 10.0), Align2::CENTER_CENTER, d, t::regular(11.0), t::TEXT_TERTIARY);
        }
        y += 20.0 + 4.0;
        let offset = month.weekday().num_days_from_monday() as i32;
        let rows = calendar_rows(month);
        for i in 0..(rows * 7) as i32 {
            let day_num = i - offset + 1;
            // from_ymd_opt возвращает None для 31 апреля или 30 февраля.
            // Раньше здесь стоял .unwrap() и программа падала при открытии
            // истории в любом месяце короче 31 дня.
            let date = if day_num > 0 {
                NaiveDate::from_ymd_opt(month.year(), month.month(), day_num as u32)
            } else {
                None
            };
            let Some(date) = date else { continue };
            let col = (i % 7) as f32;
            let row = (i / 7) as f32;
            let r = Rect::from_min_size(pos2(left + col * (cell_w + 3.0), y + row * 33.0), vec2(cell_w, 30.0));
            let resp = ui.interact(r, Id::new(("day", date.num_days_from_ce())), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let it = kit::interaction(ui, &resp, true);
            let selected = self.selected_date == date;
            let is_today = date == today;
            let has_data = self.state.history.iter().any(|h| h.date.date_naive() == date);
            if selected {
                ui.painter().rect_filled(r, t::RADIUS_CTRL, accent);
            } else {
                ui.painter().rect_filled(r, t::RADIUS_CTRL, kit::lerp(Color32::TRANSPARENT, t::SURFACE_2, it.hover));
                if is_today {
                    ui.painter().rect_stroke(r, t::RADIUS_CTRL, Stroke::new(1.0, accent), StrokeKind::Inside);
                }
            }
            let color = if selected { on_accent } else if date > today { t::TEXT_TERTIARY } else { t::TEXT_PRIMARY };
            let font = if selected || is_today { t::medium(13.0) } else { t::regular(13.0) };
            kit::text(ui, pos2(r.center().x, r.center().y - 2.0), Align2::CENTER_CENTER, &day_num.to_string(), font, color);
            if has_data {
                ui.painter().circle_filled(pos2(r.center().x, r.bottom() - 6.0), 2.0, if selected { on_accent } else { accent });
            }
            if resp.clicked() {
                self.selected_date = date;
            }
        }
        y += rows as f32 * 33.0 - 3.0 + 12.0;

        // Итоги за день и за месяц.
        let day_total: u64 = self.state.history.iter()
            .filter(|h| h.date.date_naive() == self.selected_date)
            .map(|h| h.duration_secs).sum();
        let month_total: u64 = self.state.history.iter()
            .filter(|h| {
                let d = h.date.date_naive();
                d.year() == month.year() && d.month() == month.month()
            })
            .map(|h| h.duration_secs).sum();
        let day_label = format!("{} {}", self.selected_date.day(), MONTHS_GENITIVE[(self.selected_date.month() - 1) as usize]);
        let half = (w - 8.0) / 2.0;
        let totals = [
            (format!("За {}", day_label), fmt_human(day_total)),
            (format!("За {}", MONTHS_RU[(month.month() - 1) as usize].to_lowercase()), fmt_human(month_total)),
        ];
        for (i, (caption, value)) in totals.iter().enumerate() {
            let r = Rect::from_min_size(pos2(left + i as f32 * (half + 8.0), y), vec2(half, 44.0));
            ui.painter().rect_filled(r, t::RADIUS_CTRL, t::SURFACE_1);
            kit::text_trunc(ui, pos2(r.left() + 11.0, r.top() + 4.0 + 7.0), Align2::LEFT_CENTER, caption, t::regular(11.0), t::TEXT_TERTIARY, half - 22.0);
            kit::text(ui, pos2(r.left() + 11.0, r.top() + 4.0 + 16.0 + 10.0), Align2::LEFT_CENTER, value, t::medium(15.0), t::TEXT_PRIMARY);
        }
        y += 44.0 + 8.0;

        let selected = self.selected_date;
        let indices: Vec<usize> = self.state.history.iter().enumerate()
            .filter(|(_, h)| h.date.date_naive() == selected)
            .map(|(i, _)| i)
            .collect();
        kit::overline(ui, pos2(left, y + 14.0), &format!("Записи · {}", indices.len()));
        y += 28.0;
        let list_rect = Rect::from_min_max(pos2(left, y), area.max);

        if indices.is_empty() {
            let cy = list_rect.center().y - 10.0;
            static_icon(ui, Rect::from_center_size(pos2(list_rect.center().x, cy - 24.0), vec2(24.0, 24.0)), ph::CALENDAR_X, 24.0, false, t::TEXT_TERTIARY);
            kit::text(ui, pos2(list_rect.center().x, cy), Align2::CENTER_CENTER, &format!("Нет записей за {}", day_label), t::medium(13.0), t::TEXT_PRIMARY);
            // «■» берём из Phosphor: такого символа может не быть в текстовом шрифте.
            let fmt = TextFormat { font_id: t::regular(12.0), color: t::TEXT_TERTIARY, ..Default::default() };
            let mut job = LayoutJob::default();
            job.append("Остановите таймер кнопкой ", 0.0, fmt.clone());
            job.append(ph::STOP, 0.0, TextFormat { font_id: t::icon(11.0, true), ..fmt.clone() });
            job.append(", чтобы записать сессию", 0.0, fmt);
            job.wrap.max_width = w;
            job.halign = egui::Align::Center;
            let g = ui.painter().layout_job(job);
            kit::paint_galley_centered(ui.painter(), pos2(list_rect.center().x, cy + 12.0), g, t::TEXT_TERTIARY);
            return;
        }

        let mut to_rem = None;
        let mut dirty = false;
        let mut list_ui = ui.new_child(UiBuilder::new().max_rect(list_rect).id_salt("history_list"));
        let content_h = indices.len() as f32 * (58.0 + 4.0);
        let overflow = content_h > list_rect.height() + 0.5;
        egui::ScrollArea::vertical()
            .id_salt("h_scroll")
            .auto_shrink([false, false])
            .max_height(list_rect.height())
            .show(&mut list_ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let row_w = ui.available_width() - if overflow { 7.0 } else { 0.0 };
                for i in indices {
                    let (slot, _) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0 + 4.0), Sense::hover());
                    let r = Rect::from_min_size(slot.min, vec2(row_w, 58.0));
                    ui.painter().rect_filled(r, t::RADIUS_CTRL, t::SURFACE_1);
                    let entry = &mut self.state.history[i];

                    // Строка 1: название, длительность, корзина.
                    let row1_cy = r.top() + 6.0 + 12.0;
                    let trash_r = Rect::from_center_size(pos2(r.right() - 4.0 - 12.0, row1_cy), vec2(24.0, 24.0));
                    if kit::button(ui, trash_r, Id::new(("entry_trash", i, entry.id)), Content::Icon { glyph: ph::TRASH, size: 14.0, fill: false }, ButtonStyle::danger_ghost(t::RADIUS_CHIP), true)
                        .on_hover_text("Удалить запись")
                        .clicked()
                    {
                        to_rem = Some(i);
                    }
                    // ВОЗВРАЩЕНЫ ЧАСЫ: format!("{:02}:{:02}:{:02}", h, m, s)
                    let dur_rect = kit::text(ui, pos2(trash_r.left() - 8.0, row1_cy), Align2::RIGHT_CENTER, &fmt_hms(entry.duration_secs), t::mono(12.0), t::TEXT_SECONDARY);
                    let name_left = r.left() + 11.0;
                    kit::text_trunc(ui, pos2(name_left, row1_cy), Align2::LEFT_CENTER, &entry.mode, t::medium(13.0), t::TEXT_PRIMARY, dur_rect.left() - 8.0 - name_left);

                    // Строка 2: заметка — поле без рамки, подложка surface.2 только в фокусе.
                    let row2_cy = r.top() + 6.0 + 24.0 + 2.0 + 10.0;
                    let note_id = Id::new(("note", i, entry.id));
                    let focused = ui.memory(|m| m.has_focus(note_id));
                    let note_color = if entry.comment.is_empty() { t::TEXT_DISABLED } else { t::TEXT_SECONDARY };
                    static_icon(ui, Rect::from_center_size(pos2(name_left + 6.0, row2_cy), vec2(12.0, 20.0)), ph::NOTE_PENCIL, 12.0, false,
                        if focused { t::TEXT_SECONDARY } else { note_color });
                    let field = Rect::from_min_max(pos2(name_left + 18.0, row2_cy - 10.0), pos2(r.right() - 4.0, row2_cy + 10.0));
                    if focused {
                        ui.painter().rect_filled(field.expand2(vec2(2.0, 0.0)), t::RADIUS_CHIP, t::SURFACE_2);
                    }
                    let edit = egui::TextEdit::singleline(&mut entry.comment)
                        .id(note_id)
                        .frame(egui::Frame::NONE)
                        .hint_text(egui::RichText::new("Заметка").color(t::TEXT_DISABLED))
                        .font(t::regular(12.0))
                        .text_color(t::TEXT_SECONDARY)
                        .vertical_align(egui::Align::Center)
                        .desired_width(field.width() - 8.0);
                    let resp = ui.place(field.shrink2(vec2(4.0, 0.0)), edit);
                    // Enter или потеря фокуса — сохранить.
                    if resp.lost_focus() {
                        dirty = true;
                    }
                }
            });
        if let Some(i) = to_rem {
            self.state.history.remove(i);
            dirty = true;
        }
        if dirty {
            self.save_to_disk();
        }
    }

    // ------------------------------------------------------------ горячие клавиши

    fn start_hotkey_recording(&mut self, action: Action) {
        // Пока пишем новое сочетание, глобальные сняты — иначе Windows перехватила
        // бы уже назначенные сочетания и они не дошли бы до окна.
        self.hotkeys.clear();
        self.hotkey_edit = Some(HotkeyEdit::Recording { action });
        system::capture::set_active(true);
    }

    fn cancel_hotkey_edit(&mut self) {
        if self.hotkey_edit.take().is_some() {
            self.apply_hotkeys();
        }
    }

    /// Назначает сочетание и проверяет, дала ли его Windows.
    fn commit_hotkey(&mut self, action: Action, combo: Combo) {
        let previous = self.config.hotkeys.get(action).cloned();
        self.config.hotkeys.set(action, Some(combo.clone()));
        let failed = self.apply_hotkeys();
        if failed.contains(&action) {
            self.config.hotkeys.set(action, previous);
            self.apply_hotkeys();
            self.hotkey_edit = Some(HotkeyEdit::SystemConflict { action, combo, since: Instant::now() });
        } else {
            self.hotkey_edit = None;
        }
        self.save_to_disk();
    }

    fn finish_hotkey_recording(&mut self, action: Action, combo: Combo) {
        if let Some(other) = self.config.hotkeys.owner_of(&combo, action) {
            self.apply_hotkeys();
            self.hotkey_edit = Some(HotkeyEdit::TakenBy { action, combo, other });
        } else {
            self.commit_hotkey(action, combo);
        }
    }

    fn draw_system_tab(&mut self, ui: &mut Ui, area: Rect, accent: Color32) {
        let ctx = ui.ctx().clone();
        // Конфликт с системой держится 2,5 с, затем поле возвращается к прежнему значению.
        if let Some(HotkeyEdit::SystemConflict { since, .. }) = &self.hotkey_edit {
            let left = t::HOTKEY_SYSTEM_CONFLICT_HOLD - since.elapsed().as_secs_f32();
            if left <= 0.0 {
                self.hotkey_edit = None;
            } else {
                ctx.request_repaint_after(Duration::from_secs_f32(left));
            }
        }

        // Строка подсказки или конфликта под полем: высота 0 → 24 за 180 мс, обратно за 150.
        let msg_h: Vec<f32> = Action::ALL
            .iter()
            .enumerate()
            .map(|(i, &a)| {
                let open = self.hotkey_edit.as_ref().is_some_and(|e| e.action() == a);
                ctx.animate_value_with_time(
                    Id::new(("hk_msg", i)),
                    if open { 24.0 } else { 0.0 },
                    if open { t::ANIM_HINT_ROW_IN } else { t::ANIM_HINT_ROW_OUT },
                )
            })
            .collect();

        // Горячие клавиши (32 + 30 × 8 + подсказки + 4 + 28) · 12 · Голос · 12 · Трей (14 + 4 + 32 × 2) · 12 · Данные (14 + 4 + 32).
        let total_h = 32.0 + msg_h.iter().map(|h| 30.0 + h).sum::<f32>() + 4.0 + 28.0 + 12.0 + VOICE_GROUP_H + 12.0 + 82.0 + 12.0 + 50.0;
        let mut save = false;
        let mut list = ui.new_child(UiBuilder::new().max_rect(area).id_salt("system_tab"));
        egui::ScrollArea::vertical()
            .id_salt("sys_scroll")
            .auto_shrink([false, false])
            .max_height(area.height())
            .show(&mut list, |ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), total_h), Sense::hover());
                save |= self.draw_system_content(ui, rect, accent, &msg_h);
            });
        if save {
            self.save_to_disk();
        }
    }

    fn draw_system_content(&mut self, ui: &mut Ui, rect: Rect, accent: Color32, msg_h: &[f32]) -> bool {
        let left = rect.left();
        let w = rect.width();
        let mut y = rect.top();
        let mut save = false;

        // ГОРЯЧИЕ КЛАВИШИ — заголовок группы с общим переключателем.
        let head = Rect::from_min_size(pos2(left, y), vec2(w, 32.0));
        let head_resp = ui.interact(head, Id::new("hk_master"), Sense::click());
        if head_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if head_resp.clicked() {
            self.config.hotkeys_enabled = !self.config.hotkeys_enabled;
            self.hotkey_edit = None;
            self.apply_hotkeys();
            save = true;
        }
        kit::overline(ui, pos2(left, head.center().y), "Горячие клавиши");
        let on = ui.ctx().animate_bool_with_time(Id::new("hk_master_knob"), self.config.hotkeys_enabled, t::ANIM_TOGGLE);
        let sw = Rect::from_min_size(pos2(head.right() - 32.0, head.center().y - 9.0), vec2(32.0, 18.0));
        kit::paint_switch(ui.painter(), sw, on, accent);
        y += 32.0;

        let enabled = self.config.hotkeys_enabled;
        for (i, action) in Action::ALL.into_iter().enumerate() {
            let row = Rect::from_min_size(pos2(left, y), vec2(w, 30.0));
            save |= self.draw_hotkey_row(ui, row, i, action, accent, enabled, msg_h[i]);
            y += 30.0 + msg_h[i];
        }

        // Сбросить все по умолчанию — кнопка-призрак, без подтверждения.
        y += 4.0;
        let reset_label = "Сбросить все по умолчанию";
        let reset_w = kit::galley(ui, reset_label, t::medium(12.0), t::TEXT_SECONDARY).size().x + 14.0 + 6.0 + 14.0;
        let reset_r = Rect::from_min_size(pos2(left - 6.0, y), vec2(reset_w, 28.0));
        if kit::button(
            ui, reset_r, Id::new("hk_reset"),
            Content::IconText { glyph: ph::ARROW_COUNTER_CLOCKWISE, size: 14.0, fill: false, label: reset_label, font: t::medium(12.0), gap: 6.0 },
            ButtonStyle::ghost(t::TEXT_SECONDARY, t::RADIUS_CTRL), enabled,
        ).clicked() {
            self.config.hotkeys = system::Bindings::default();
            self.hotkey_edit = None;
            self.apply_hotkeys();
            save = true;
        }
        y += 28.0 + 12.0;

        // ГОЛОС
        save |= self.draw_voice_group(ui, left, y, w, accent);
        y += VOICE_GROUP_H + 12.0;

        // ТРЕЙ
        kit::overline(ui, pos2(left, y + 7.0), "Трей");
        y += 14.0 + 4.0;
        let tray_r = Rect::from_min_size(pos2(left, y), vec2(w, 32.0));
        save |= kit::toggle_row(ui, tray_r, Id::new("toggle_tray"), "Значок в трее", 0.0, &mut self.config.tray_enabled, accent, true);
        y += 32.0;
        let tray_on = self.config.tray_enabled;
        let close_r = Rect::from_min_size(pos2(left, y), vec2(w, 32.0));
        save |= kit::toggle_row(ui, close_r, Id::new("toggle_close_tray"), "Крестик сворачивает в трей", 12.0, &mut self.config.close_to_tray, accent, tray_on);
        y += 32.0 + 12.0;

        // ДАННЫЕ
        kit::overline(ui, pos2(left, y + 7.0), "Данные");
        y += 14.0 + 4.0;
        save |= self.draw_data_row(ui, left, y, w);
        save
    }

    /// Выпадающий список микрофонов. `Some(None)` — «по умолчанию в Windows».
    fn mic_menu(&self, button: &Response, accent: Color32) -> Option<Option<String>> {
        const W: f32 = 280.0;
        let current = self.config.voice_device.clone();
        let frame = egui::Frame::NONE
            .fill(t::SURFACE_1)
            .stroke(Stroke::new(1.0, t::LINE))
            .corner_radius(t::RADIUS_CTRL)
            .inner_margin(6.0);
        let mut chosen = None;
        let options: Vec<Option<String>> = std::iter::once(None).chain(self.voice_devices.iter().cloned().map(Some)).collect();
        egui::Popup::menu(button)
            .align(egui::RectAlign::BOTTOM_END)
            .gap(4.0)
            .width(W)
            .frame(frame)
            .show(|ui| {
                let (head, _) = ui.allocate_exact_size(vec2(W - 12.0, 22.0), Sense::hover());
                kit::overline(ui, pos2(head.left() + 6.0, head.center().y), "Микрофон");
                for option in options {
                    let (r, resp) = ui.allocate_exact_size(vec2(W - 12.0, 30.0), Sense::click());
                    let selected = option == current;
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(r, t::RADIUS_CHIP, t::SURFACE_3);
                    } else if selected {
                        ui.painter().rect_filled(r, t::RADIUS_CHIP, t::tint(accent, 0x22));
                    }
                    let label = option.clone().unwrap_or_else(|| "По умолчанию в Windows".to_string());
                    let color = if selected { accent } else { t::TEXT_PRIMARY };
                    let glyph = if option.is_some() { ph::MICROPHONE } else { ph::GEAR };
                    static_icon(ui, Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), vec2(14.0, 14.0)), glyph, 14.0, false, if selected { accent } else { t::TEXT_SECONDARY });
                    kit::text_trunc(ui, pos2(r.left() + 30.0, r.center().y), Align2::LEFT_CENTER, &label, t::regular(13.0), color, r.width() - 56.0);
                    if selected {
                        static_icon(ui, Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(14.0, 14.0)), ph::CHECK, 14.0, false, accent);
                    }
                    if resp.clicked() {
                        chosen = Some(option);
                        ui.close();
                    }
                }
            });
        chosen
    }

    /// Группа «Голос»: переключатель, состояние модели, последняя фраза и примеры.
    fn draw_voice_group(&mut self, ui: &mut Ui, left: f32, y: f32, w: f32, accent: Color32) -> bool {
        let mut y = y;
        kit::overline(ui, pos2(left, y + 7.0), "Голос");
        y += 14.0 + 4.0;
        let row = Rect::from_min_size(pos2(left, y), vec2(w, 32.0));
        let mut save = kit::toggle_row(ui, row, Id::new("toggle_voice"), "Голосовое управление «Джарвис»", 0.0, &mut self.config.voice_enabled, accent, true);
        if save && self.config.voice_enabled {
            self.voice_status = None;
        }
        y += 32.0;

        // Микрофон: выпадающий список устройств.
        kit::text(ui, pos2(left, y + 16.0), Align2::LEFT_CENTER, "Микрофон", t::regular(13.0), t::TEXT_PRIMARY);
        let shown = match (&self.config.voice_device, &self.voice_device_active) {
            (Some(name), _) => name.clone(),
            (None, Some(active)) => format!("По умолчанию · {active}"),
            (None, None) => "По умолчанию".to_string(),
        };
        let shown: String = if shown.chars().count() > 30 { shown.chars().take(29).chain(['…']).collect() } else { shown };
        let font = t::regular(12.0);
        let chip_w = kit::galley(ui, &shown, font.clone(), t::TEXT_SECONDARY).size().x + 20.0 + 14.0 + 16.0;
        let chip_r = Rect::from_min_size(pos2(left + w - chip_w, y + 3.0), vec2(chip_w, 26.0));
        let chip = kit::button(
            ui, chip_r, Id::new("voice_mic"),
            Content::IconText { glyph: ph::MICROPHONE, size: 14.0, fill: false, label: &shown, font, gap: 6.0 },
            ButtonStyle::secondary(t::RADIUS_CTRL), true,
        ).on_hover_text("Выбрать микрофон");
        // Стрелка списка поверх правого края чипа (подпись сдвинута на её место).
        static_icon(ui, Rect::from_center_size(pos2(chip_r.right() - 12.0, chip_r.center().y), vec2(12.0, 12.0)), ph::CARET_DOWN, 11.0, false, t::TEXT_TERTIARY);
        if chip.clicked() {
            // Список устройств читается только при открытии — перечисление не бесплатное.
            self.voice_devices = crate::voice::input_devices();
        }
        if let Some(choice) = self.mic_menu(&chip, accent) {
            if choice != self.config.voice_device {
                self.config.voice_device = choice;
                // Перезапуск распознавания с новым микрофоном.
                self.voice = None;
                self.voice_status = None;
                self.voice_device_active = None;
                self.voice_listening = false;
                save = true;
            }
        }
        y += 32.0;

        let (status, color) = match (&self.voice_status, self.config.voice_enabled) {
            (Some(VoiceStatus::Error(e)), _) => (format!("Ошибка: {e}"), t::DANGER),
            (_, false) => ("Выключено · распознавание офлайн".to_string(), t::TEXT_TERTIARY),
            (Some(VoiceStatus::Ready), true) if self.voice_listening => ("Слушаю команду…".to_string(), accent),
            (Some(VoiceStatus::Ready), true) => ("Готов — скажите «Джарвис» и команду".to_string(), accent),
            (_, true) => ("Загрузка модели…".to_string(), t::TEXT_SECONDARY),
        };
        kit::text_trunc(ui, pos2(left, y + 11.0), Align2::LEFT_CENTER, &status, t::regular(12.0), color, w);
        y += 22.0;

        // Уровень микрофона: если полоска не двигается, когда говоришь, — выбран не тот микрофон.
        let meter = Rect::from_min_size(pos2(left, y + 3.0), vec2(w, 4.0));
        ui.painter().rect_filled(meter, 2u8, t::SURFACE_2);
        let level = self.voice.as_ref().map(|v| v.level()).unwrap_or(0.0);
        let shown_level = ui.ctx().animate_value_with_time(Id::new("voice_level"), level.sqrt().clamp(0.0, 1.0), 0.08);
        if shown_level > 0.005 {
            let fill = Rect::from_min_size(meter.min, vec2(meter.width() * shown_level, meter.height()));
            ui.painter().rect_filled(fill, 2u8, accent);
        }
        if self.voice.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        y += 10.0;
        let heard = match &self.voice_last_heard {
            Some(h) => format!("Услышано: «{h}»"),
            None => "Услышано: —".to_string(),
        };
        kit::text_trunc(ui, pos2(left, y + 11.0), Align2::LEFT_CENTER, &heard, t::regular(12.0), t::TEXT_SECONDARY, w);
        y += 22.0;

        const EXAMPLES: [&str; 4] = [
            "«пауза» · «дальше» · «тихо» · «таймер на 25 минут работа»",
            "«заметка купить молоко» · «напомни завтра в 9 позвонить»",
            "«в заметке МИНОВ отметь контрольное» · «открой заметку отчёт»",
            "«создай папку учёба» · «положи отчёт в папку работа»",
        ];
        for line in EXAMPLES {
            kit::text_trunc(ui, pos2(left, y + 9.0), Align2::LEFT_CENTER, line, t::regular(11.0), t::TEXT_TERTIARY, w);
            y += 18.0;
        }
        save
    }

    /// Строка горячей клавиши: подпись и поле 140×26; под ней — строка
    /// подсказки или конфликта высотой `msg_h` (анимируется 0 ↔ 24).
    #[allow(clippy::too_many_arguments)]
    fn draw_hotkey_row(&mut self, ui: &mut Ui, row: Rect, i: usize, action: Action, accent: Color32, enabled: bool, msg_h: f32) -> bool {
        let mut save = false;
        let full = Rect::from_min_max(row.min, pos2(row.right(), row.bottom() + msg_h));
        let mut rui = ui.new_child(UiBuilder::new().max_rect(full).id_salt(("hk_row", i)));
        rui.set_clip_rect(full.intersect(ui.clip_rect()));
        if !enabled {
            rui.multiply_opacity(0.45);
        }

        let field = Rect::from_min_size(pos2(row.right() - 140.0, row.center().y - 13.0), vec2(140.0, 26.0));
        kit::text_trunc(&rui, pos2(row.left(), row.center().y), Align2::LEFT_CENTER, action.label(), t::regular(13.0), t::TEXT_PRIMARY, field.left() - 12.0 - row.left());

        let field_id = Id::new(("hk_field", i));
        let resp = rui.interact(field, field_id, if enabled { Sense::click() } else { Sense::hover() });
        let current = self.config.hotkeys.get(action).cloned();
        let edit_here = self.hotkey_edit.as_ref().filter(|e| e.action() == action);
        let editing = edit_here.is_some();
        let recording = matches!(edit_here, Some(HotkeyEdit::Recording { .. }));
        let conflict = match edit_here {
            Some(HotkeyEdit::TakenBy { combo, .. }) | Some(HotkeyEdit::SystemConflict { combo, .. }) => Some(combo.clone()),
            _ => None,
        };
        let taken_by = match edit_here {
            Some(HotkeyEdit::TakenBy { combo, other, .. }) => Some((combo.clone(), *other)),
            _ => None,
        };

        let hover = rui.ctx().animate_bool_with_time(resp.id.with("hover"), enabled && resp.hovered() && !editing, t::ANIM_HOVER);
        if enabled && resp.hovered() {
            rui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let rec_t = rui.ctx().animate_bool_with_time(field_id.with("rec"), recording, t::ANIM_HOVER);

        // Поле: назначено / не назначено / запись / конфликт.
        let (fill, stroke) = if conflict.is_some() {
            (t::DANGER_TINT, t::DANGER)
        } else if current.is_some() {
            (
                kit::lerp(kit::lerp(t::SURFACE_2, t::SURFACE_3, hover), t::accent_tint(accent), rec_t),
                kit::lerp(t::BLOCK_STROKE, accent, rec_t),
            )
        } else {
            (
                kit::lerp(Color32::TRANSPARENT, t::accent_tint(accent), rec_t),
                kit::lerp(t::LINE, accent, rec_t),
            )
        };
        rui.painter().rect(field, 6u8, fill, Stroke::new(1.0, stroke), StrokeKind::Inside);

        let mut text_x = field.left() + 8.0;
        let (text, color, font) = if recording {
            // Точка записи: альфа 1 ↔ 0,35 по синусу, цикл 1000 мс.
            let phase = rui.input(|i| i.time) as f32 % 1.0;
            let a = 0.675 + 0.325 * (phase * std::f32::consts::TAU).cos();
            rui.painter().circle_filled(pos2(text_x + 3.0, field.center().y), 3.0, accent.gamma_multiply(a));
            rui.ctx().request_repaint_after(Duration::from_millis(33));
            text_x += 10.0;
            match system::partial_display(rui.input(|i| i.modifiers)) {
                Some(partial) => (partial, t::TEXT_PRIMARY, t::medium(12.0)),
                None => ("Нажмите сочетание…".to_string(), t::TEXT_PRIMARY, t::regular(12.0)),
            }
        } else if let Some(combo) = &conflict {
            static_icon(&rui, Rect::from_center_size(pos2(text_x + 6.0, field.center().y), vec2(12.0, 12.0)), ph::WARNING, 12.0, true, t::DANGER);
            text_x += 16.0;
            (combo.display(), t::DANGER, t::medium(12.0))
        } else if let Some(combo) = &current {
            (combo.display(), t::TEXT_PRIMARY, t::medium(12.0))
        } else {
            ("Не назначено".to_string(), t::TEXT_TERTIARY, t::regular(12.0))
        };

        // При наведении на назначенное поле — кнопка «очистить» 20×20 справа.
        let show_clear = enabled && current.is_some() && !editing && rui.rect_contains_pointer(field);
        let text_right = if show_clear { field.right() - 3.0 - 20.0 - 4.0 } else { field.right() - 8.0 };
        kit::text_trunc(&rui, pos2(text_x, field.center().y), Align2::LEFT_CENTER, &text, font, color, text_right - text_x);
        let mut cleared = false;
        if show_clear {
            let clear_r = Rect::from_center_size(pos2(field.right() - 3.0 - 10.0, field.center().y), vec2(20.0, 20.0));
            let style = ButtonStyle {
                fill: t::TOGGLE_OFF,
                hover_fill: kit::lerp(t::TOGGLE_OFF, t::SURFACE_3, 0.5),
                press_fill: t::SURFACE_3,
                color: t::TEXT_PRIMARY,
                hover_color: t::TEXT_PRIMARY,
                stroke: None,
                radius: 4,
            };
            if kit::button(&rui, clear_r, field_id.with("clear"), Content::Icon { glyph: ph::X, size: 12.0, fill: false }, style, true)
                .on_hover_text("Очистить")
                .clicked()
            {
                self.config.hotkeys.set(action, None);
                self.apply_hotkeys();
                save = true;
                cleared = true;
            }
        }

        if enabled && resp.clicked() && !cleared && !recording {
            self.start_hotkey_recording(action);
        }

        // Запись: ловим первое сочетание с не-модификаторной клавишей.
        if recording {
            rui.memory_mut(|m| {
                m.request_focus(field_id);
                m.set_focus_lock_filter(
                    field_id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                );
            });
            // Само нажатие ловит обработчик окна (см. system::capture): так видны
            // медиаклавиши и сочетания с Alt.
            match system::capture::take() {
                Some(system::capture::Captured::Cancel) => self.cancel_hotkey_edit(),
                Some(system::capture::Captured::Combo(combo)) if combo.is_acceptable() => {
                    self.finish_hotkey_recording(action, combo);
                    save = true;
                }
                _ => {}
            }
            // Клик мимо поля — тоже отмена.
            let clicked_elsewhere = rui.input(|i| i.pointer.any_pressed())
                && rui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !field.contains(p));
            if clicked_elsewhere && matches!(self.hotkey_edit, Some(HotkeyEdit::Recording { .. })) {
                self.cancel_hotkey_edit();
            }
        }

        // Строка под полем.
        if msg_h > 0.5 {
            let msg = Rect::from_min_size(pos2(row.left(), row.bottom()), vec2(row.width(), 24.0));
            let mut mui = rui.new_child(UiBuilder::new().max_rect(msg).id_salt("msg"));
            mui.multiply_opacity((msg_h / 24.0).clamp(0.0, 1.0));
            match self.hotkey_edit.as_ref().filter(|e| e.action() == action) {
                Some(HotkeyEdit::Recording { .. }) => {
                    kit::text(&mui, pos2(msg.left(), msg.center().y), Align2::LEFT_CENTER, "Esc — отмена", t::regular(12.0), t::TEXT_TERTIARY);
                }
                Some(HotkeyEdit::SystemConflict { .. }) => {
                    kit::text_trunc(&mui, pos2(msg.left(), msg.center().y), Align2::LEFT_CENTER, "Занято Windows или другой программой", t::regular(12.0), t::DANGER, msg.width());
                }
                Some(HotkeyEdit::TakenBy { .. }) => {
                    if let Some((combo, other)) = taken_by.clone() {
                        let cancel_w = kit::galley(&mui, "Отмена", t::medium(12.0), t::TEXT_PRIMARY).size().x + 16.0;
                        let replace_w = kit::galley(&mui, "Заменить", t::medium(12.0), accent).size().x + 16.0;
                        let cancel_r = Rect::from_min_size(pos2(msg.right() - cancel_w, msg.center().y - 11.0), vec2(cancel_w, 22.0));
                        let replace_r = Rect::from_min_size(pos2(cancel_r.left() - 8.0 - replace_w, cancel_r.top()), vec2(replace_w, 22.0));
                        kit::text_trunc(&mui, pos2(msg.left(), msg.center().y), Align2::LEFT_CENTER, &format!("Уже у «{}»", other.label()), t::regular(12.0), t::DANGER, replace_r.left() - 8.0 - msg.left());
                        if kit::button(&mui, replace_r, field_id.with("replace"), Content::Text { label: "Заменить", font: t::medium(12.0) }, ButtonStyle::accent_outline(accent, t::RADIUS_CHIP), true).clicked() {
                            // «Заменить» снимает сочетание с другого действия.
                            self.config.hotkeys.set(other, None);
                            self.commit_hotkey(action, combo);
                            save = true;
                        }
                        if kit::button(&mui, cancel_r, field_id.with("cancel"), Content::Text { label: "Отмена", font: t::medium(12.0) }, ButtonStyle::secondary_bright(t::RADIUS_CHIP), true).clicked() {
                            self.cancel_hotkey_edit();
                        }
                    }
                }
                None => {}
            }
        }
        save
    }

    /// ДАННЫЕ: папка хранения — строка высотой 32 от `y`. Возвращает, нужно ли сохранить.
    fn draw_data_row(&mut self, ui: &mut Ui, left: f32, y: f32, w: f32) -> bool {
        let mut save = false;
        let data_dir = self.config.data_path.clone().unwrap_or_else(storage::get_default_path);
        let reset_r = Rect::from_min_size(pos2(left + w - 28.0, y + 2.0), vec2(28.0, 28.0));
        let change_label = "Изменить";
        let change_w = kit::galley(ui, change_label, t::medium(12.0), t::TEXT_PRIMARY).size().x + 20.0;
        let change_r = Rect::from_min_size(pos2(reset_r.left() - 6.0 - change_w, y), vec2(change_w, 32.0));
        let path_r = Rect::from_min_max(pos2(left, y), pos2(change_r.left() - 6.0, y + 32.0));
        ui.painter().rect(path_r, t::RADIUS_CTRL, t::SURFACE_2, t::block_stroke(), StrokeKind::Inside);
        static_icon(ui, Rect::from_center_size(pos2(path_r.left() + 17.0, path_r.center().y), vec2(14.0, 14.0)), ph::FOLDER, 14.0, false, t::TEXT_SECONDARY);
        let path_text = storage::state_file_path(self.config.data_path.as_ref()).display().to_string();
        let path_resp = ui.interact(path_r, Id::new("data_path"), Sense::hover());
        kit::text_trunc(ui, pos2(path_r.left() + 30.0, path_r.center().y), Align2::LEFT_CENTER, &data_dir.display().to_string(), t::regular(12.0), t::TEXT_SECONDARY, path_r.width() - 40.0);
        path_resp.on_hover_text(path_text);
        if kit::button(ui, change_r, Id::new("data_change"), Content::Text { label: change_label, font: t::medium(12.0) }, ButtonStyle::secondary_bright(t::RADIUS_CTRL), true).clicked() {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                self.config.data_path = Some(dir);
                save = true; // сразу переносим данные в новое место
            }
        }
        if kit::button(ui, reset_r, Id::new("data_reset"), Content::Icon { glyph: ph::ARROW_COUNTER_CLOCKWISE, size: 14.0, fill: false }, ButtonStyle::secondary(t::RADIUS_CTRL), self.config.data_path.is_some())
            .on_hover_text("Папка по умолчанию")
            .clicked()
        {
            self.config.data_path = None;
            save = true;
        }

        save
    }

    fn draw_options_tab(&mut self, ui: &mut Ui, area: Rect, accent: Color32) {
        let ctx = ui.ctx().clone();
        let left = area.left();
        let w = area.width();
        let mut y = area.top();
        let mut save = false;

        // АКЦЕНТ: 8 готовых цветов + свой цвет.
        kit::overline(ui, pos2(left, y + 7.0), "Акцент");
        y += 14.0 + 8.0;
        let current = self.config.primary_color;
        let custom_active = !t::SWATCHES.contains(&current);
        let mut picked = None;
        for (i, rgb) in t::SWATCHES.iter().enumerate() {
            let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            let center = pos2(left + 2.0 + 12.0 + i as f32 * 34.0, y + 12.0);
            let r = Rect::from_center_size(center, vec2(24.0, 24.0));
            let resp = ui.interact(r, Id::new(("swatch", i)), Sense::click());
            if resp.hovered() {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            ui.painter().circle_filled(center, 12.0, c);
            if *rgb == current {
                // Кольцо: 2 px фона, затем 2 px цвета.
                ui.painter().circle_stroke(center, 15.0, Stroke::new(2.0, c));
            }
            if resp.clicked() {
                picked = Some(*rgb);
            }
        }
        // Свой цвет — кнопка с палитрой, открывает пипетку.
        let custom_center = pos2(left + 2.0 + 12.0 + 8.0 * 34.0, y + 12.0);
        let custom_r = Rect::from_center_size(custom_center, vec2(24.0, 24.0));
        let custom_resp = ui.interact(custom_r, Id::new("swatch_custom"), Sense::click()).on_hover_text("Свой цвет");
        if custom_resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        if custom_active {
            ui.painter().circle_filled(custom_center, 12.0, accent);
            ui.painter().circle_stroke(custom_center, 15.0, Stroke::new(2.0, accent));
        } else {
            ui.painter().circle_filled(custom_center, 12.0, t::SURFACE_2);
            static_icon(ui, custom_r, ph::PALETTE, 14.0, false, t::TEXT_SECONDARY);
        }
        if custom_resp.clicked() {
            self.accent_picker_open = !self.accent_picker_open;
        }
        if let Some(rgb) = picked {
            self.config.primary_color = rgb;
            t::apply_style(&ctx, accent_of(&self.config));
            self.accent_picker_open = false;
            save = true;
        }
        let swatch_bottom = y + 24.0;
        y += 24.0 + 12.0;

        // БУДИЛЬНИК: громкость и звук.
        kit::overline(ui, pos2(left, y + 7.0), "Будильник");
        y += 14.0 + 4.0;
        kit::text(ui, pos2(left, y + 16.0), Align2::LEFT_CENTER, "Громкость", t::regular(13.0), t::TEXT_PRIMARY);
        let slider_r = Rect::from_min_max(pos2(left + 72.0 + 12.0, y + 8.0), pos2(left + w - 36.0 - 12.0, y + 24.0));
        let out = kit::slider(ui, slider_r, Id::new("volume"), &mut self.config.alarm_volume, accent);
        // Пишем на диск по отпусканию ползунка, а не на каждый пиксель перетаскивания.
        if out.released {
            save = true;
        }
        let _ = out.changed;
        kit::text(ui, pos2(left + w, y + 16.0), Align2::RIGHT_CENTER, &format!("{:.0}%", self.config.alarm_volume * 100.0), t::mono(12.0), t::TEXT_SECONDARY);
        y += 32.0 + 4.0;

        kit::text(ui, pos2(left, y + 16.0), Align2::LEFT_CENTER, "Звук", t::regular(13.0), t::TEXT_PRIMARY);
        let test_r = Rect::from_min_size(pos2(left + w - 28.0, y + 2.0), vec2(28.0, 28.0));
        let reset_r = test_r.translate(vec2(-34.0, 0.0));
        let choose_label = "Выбрать…";
        let choose_w = kit::galley(ui, choose_label, t::medium(12.0), t::TEXT_PRIMARY).size().x + 20.0;
        let choose_r = Rect::from_min_size(pos2(reset_r.left() - 6.0 - choose_w, y + 2.0), vec2(choose_w, 28.0));
        let sound_name = match &self.config.alarm_sound {
            Some(p) if Path::new(p).exists() => Path::new(p)
                .file_name().map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone()),
            Some(_) => "файл не найден → встроенный".to_string(),
            None => "Встроенный".to_string(),
        };
        let name_left = left + 72.0 + 6.0;
        kit::text_trunc(ui, pos2(name_left, y + 16.0), Align2::LEFT_CENTER, &sound_name, t::regular(12.0), t::TEXT_SECONDARY, choose_r.left() - 6.0 - name_left);
        if kit::button(ui, choose_r, Id::new("sound_pick"), Content::Text { label: choose_label, font: t::medium(12.0) }, ButtonStyle::secondary_bright(t::RADIUS_CTRL), true).clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Аудио", &["mp3", "wav", "ogg", "flac"])
                .pick_file()
            {
                self.config.alarm_sound = Some(path.display().to_string());
                save = true;
            }
        }
        if kit::button(ui, reset_r, Id::new("sound_reset"), Content::Icon { glyph: ph::ARROW_COUNTER_CLOCKWISE, size: 14.0, fill: false }, ButtonStyle::secondary(t::RADIUS_CTRL), self.config.alarm_sound.is_some())
            .on_hover_text("Вернуть встроенный")
            .clicked()
        {
            self.config.alarm_sound = None;
            save = true;
        }
        let testing = self.alarm_is_playing() && self.ringing.is_none();
        let (test_glyph, test_tip) = if testing { (ph::STOP, "Остановить") } else { (ph::PLAY, "Тест звука") };
        if kit::button(ui, test_r, Id::new("sound_test"), Content::Icon { glyph: test_glyph, size: 14.0, fill: true }, ButtonStyle::accent(accent, t::RADIUS_CTRL), true)
            .on_hover_text(test_tip)
            .clicked()
        {
            if testing { self.stop_alarm(); } else { self.play_alarm(false); }
        }
        y += 32.0 + 4.0;

        let notify_r = Rect::from_min_size(pos2(left, y), vec2(w, 32.0));
        if kit::toggle_row(ui, notify_r, Id::new("toggle_notify"), "Уведомление Windows", 0.0, &mut self.config.notify_enabled, accent, true) {
            save = true;
        }
        y += 32.0 + 12.0;

        // ОКНО И МУЗЫКА: переключатели.
        kit::overline(ui, pos2(left, y + 7.0), "Окно и музыка");
        y += 14.0 + 4.0;
        let top_r = Rect::from_min_size(pos2(left, y), vec2(w, 32.0));
        if kit::toggle_row(ui, top_r, Id::new(("toggle", 0)), "Поверх всех окон", 0.0, &mut self.config.always_on_top, accent, true) {
            save = true;
        }
        y += 32.0;

        // Зазор между блоками: 4–16 px, меняется сразу, на диск — по отпусканию.
        kit::text(ui, pos2(left, y + 16.0), Align2::LEFT_CENTER, "Зазор между блоками", t::regular(13.0), t::TEXT_PRIMARY);
        let gap_r = Rect::from_min_max(pos2(left + 128.0 + 12.0, y + 8.0), pos2(left + w - 36.0 - 12.0, y + 24.0));
        let span = t::BLOCK_GAP_MAX - t::BLOCK_GAP_MIN;
        let mut k = ((self.config.block_gap - t::BLOCK_GAP_MIN) / span).clamp(0.0, 1.0);
        let gap_out = kit::slider(ui, gap_r, Id::new("block_gap"), &mut k, accent);
        if gap_out.changed {
            self.config.block_gap = (t::BLOCK_GAP_MIN + k * span).round();
        }
        if gap_out.released {
            save = true;
        }
        kit::text(ui, pos2(left + w, y + 16.0), Align2::RIGHT_CENTER, &format!("{} px", self.config.block_gap.round() as i32), t::mono(12.0), t::TEXT_SECONDARY);
        y += 32.0;

        let music_on = self.config.music_enabled;
        let rows: [(&str, f32, bool); 4] = [
            ("Плашка «Сейчас играет»", 0.0, true),
            ("Скрывать, когда ничего не играет", 12.0, music_on),
            ("Пауза музыки при будильнике", 12.0, music_on),
            ("Продолжать со стартом фокуса", 12.0, music_on),
        ];
        for (i, (label, indent, enabled)) in rows.into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(left, y), vec2(w, 32.0));
            let value = match i {
                0 => &mut self.config.music_enabled,
                1 => &mut self.config.music_hide_idle,
                2 => &mut self.config.music_pause_on_alarm,
                _ => &mut self.config.music_resume_on_focus,
            };
            if kit::toggle_row(ui, r, Id::new(("toggle", i + 1)), label, indent, value, accent, enabled) {
                save = true;
            }
            y += 32.0;
        }
        let _ = y;

        // Пипетка своего цвета — поверх содержимого, под рядом свотчей.
        if self.accent_picker_open {
            let mut color = accent;
            let area_resp = egui::Area::new(Id::new("accent_picker"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos2(left, swatch_bottom + 8.0))
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        egui::color_picker::color_picker_color32(ui, &mut color, egui::color_picker::Alpha::Opaque)
                    }).inner
                });
            if area_resp.inner {
                self.config.primary_color = [color.r(), color.g(), color.b()];
                t::apply_style(&ctx, color);
            }
            let clicked_outside = ctx.input(|i| i.pointer.any_click())
                && ctx.input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| !area_resp.response.rect.contains(p) && !custom_r.contains(p));
            if clicked_outside {
                self.accent_picker_open = false;
                save = true;
            }
        }

        if save {
            self.save_to_disk();
        }
    }
}

impl eframe::App for TimerApp {
    // Окно прозрачное, поэтому и фон кадра должен быть полностью прозрачным,
    // иначе в промежутках между "плавающими" блоками остаётся серая заливка.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// Вызывается перед каждым ui() и отдельно, пока окно скрыто.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- ГОРЯЧИЕ КЛАВИШИ И ТРЕЙ ---
        for action in self.hotkeys.poll() {
            self.handle_action(ctx, action);
        }
        let tray_commands = self.tray.as_ref().map(|tr| tr.poll()).unwrap_or_default();
        for cmd in tray_commands {
            self.handle_tray(ctx, cmd);
        }

        // --- ЛОГИКА ТАЙМЕРОВ ---
        let _ = self.music_mode(); // свежий снимок музыки — для паузы при будильнике
        self.tick_timers(ctx);
        self.finish_dying_rows();
        if let Some(id) = self.ringing {
            if self.timer_index(id).is_none() {
                self.silence(false);
            }
        }

        // --- СОХРАНЕНИЕ ---
        // Закрытие окна (крестик или Alt+F4): успеваем дописать прогресс,
        // иначе время идущего таймера с момента последнего старта пропадало.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.sync_and_save();
        }
        if self.last_autosave.elapsed() >= AUTOSAVE_EVERY {
            self.sync_and_save();
            self.last_autosave = Instant::now();
        }

        system::capture::set_active(matches!(self.hotkey_edit, Some(HotkeyEdit::Recording { .. })));

        // Ушли с вкладки «Система» — запись сочетания и конфликт отменяются.
        if self.hotkey_edit.is_some() && (!self.menu_open || self.tab != Tab::System || !self.widget_visible) {
            self.cancel_hotkey_edit();
        }

        self.sync_tray(ctx);
        self.sync_voice(ctx);

        // --- ЗАМЕТКИ ---
        self.tick_reminders(ctx);
        if let Some((id, since)) = self.note_alert {
            if since.elapsed() > NOTE_ALERT_HOLD || self.note_index(id).is_none() {
                self.note_alert = None;
            }
        }
        if self.notes_dirty_at.is_some_and(|at| at.elapsed() > Duration::from_millis(800)) {
            self.notes_dirty_at = None;
            self.save_to_disk();
        }

        // Скрытый виджет ничего не рисует, но таймеры должны тикать.
        if !self.widget_visible {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let accent = accent_of(&self.config);
        if !self.system_frame_removed {
            self.system_frame_removed = true;
            remove_system_frame(frame);
        }

        // Таймеры, сохранение, горячие клавиши и трей — в logic(): она работает
        // и когда виджет скрыт, а ui() в этот момент не вызывается.

        // --- МУЗЫКА ---
        let mode = self.music_mode();
        self.update_cover_texture(&ctx);
        let music_visible = mode != MusicMode::Hidden;
        let music_vis = ctx.animate_value_with_time(
            Id::new("music_vis"),
            if music_visible { 1.0 } else { 0.0 },
            if music_visible { t::ANIM_MUSIC_SHOW } else { t::ANIM_MUSIC_HIDE },
        );
        let music_present = music_visible || music_vis > 0.001;
        let music_target_h = match mode {
            MusicMode::Full => t::MUSIC_H,
            MusicMode::Mini => t::MUSIC_MINI_H,
            MusicMode::Hidden => self.music_last_h,
        };
        self.music_last_h = music_target_h;
        let music_h = ctx.animate_value_with_time(Id::new("music_h"), music_target_h, t::ANIM_MUSIC_RESIZE);
        // Окно растёт сразу, а уменьшается только когда анимация закончилась —
        // чтобы не менять размер окна каждый кадр.
        if music_target_h > self.music_slot_h || (music_h - music_target_h).abs() < 0.5 {
            self.music_slot_h = music_target_h;
        }

        // --- МЕНЮ ---
        let menu_raw = ctx.animate_value_with_time(
            Id::new("menu_vis"),
            if self.menu_open { 1.0 } else { 0.0 },
            if self.menu_open { t::ANIM_MENU_OPEN } else { t::ANIM_MENU_CLOSE },
        );
        let menu_present = self.menu_open || menu_raw > 0.001;

        // --- РАСКЛАДКА СТОПКИ ---
        // По вертикали всё считается относительно верха виджета (вниз — плюс).
        // Виджет стоит на месте; плашка закреплена над ним или под ним, меню
        // раскрывается туда, где на экране есть место.
        let gap = self.config.block_gap.clamp(t::BLOCK_GAP_MIN, t::BLOCK_GAP_MAX);
        let slot_h = self.music_slot_h;
        let slot_below = t::WIDGET_H + gap;
        let slot_above = -(gap + music_h);
        let music_home = if self.config.music_above { slot_above } else { slot_below };

        let just_opened = self.menu_open && !self.prev_menu_open;
        if just_opened {
            let music_below_part = if music_present && !self.config.music_above { gap + slot_h } else { 0.0 };
            let music_above_part = if music_present && self.config.music_above { gap + slot_h } else { 0.0 };
            let need_below = t::WIDGET_H + music_below_part + gap + t::MENU_H;
            let need_above = music_above_part + gap + t::MENU_H;
            self.menu_up = self.choose_menu_up(&ctx, need_below, need_above);
        }
        self.prev_menu_open = self.menu_open;

        // --- ЗАМЕТКИ ---
        // Панель стоит сбоку от виджета (слева, если там есть место) и
        // по высоте растёт туда же, куда хватает экрана.
        let notes_raw = ctx.animate_value_with_time(
            Id::new("notes_vis"),
            if self.notes_open { 1.0 } else { 0.0 },
            if self.notes_open { t::ANIM_MENU_OPEN } else { t::ANIM_MENU_CLOSE },
        );
        let notes_present = self.notes_open || notes_raw > 0.001;
        // По высоте — от виджета до дальнего края меню настроек (как будто оно открыто),
        // чтобы панель и меню заканчивались на одной линии.
        let music_below = if music_present && !self.config.music_above { gap + slot_h } else { 0.0 };
        let music_above = if music_present && self.config.music_above { gap + slot_h } else { 0.0 };
        let notes_full = |up: bool| t::WIDGET_H + gap + t::MENU_H + if up { music_above } else { music_below };
        if self.notes_open && !self.prev_notes_open {
            self.notes_up = if self.menu_open {
                self.menu_up
            } else {
                self.choose_menu_up(&ctx, notes_full(false), notes_full(true) - t::WIDGET_H)
            };
            self.notes_right = !self.notes_fit_left(&ctx, t::NOTES_W + gap);
        }
        self.prev_notes_open = self.notes_open;
        let notes_target = notes_full(self.notes_up);
        let notes_h = ctx.animate_value_with_time(Id::new("notes_h"), notes_target, t::ANIM_MUSIC_RESIZE);
        let notes_slot = notes_h.max(notes_target);
        let notes_y = if self.notes_up { t::WIDGET_H - notes_h } else { 0.0 };

        // Плашка: пока её тянут — за курсором, после отпускания доезжает до места.
        let music_y = if let Some(d) = &self.music_drag {
            d.y
        } else if let Some((from, at)) = self.music_snap {
            let k = kit::tween(at, t::ANIM_MUSIC_SNAP);
            if k >= 1.0 {
                self.music_snap = None;
            }
            from + (music_home - from) * k
        } else {
            music_home
        };
        let music_moving = self.music_drag.is_some() || self.music_snap.is_some();

        // Меню стоит за плашкой, если та с той же стороны виджета. Когда плашку
        // переставили, меню плавно сдвигается; в момент открытия — встаёт сразу.
        let music_on_menu_side = music_present && self.config.music_above == self.menu_up;
        let menu_target = if self.menu_up {
            -(gap + t::MENU_H) - if music_on_menu_side { gap + slot_h } else { 0.0 }
        } else {
            t::WIDGET_H + gap + if music_on_menu_side { slot_h + gap } else { 0.0 }
        };
        let shift_time = if just_opened || !menu_present { 0.0001 } else { t::ANIM_MENU_SHIFT };
        let menu_y = ctx.animate_value_with_time(Id::new("menu_y"), menu_target, shift_time);

        // Окно вмещает все блоки. Пока плашку тянут или она доезжает, держим
        // место под оба её положения — чтобы она могла пройти мимо виджета.
        // Только после начала перетаскивания, а не по нажатию: иначе окно
        // расширялось под курсором при клике по кнопкам плашки, и клик терялся.
        let (mut above_ext, mut below_ext) = (0.0f32, 0.0f32);
        let mut include = |top: f32, h: f32| {
            above_ext = above_ext.max(-top);
            below_ext = below_ext.max(top + h - t::WIDGET_H);
        };
        if music_present {
            let both = music_moving;
            if both || self.config.music_above {
                include(-(gap + slot_h), slot_h);
            }
            if both || !self.config.music_above {
                include(slot_below, slot_h);
            }
        }
        if menu_present {
            include(menu_target, t::MENU_H);
            include(menu_y, t::MENU_H);
        }
        if notes_present {
            include(if self.notes_up { t::WIDGET_H - notes_slot } else { 0.0 }, notes_slot);
        }
        let stack_h = above_ext + t::WIDGET_H + below_ext;
        let side = t::NOTES_W + gap;
        let left_ext = if notes_present && !self.notes_right { side } else { 0.0 };
        let right_ext = if notes_present && self.notes_right { side } else { 0.0 };

        // --- ГЕОМЕТРИЯ ---
        // Окно сразу получает полную высоту стопки (лишнее — прозрачное),
        // а блоки появляются внутри него анимацией. Закрытие — наоборот.
        self.sync_geometry(&ctx, stack_h, above_ext, left_ext, right_ext);
        if self.hold_until_placed {
            if self.geometry_pending.is_some() {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            self.hold_until_placed = false;
        }
        let (above, left) = self.applied.map(|(_, a, _, l, _)| (a, l)).unwrap_or((0.0, 0.0));

        let origin = ui.max_rect().min;
        let x = origin.x + t::PAD_X + left;
        let widget_top = origin.y + t::PAD_TOP + above;
        let widget_rect = Rect::from_min_size(pos2(x, widget_top), vec2(t::WINDOW_W, t::WIDGET_H));
        // Блоки центрируются по виджету: плашка уже него, меню — во всю ширину.
        let center_x = x + t::WINDOW_W / 2.0;
        let music_rect = music_present
            .then(|| Rect::from_min_size(pos2(center_x - t::MUSIC_W / 2.0, widget_top + music_y), vec2(t::MUSIC_W, music_h)));
        let menu_rect = menu_present.then(|| Rect::from_min_size(pos2(x, widget_top + menu_y), vec2(t::WINDOW_W, t::MENU_H)));

        {
            let mut wui = ui.new_child(UiBuilder::new().max_rect(widget_rect).id_salt("widget"));
            self.draw_widget(&mut wui, widget_rect, accent);
        }
        if notes_present {
            let nx = if self.notes_right { x + t::WINDOW_W + gap } else { x - gap - t::NOTES_W };
            let r = Rect::from_min_size(pos2(nx, widget_top + notes_y), vec2(t::NOTES_W, notes_h));
            // Открытие: alpha 0 → 1 и сдвиг 8 → 0 от виджета.
            let e = ease_out(notes_raw);
            let toward_widget = if self.notes_right { -1.0 } else { 1.0 };
            let r = r.translate(vec2(toward_widget * 8.0 * (1.0 - e), 0.0));
            let mut nui = ui.new_child(UiBuilder::new().max_rect(r).id_salt("notes"));
            nui.multiply_opacity(e);
            self.draw_notes(&mut nui, r, accent);
        }
        if let Some(r) = menu_rect {
            // Открытие меню: alpha 0 → 1 и сдвиг 8 → 0 от виджета.
            let e = ease_out(menu_raw);
            let toward_widget = if self.menu_up { 1.0 } else { -1.0 };
            let r = r.translate(vec2(0.0, toward_widget * 8.0 * (1.0 - e)));
            let mut menu_ui = ui.new_child(UiBuilder::new().max_rect(r).id_salt("menu"));
            menu_ui.multiply_opacity(e);
            self.draw_menu(&mut menu_ui, r, accent);
        }
        if let Some(r) = music_rect {
            // Появление: alpha 0 → 1 и сдвиг 6 → 0 от виджета.
            let e = ease_out(music_vis);
            let toward_widget = if music_y < 0.0 { 1.0 } else { -1.0 };
            let r = r.translate(vec2(0.0, toward_widget * 6.0 * (1.0 - e)));

            // Плашку можно перетащить над виджетом или под него. Зона перетаскивания
            // выделяется до кнопок плашки, поэтому кнопки по-прежнему нажимаются.
            let drag = ui.interact(r, Id::new("music_drag"), Sense::click_and_drag());
            // Курсор относительно верха виджета. Берём системный курсор в экранных
            // пикселях: окно во время перетаскивания расширяется, и координаты egui
            // внутри окна на кадр-другой отстают — плашка бы дёргалась. Виджет же
            // на экране стоит на месте, так что эта величина стабильна.
            let ppp = ctx.pixels_per_point();
            let cursor_rel = match (cursor_px(), self.anchor_px) {
                (Some(c), Some(a)) => Some((c.y - a.y) / ppp),
                _ => drag.interact_pointer_pos().map(|p| p.y - widget_top),
            };
            if drag.drag_started() {
                if let Some(cy) = cursor_rel {
                    self.music_drag = Some(MusicDrag { y: music_y, grab_dy: cy - music_y });
                    self.music_snap = None;
                }
            }
            if drag.dragged() {
                if let (Some(d), Some(cy)) = (self.music_drag.as_mut(), cursor_rel) {
                    d.y = (cy - d.grab_dy).clamp(slot_above, slot_below);
                }
            }
            if drag.drag_stopped() {
                if let Some(d) = self.music_drag.take() {
                    // Куда ближе центр плашки — туда и закрепляем.
                    let above_widget = d.y + music_h / 2.0 < t::WIDGET_H / 2.0;
                    if above_widget != self.config.music_above {
                        self.config.music_above = above_widget;
                        self.save_to_disk();
                    }
                    self.music_snap = Some((d.y, Instant::now()));
                }
            }
            if self.music_drag.is_some() {
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            } else if drag.hovered() {
                ctx.set_cursor_icon(CursorIcon::Grab);
            }

            let mut mui = ui.new_child(UiBuilder::new().max_rect(r).id_salt("music"));
            mui.multiply_opacity(e);
            self.draw_music(&mut mui, r, accent);
        } else {
            self.music_drag = None;
        }

        if self.applied_on_top != Some(self.config.always_on_top) {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                if self.config.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal }
            ));
            self.applied_on_top = Some(self.config.always_on_top);
        }

        // Перерисовка: полная частота, пока что-то анимируется или с виджетом
        // взаимодействуют; редкие кадры в покое — чтобы не грузить процессор.
        let any_running = self.state.active_timers.iter().any(|t| t.is_running());
        let own_anims = self.ringing.is_some()
            || !self.row_born.is_empty()
            || !self.row_dying.is_empty()
            || self.tab_changed_at.elapsed().as_secs_f32() < t::ANIM_TAB
            || self.geometry_pending.is_some()
            || self.music_drag.is_some()
            || self.music_snap.is_some()
            || self.note_alert.is_some_and(|(_, since)| since.elapsed() < NOTE_ALERT_PULSE)
            || self.voice_listening;
        let interacting = ctx.is_pointer_over_egui() || ctx.input(|i| i.pointer.any_down());
        let wait_ms = if own_anims || interacting {
            16
        } else if any_running || self.menu_open || self.music_last.as_ref().is_some_and(|s| s.playing) {
            250
        } else {
            1000
        };
        ctx.request_repaint_after(Duration::from_millis(wait_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Повторяет арифметику сетки календаря. До правки здесь стоял .unwrap()
    /// и приложение падало на 31 апреля / 30 февраля.
    #[test]
    fn calendar_grid_covers_every_month_without_invalid_dates() {
        for year in 2023..=2030 {
            for m in 1..=12u32 {
                let month = NaiveDate::from_ymd_opt(year, m, 1).unwrap();
                let start_offset = month.weekday().num_days_from_monday() as i32;
                let mut rendered = 0i64;
                for i in 0..(calendar_rows(month) * 7) as i32 {
                    let day_num = i - start_offset + 1;
                    let date = if day_num > 0 {
                        NaiveDate::from_ymd_opt(month.year(), month.month(), day_num as u32)
                    } else {
                        None
                    };
                    if date.is_some() { rendered += 1; }
                }
                let days_in_month = shift_month(month, 1).signed_duration_since(month).num_days();
                assert_eq!(rendered, days_in_month, "месяц {}-{}", year, m);
            }
        }
    }

    #[test]
    fn calendar_rows_fit_five_or_six() {
        // Октябрь 2026 начинается в четверг: 3 + 31 = 34 ячейки → 5 строк.
        assert_eq!(calendar_rows(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()), 5);
        // Март 2026 начинается в воскресенье: 6 + 31 = 37 → 6 строк.
        assert_eq!(calendar_rows(NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()), 6);
        // Февраль 2027 начинается в понедельник: ровно 4 строки.
        assert_eq!(calendar_rows(NaiveDate::from_ymd_opt(2027, 2, 1).unwrap()), 4);
    }

    #[test]
    fn shift_month_wraps_over_year_boundary() {
        let jan = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        assert_eq!(shift_month(jan, -1), NaiveDate::from_ymd_opt(2025, 12, 1).unwrap());
        let dec = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        assert_eq!(shift_month(dec, 1), NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
    }

    #[test]
    fn short_months_really_reject_day_31() {
        assert!(NaiveDate::from_ymd_opt(2026, 4, 31).is_none());
        assert!(NaiveDate::from_ymd_opt(2026, 2, 29).is_none());
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(fmt_hms(3661), "01:01:01");
        assert_eq!(fmt_hms(59), "00:00:59");
        // Минуты дополняются нулём, если есть часы: «2 ч 02 мин», как в макете.
        assert_eq!(fmt_human(3720), "1 ч 02 мин");
        assert_eq!(fmt_human(41 * 3600 + 30 * 60), "41 ч 30 мин");
        assert_eq!(fmt_human(120), "2 мин");
        assert_eq!(fmt_track_time(83.0), "1:23");
        assert_eq!(fmt_track_time(225.4), "3:45");
        assert_eq!(fmt_track_time(3723.0), "1:02:03");
    }

    #[test]
    fn breaks_are_short_timers_only() {
        let short = TimerInstance::new("Перерыв 5".into(), TimerMode::Timer(Duration::from_secs(5 * 60)));
        let edge = TimerInstance::new("Перерыв 15".into(), TimerMode::Timer(Duration::from_secs(15 * 60)));
        let focus = TimerInstance::new("Фокус 25".into(), TimerMode::Timer(Duration::from_secs(25 * 60)));
        let sw = TimerInstance::new("С".into(), TimerMode::Stopwatch);
        assert!(is_break(&short));
        assert!(is_break(&edge));
        assert!(!is_break(&focus));
        assert!(!is_break(&sw));
    }
}
