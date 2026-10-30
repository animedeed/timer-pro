//! Связь с Windows: глобальные горячие клавиши, значок в трее и уведомления.
//! Макет — `design/Timer Pro v2.dc.html`, разделы 3–4.
//!
//! Горячие клавиши и трей создаются на главном потоке: их скрытые окна получают
//! сообщения через цикл событий winit. Обработчики событий будят egui через
//! `request_repaint`, поэтому всё работает и при скрытом виджете.

use crate::theme as t;
use eframe::egui::{self, Color32};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde::{Deserialize, Serialize};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

// ================================================================ действия

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    StartPause,
    StopRecord,
    Silence,
    ToggleWidget,
    ToggleMenu,
    MusicPlayPause,
    MusicNext,
    MusicPrev,
    ToggleNotes,
}

impl Action {
    pub const ALL: [Action; 9] = [
        Action::StartPause,
        Action::StopRecord,
        Action::Silence,
        Action::ToggleWidget,
        Action::ToggleMenu,
        Action::MusicPlayPause,
        Action::MusicNext,
        Action::MusicPrev,
        Action::ToggleNotes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::StartPause => "Старт / пауза",
            Action::StopRecord => "Стоп и записать",
            Action::Silence => "Тихо (будильник)",
            Action::ToggleWidget => "Показать / скрыть виджет",
            Action::ToggleMenu => "Открыть / закрыть меню",
            Action::MusicPlayPause => "Музыка: играть / пауза",
            Action::MusicNext => "Музыка: следующий",
            Action::MusicPrev => "Музыка: предыдущий",
            Action::ToggleNotes => "Показать / скрыть заметки",
        }
    }
}

// ================================================================ сочетания

/// Клавиши, которых нет в egui: медиаклавиши и цифровой блок (для egui «5»
/// на нумпаде и «5» в верхнем ряду — одно и то же). Хранятся под собственными
/// именами. (имя, код, подпись, виртуальный код Windows, можно ли без Ctrl/Alt)
const SPECIAL_KEYS: [(&str, Code, &str, u32, bool); 19] = [
    ("MediaPlayPause", Code::MediaPlayPause, "Медиа: играть/пауза", 0xB3, true),
    ("MediaTrackNext", Code::MediaTrackNext, "Медиа: следующий", 0xB0, true),
    ("MediaTrackPrevious", Code::MediaTrackPrevious, "Медиа: предыдущий", 0xB1, true),
    ("MediaStop", Code::MediaStop, "Медиа: стоп", 0xB2, true),
    ("Numpad0", Code::Numpad0, "Num 0", 0x60, false),
    ("Numpad1", Code::Numpad1, "Num 1", 0x61, false),
    ("Numpad2", Code::Numpad2, "Num 2", 0x62, false),
    ("Numpad3", Code::Numpad3, "Num 3", 0x63, false),
    ("Numpad4", Code::Numpad4, "Num 4", 0x64, false),
    ("Numpad5", Code::Numpad5, "Num 5", 0x65, false),
    ("Numpad6", Code::Numpad6, "Num 6", 0x66, false),
    ("Numpad7", Code::Numpad7, "Num 7", 0x67, false),
    ("Numpad8", Code::Numpad8, "Num 8", 0x68, false),
    ("Numpad9", Code::Numpad9, "Num 9", 0x69, false),
    ("NumpadMultiply", Code::NumpadMultiply, "Num *", 0x6A, false),
    ("NumpadAdd", Code::NumpadAdd, "Num +", 0x6B, false),
    ("NumpadSubtract", Code::NumpadSubtract, "Num −", 0x6D, false),
    ("NumpadDecimal", Code::NumpadDecimal, "Num .", 0x6E, false),
    ("NumpadDivide", Code::NumpadDivide, "Num /", 0x6F, false),
];

/// (код, подпись, можно ли без Ctrl/Alt)
fn special_key(name: &str) -> Option<(Code, &'static str, bool)> {
    SPECIAL_KEYS.iter().find(|(n, ..)| *n == name).map(|(_, code, ru, _, alone)| (*code, *ru, *alone))
}

/// Сочетание клавиш. Обычная клавиша хранится под именем egui (`Key::name`) —
/// по физическому положению, поэтому не зависит от раскладки.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

impl Combo {
    pub fn new(ctrl: bool, alt: bool, shift: bool, key: egui::Key) -> Self {
        Self { ctrl, alt, shift, key: key.name().to_owned() }
    }

    fn egui_key(&self) -> Option<egui::Key> {
        egui::Key::from_name(&self.key)
    }

    fn special(name: &str, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self { ctrl, alt, shift, key: name.to_owned() }
    }

    /// Одиночная клавиша без Ctrl/Alt не принимается — кроме F13–F24 и медиаклавиш.
    pub fn is_acceptable(&self) -> bool {
        if let Some((_, _, alone)) = special_key(&self.key) {
            return alone || self.ctrl || self.alt;
        }
        let Some(key) = self.egui_key() else { return false };
        key_code(key).is_some() && (self.ctrl || self.alt || is_extra_f_key(key))
    }

    fn code(&self) -> Option<Code> {
        special_key(&self.key).map(|(code, ..)| code).or_else(|| key_code(self.egui_key()?))
    }

    /// «Ctrl+Alt+Пробел»: через «+» без пробелов, клавиши по-русски.
    pub fn display(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        let key = special_key(&self.key)
            .map(|(_, ru, _)| ru.to_string())
            .or_else(|| self.egui_key().map(key_display))
            .unwrap_or_else(|| self.key.clone());
        parts.push(key);
        parts.join("+")
    }

    pub fn to_hotkey(&self) -> Option<HotKey> {
        let code = self.code()?;
        let mut mods = Modifiers::empty();
        if self.ctrl {
            mods |= Modifiers::CONTROL;
        }
        if self.alt {
            mods |= Modifiers::ALT;
        }
        if self.shift {
            mods |= Modifiers::SHIFT;
        }
        Some(HotKey::new(Some(mods), code))
    }
}

/// Запись нового сочетания. Нажатия ловятся прямо в обработчике сообщений
/// окна (WM_KEYDOWN / WM_SYSKEYDOWN), а не через egui: egui не видит
/// медиаклавиши, а сочетания с Alt Windows иначе отправила бы в системное меню.
pub mod capture {
    use super::{Combo, SPECIAL_KEYS};
    use eframe::egui;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    pub enum Captured {
        Combo(Combo),
        Cancel,
    }

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static RESULT: Mutex<Option<Captured>> = Mutex::new(None);
    static CTX: OnceLock<egui::Context> = OnceLock::new();

    pub fn init(ctx: &egui::Context) {
        let _ = CTX.set(ctx.clone());
    }

    /// Включает или выключает перехват; при включении сбрасывает прошлый результат.
    pub fn set_active(active: bool) {
        let was = ACTIVE.swap(active, Ordering::Relaxed);
        if active && !was {
            if let Ok(mut r) = RESULT.lock() {
                *r = None;
            }
        }
    }

    pub fn take() -> Option<Captured> {
        RESULT.lock().ok().and_then(|mut r| r.take())
    }

    /// Вызывается из обработчика окна. `true` — нажатие поглощено.
    pub fn on_key_down(vk: u32, ctrl: bool, alt: bool, shift: bool) -> bool {
        if !ACTIVE.load(Ordering::Relaxed) {
            return false;
        }
        // Сами модификаторы (Shift, Ctrl, Alt, Win — левые и правые) не фиксируют сочетание.
        if matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5) {
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
            return true;
        }
        let captured = if vk == 0x1B {
            Some(Captured::Cancel)
        } else if let Some((name, ..)) = SPECIAL_KEYS.iter().find(|(_, _, _, v, _)| *v == vk) {
            Some(Captured::Combo(Combo::special(name, ctrl, alt, shift)))
        } else {
            vk_to_egui(vk).map(|key| Captured::Combo(Combo::new(ctrl, alt, shift, key)))
        };
        if let Some(c) = captured {
            if let Ok(mut r) = RESULT.lock() {
                *r = Some(c);
            }
        }
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
        true
    }

    /// Виртуальный код клавиши Windows → клавиша egui.
    fn vk_to_egui(vk: u32) -> Option<egui::Key> {
        use egui::Key as K;
        const LETTERS: [K; 26] = [
            K::A, K::B, K::C, K::D, K::E, K::F, K::G, K::H, K::I, K::J, K::K, K::L, K::M,
            K::N, K::O, K::P, K::Q, K::R, K::S, K::T, K::U, K::V, K::W, K::X, K::Y, K::Z,
        ];
        const DIGITS: [K; 10] = [K::Num0, K::Num1, K::Num2, K::Num3, K::Num4, K::Num5, K::Num6, K::Num7, K::Num8, K::Num9];
        const F_KEYS: [K; 24] = [
            K::F1, K::F2, K::F3, K::F4, K::F5, K::F6, K::F7, K::F8, K::F9, K::F10, K::F11, K::F12,
            K::F13, K::F14, K::F15, K::F16, K::F17, K::F18, K::F19, K::F20, K::F21, K::F22, K::F23, K::F24,
        ];
        Some(match vk {
            0x41..=0x5A => LETTERS[(vk - 0x41) as usize],
            0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
            0x70..=0x87 => F_KEYS[(vk - 0x70) as usize],
            0x20 => K::Space,
            0x0D => K::Enter,
            0x09 => K::Tab,
            0x08 => K::Backspace,
            0x2D => K::Insert,
            0x2E => K::Delete,
            0x24 => K::Home,
            0x23 => K::End,
            0x21 => K::PageUp,
            0x22 => K::PageDown,
            0x25 => K::ArrowLeft,
            0x26 => K::ArrowUp,
            0x27 => K::ArrowRight,
            0x28 => K::ArrowDown,
            _ => return None,
        })
    }
}

/// Пока зажаты только модификаторы: «Ctrl+Alt+…».
pub fn partial_display(m: egui::Modifiers) -> Option<String> {
    let mut parts = Vec::new();
    if m.ctrl {
        parts.push("Ctrl");
    }
    if m.alt {
        parts.push("Alt");
    }
    if m.shift {
        parts.push("Shift");
    }
    (!parts.is_empty()).then(|| format!("{}+…", parts.join("+")))
}

fn is_extra_f_key(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(key, F13 | F14 | F15 | F16 | F17 | F18 | F19 | F20 | F21 | F22 | F23 | F24)
}

fn key_display(key: egui::Key) -> String {
    use egui::Key::*;
    match key {
        Space => "Пробел".into(),
        ArrowUp => "Стрелка вверх".into(),
        ArrowDown => "Стрелка вниз".into(),
        ArrowLeft => "Стрелка влево".into(),
        ArrowRight => "Стрелка вправо".into(),
        Escape => "Esc".into(),
        other => other.name().to_owned(),
    }
}

/// Клавиши, которые можно назначить глобально.
fn key_code(key: egui::Key) -> Option<Code> {
    use egui::Key as K;
    Some(match key {
        K::A => Code::KeyA, K::B => Code::KeyB, K::C => Code::KeyC, K::D => Code::KeyD,
        K::E => Code::KeyE, K::F => Code::KeyF, K::G => Code::KeyG, K::H => Code::KeyH,
        K::I => Code::KeyI, K::J => Code::KeyJ, K::K => Code::KeyK, K::L => Code::KeyL,
        K::M => Code::KeyM, K::N => Code::KeyN, K::O => Code::KeyO, K::P => Code::KeyP,
        K::Q => Code::KeyQ, K::R => Code::KeyR, K::S => Code::KeyS, K::T => Code::KeyT,
        K::U => Code::KeyU, K::V => Code::KeyV, K::W => Code::KeyW, K::X => Code::KeyX,
        K::Y => Code::KeyY, K::Z => Code::KeyZ,
        K::Num0 => Code::Digit0, K::Num1 => Code::Digit1, K::Num2 => Code::Digit2,
        K::Num3 => Code::Digit3, K::Num4 => Code::Digit4, K::Num5 => Code::Digit5,
        K::Num6 => Code::Digit6, K::Num7 => Code::Digit7, K::Num8 => Code::Digit8,
        K::Num9 => Code::Digit9,
        K::F1 => Code::F1, K::F2 => Code::F2, K::F3 => Code::F3, K::F4 => Code::F4,
        K::F5 => Code::F5, K::F6 => Code::F6, K::F7 => Code::F7, K::F8 => Code::F8,
        K::F9 => Code::F9, K::F10 => Code::F10, K::F11 => Code::F11, K::F12 => Code::F12,
        K::F13 => Code::F13, K::F14 => Code::F14, K::F15 => Code::F15, K::F16 => Code::F16,
        K::F17 => Code::F17, K::F18 => Code::F18, K::F19 => Code::F19, K::F20 => Code::F20,
        K::F21 => Code::F21, K::F22 => Code::F22, K::F23 => Code::F23, K::F24 => Code::F24,
        K::Space => Code::Space,
        K::Enter => Code::Enter,
        K::Tab => Code::Tab,
        K::Backspace => Code::Backspace,
        K::Insert => Code::Insert,
        K::Delete => Code::Delete,
        K::Home => Code::Home,
        K::End => Code::End,
        K::PageUp => Code::PageUp,
        K::PageDown => Code::PageDown,
        K::ArrowUp => Code::ArrowUp,
        K::ArrowDown => Code::ArrowDown,
        K::ArrowLeft => Code::ArrowLeft,
        K::ArrowRight => Code::ArrowRight,
        _ => return None,
    })
}

/// Назначения по действиям. `None` — «Не назначено».
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Bindings {
    pub start_pause: Option<Combo>,
    pub stop_record: Option<Combo>,
    pub silence: Option<Combo>,
    pub toggle_widget: Option<Combo>,
    pub toggle_menu: Option<Combo>,
    pub music_play_pause: Option<Combo>,
    pub music_next: Option<Combo>,
    pub music_prev: Option<Combo>,
    pub toggle_notes: Option<Combo>,
}

impl Default for Bindings {
    /// Значения по умолчанию из макета.
    fn default() -> Self {
        let ca = |key| Some(Combo::new(true, true, false, key));
        Self {
            start_pause: ca(egui::Key::Space),
            stop_record: ca(egui::Key::Enter),
            silence: ca(egui::Key::Q),
            toggle_widget: ca(egui::Key::T),
            toggle_menu: ca(egui::Key::O),
            music_play_pause: None,
            music_next: None,
            music_prev: None,
            toggle_notes: ca(egui::Key::N),
        }
    }
}

impl Bindings {
    pub fn get(&self, a: Action) -> Option<&Combo> {
        match a {
            Action::StartPause => self.start_pause.as_ref(),
            Action::StopRecord => self.stop_record.as_ref(),
            Action::Silence => self.silence.as_ref(),
            Action::ToggleWidget => self.toggle_widget.as_ref(),
            Action::ToggleMenu => self.toggle_menu.as_ref(),
            Action::MusicPlayPause => self.music_play_pause.as_ref(),
            Action::MusicNext => self.music_next.as_ref(),
            Action::MusicPrev => self.music_prev.as_ref(),
            Action::ToggleNotes => self.toggle_notes.as_ref(),
        }
    }

    pub fn set(&mut self, a: Action, combo: Option<Combo>) {
        let slot = match a {
            Action::StartPause => &mut self.start_pause,
            Action::StopRecord => &mut self.stop_record,
            Action::Silence => &mut self.silence,
            Action::ToggleWidget => &mut self.toggle_widget,
            Action::ToggleMenu => &mut self.toggle_menu,
            Action::MusicPlayPause => &mut self.music_play_pause,
            Action::MusicNext => &mut self.music_next,
            Action::MusicPrev => &mut self.music_prev,
            Action::ToggleNotes => &mut self.toggle_notes,
        };
        *slot = combo;
    }

    /// Какое другое действие уже использует это сочетание.
    pub fn owner_of(&self, combo: &Combo, except: Action) -> Option<Action> {
        Action::ALL.into_iter().find(|&a| a != except && self.get(a) == Some(combo))
    }
}

// ================================================================ регистрация в системе

pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    registered: Vec<(Action, HotKey)>,
    rx: Receiver<u32>,
}

impl Hotkeys {
    pub fn new(ctx: &egui::Context) -> Self {
        capture::init(ctx);
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.state() == HotKeyState::Pressed {
                let _ = tx.send(e.id());
                ctx.request_repaint();
            }
        }));
        Self { manager: GlobalHotKeyManager::new().ok(), registered: Vec::new(), rx }
    }

    /// Снимает все сочетания (на время записи нового — чтобы нажатия дошли до окна).
    pub fn clear(&mut self) {
        if let Some(m) = &self.manager {
            for (_, hk) in self.registered.drain(..) {
                let _ = m.unregister(hk);
            }
        }
        self.registered.clear();
    }

    /// Регистрирует всё заново. Возвращает действия, сочетания которых
    /// заняты Windows или другой программой.
    pub fn apply(&mut self, enabled: bool, bindings: &Bindings) -> Vec<Action> {
        self.clear();
        let mut failed = Vec::new();
        let Some(m) = &self.manager else { return failed };
        if !enabled {
            return failed;
        }
        for a in Action::ALL {
            let Some(hk) = bindings.get(a).and_then(Combo::to_hotkey) else { continue };
            match m.register(hk) {
                Ok(()) => self.registered.push((a, hk)),
                Err(_) => failed.push(a),
            }
        }
        failed
    }

    pub fn poll(&self) -> Vec<Action> {
        self.rx
            .try_iter()
            .filter_map(|id| self.registered.iter().find(|(_, hk)| hk.id() == id).map(|(a, _)| *a))
            .collect()
    }
}

// ================================================================ трей

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayLook {
    Idle,
    Running,
    Ringing,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TrayState {
    pub widget_visible: bool,
    /// Имя закреплённого таймера; `None` — таймера нет.
    pub timer_name: Option<String>,
    pub running: bool,
    pub ringing: bool,
    pub on_top: bool,
    pub tooltip: String,
    pub look: TrayLook,
    pub accent: Color32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayCommand {
    ToggleWidget,
    StartPause,
    StopRecord,
    Silence,
    ToggleOnTop,
    Exit,
}

const MENU_TOGGLE: &str = "tp-toggle";
const MENU_START: &str = "tp-start";
const MENU_STOP: &str = "tp-stop";
const MENU_SILENCE: &str = "tp-silence";
const MENU_ON_TOP: &str = "tp-on-top";
const MENU_EXIT: &str = "tp-exit";

/// Левый клик по значку срабатывает на отпускании. Двойной клик даёт два
/// отпускания подряд — второе гасим, чтобы двойной клик действовал как один.
const CLICK_DEBOUNCE: Duration = Duration::from_millis(450);

pub struct Tray {
    icon: TrayIcon,
    toggle: MenuItem,
    start: MenuItem,
    stop: MenuItem,
    silence: MenuItem,
    on_top: CheckMenuItem,
    rx: Receiver<TrayCommand>,
    last: Option<TrayState>,
}

impl Tray {
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let toggle = MenuItem::with_id(MENU_TOGGLE, "Скрыть виджет", true, None);
        let start = MenuItem::with_id(MENU_START, "Старт / пауза", true, None);
        let stop = MenuItem::with_id(MENU_STOP, "Стоп и записать", true, None);
        let silence = MenuItem::with_id(MENU_SILENCE, "Тихо", false, None);
        let on_top = CheckMenuItem::with_id(MENU_ON_TOP, "Поверх всех окон", true, true, None);
        let exit = MenuItem::with_id(MENU_EXIT, "Выход", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &toggle,
            &start,
            &stop,
            &silence,
            &PredefinedMenuItem::separator(),
            &on_top,
            &PredefinedMenuItem::separator(),
            &exit,
        ])
        .ok()?;

        let (tx, rx) = channel();
        {
            let tx = tx.clone();
            let ctx = ctx.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let cmd = match e.id().as_ref() {
                    MENU_TOGGLE => TrayCommand::ToggleWidget,
                    MENU_START => TrayCommand::StartPause,
                    MENU_STOP => TrayCommand::StopRecord,
                    MENU_SILENCE => TrayCommand::Silence,
                    MENU_ON_TOP => TrayCommand::ToggleOnTop,
                    MENU_EXIT => TrayCommand::Exit,
                    _ => return,
                };
                let _ = tx.send(cmd);
                ctx.request_repaint();
            }));
        }
        {
            let ctx = ctx.clone();
            let last_click = Mutex::new(Instant::now() - CLICK_DEBOUNCE);
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    let mut last = last_click.lock().unwrap_or_else(|p| p.into_inner());
                    if last.elapsed() >= CLICK_DEBOUNCE {
                        *last = Instant::now();
                        let _ = tx.send(TrayCommand::ToggleWidget);
                        ctx.request_repaint();
                    }
                }
            }));
        }

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Timer Pro")
            .with_icon(render_icon(TrayLook::Idle, t::KNOB())?)
            .build()
            .ok()?;
        Some(Self { icon, toggle, start, stop, silence, on_top, rx, last: None })
    }

    pub fn poll(&self) -> Vec<TrayCommand> {
        self.rx.try_iter().collect()
    }

    /// Обновляет значок, подсказку и пункты меню — только то, что поменялось.
    pub fn update(&mut self, st: TrayState) {
        if self.last.as_ref() == Some(&st) {
            return;
        }
        let prev = self.last.take();
        let changed = |f: fn(&TrayState) -> bool| prev.as_ref().is_none_or(|p| f(p) != f(&st));

        if prev.as_ref().is_none_or(|p| p.look != st.look || p.accent != st.accent) {
            if let Some(icon) = render_icon(st.look, st.accent) {
                let _ = self.icon.set_icon(Some(icon));
            }
        }
        if prev.as_ref().is_none_or(|p| p.tooltip != st.tooltip) {
            let _ = self.icon.set_tooltip(Some(&st.tooltip));
        }
        if changed(|s| s.widget_visible) {
            self.toggle.set_text(if st.widget_visible { "Скрыть виджет" } else { "Показать виджет" });
        }
        if prev.as_ref().is_none_or(|p| p.timer_name != st.timer_name || p.running != st.running) {
            let text = match &st.timer_name {
                Some(name) if st.running => format!("Пауза — «{name}»"),
                Some(name) => format!("Старт — «{name}»"),
                None => "Старт / пауза".to_string(),
            };
            self.start.set_text(text);
            self.start.set_enabled(st.timer_name.is_some());
            self.stop.set_enabled(st.timer_name.is_some());
        }
        if changed(|s| s.ringing) {
            self.silence.set_enabled(st.ringing);
        }
        if changed(|s| s.on_top) {
            self.on_top.set_checked(st.on_top);
        }
        self.last = Some(st);
    }
}

/// Размер значка трея под масштаб системы: 16 px при 100 %, 20 при 125 %, 32 при 200 %.
fn tray_icon_size() -> u32 {
    #[cfg(windows)]
    {
        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() };
        if dpi > 0 {
            return ((16.0 * dpi as f32 / 96.0).round() as u32).clamp(16, 64);
        }
    }
    16
}

fn render_icon(look: TrayLook, accent: Color32) -> Option<Icon> {
    let (disc, ink) = match look {
        TrayLook::Idle => (t::SURFACE_3(), t::TEXT_PRIMARY()),
        TrayLook::Running => (accent, t::on_accent(accent)),
        TrayLook::Ringing => (t::DANGER(), Color32::from_rgb(0x14, 0x15, 0x1F)),
    };
    let size = tray_icon_size();
    Icon::from_rgba(tray_icon_rgba(size, disc, ink), size, size).ok()
}

/// Значок «часы» по сетке 16 px из макета: диск r 7,5; кольцо r 4,25 с обводкой
/// 1,5; стрелка от центра вверх до y 5,25, обводка 1,5, скруглённые концы.
/// Сглаживание — по расстоянию до края фигуры.
pub fn tray_icon_rgba(size: u32, disc: Color32, ink: Color32) -> Vec<u8> {
    let s = size as f32 / 16.0;
    let (cx, cy) = (8.0 * s, 8.0 * s);
    let half_stroke = 0.75 * s;
    let hand_top = 5.25 * s;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let disc_a = (7.5 * s - d + 0.5).clamp(0.0, 1.0);
            let ring_a = (half_stroke - (d - 4.25 * s).abs() + 0.5).clamp(0.0, 1.0);
            let qy = py.clamp(hand_top, cy);
            let ds = ((px - cx).powi(2) + (py - qy).powi(2)).sqrt();
            let hand_a = (half_stroke - ds + 0.5).clamp(0.0, 1.0);
            let ink_a = ring_a.max(hand_a);
            let mix = |a: u8, b: u8| (a as f32 * (1.0 - ink_a) + b as f32 * ink_a).round() as u8;
            out.extend_from_slice(&[
                mix(disc.r(), ink.r()),
                mix(disc.g(), ink.g()),
                mix(disc.b(), ink.b()),
                (disc_a * 255.0).round() as u8,
            ]);
        }
    }
    out
}

// ================================================================ уведомления

/// Идентификатор приложения для Windows: без него система не показывает
/// уведомления обычных (не из Store) программ.
pub const APP_ID: &str = "TimerPro.Widget";

/// Регистрирует программу как источник уведомлений: идентификатор процесса и
/// запись в реестре пользователя с названием и иконкой. Вызывать до создания окон.
#[cfg(windows)]
pub fn register_app_identity() {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID));
    }

    let dir = crate::storage::get_default_path();
    let _ = std::fs::create_dir_all(&dir);
    let icon_path = dir.join("notification-icon.png");
    let icon_bytes: &[u8] = include_bytes!("../resources/timer.png");
    if std::fs::metadata(&icon_path).map(|m| m.len() != icon_bytes.len() as u64).unwrap_or(true) {
        let _ = std::fs::write(&icon_path, icon_bytes);
    }

    let key = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{APP_ID}"));
    let set = |name: &str, value: &str| {
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &key,
                &HSTRING::from(name),
                REG_SZ.0,
                Some(wide.as_ptr() as *const core::ffi::c_void),
                (wide.len() * 2) as u32,
            );
        }
    };
    set("DisplayName", "Timer Pro");
    set("IconUri", &icon_path.display().to_string());

    // Одной записи в реестре Windows мало: уведомления обычной программы
    // показываются, только если в меню «Пуск» есть её ярлык с тем же AppUserModelID.
    let _ = ensure_start_menu_shortcut();
}

/// Ярлык «Timer Pro» в меню «Пуск» пользователя, указывающий на текущий exe,
/// со свойством System.AppUserModel.ID = APP_ID. Обновляется при каждом запуске,
/// чтобы указывать на тот exe, который запущен сейчас.
#[cfg(windows)]
fn ensure_start_menu_shortcut() -> windows::core::Result<()> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    let Ok(exe) = std::env::current_exe() else { return Ok(()) };
    let Some(appdata) = std::env::var_os("APPDATA") else { return Ok(()) };
    let programs = std::path::PathBuf::from(appdata).join("Microsoft").join("Windows").join("Start Menu").join("Programs");
    let _ = std::fs::create_dir_all(&programs);
    let lnk = programs.join("Timer Pro.lnk");

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        if let Some(dir) = exe.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        link.SetDescription(&HSTRING::from("Timer Pro — таймер и секундомер"))?;
        link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
        let store: IPropertyStore = link.cast()?;
        store.SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(APP_ID))?;
        store.Commit()?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn register_app_identity() {}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual><audio silent=\"true\"/></toast>",
        xml_escape(title),
        xml_escape(body)
    )
}

/// Беззвучное уведомление Windows: звенит наш будильник, двойной звук не нужен.
pub fn notify(title: &str, body: &str) {
    let xml = toast_xml(title, body);
    std::thread::spawn(move || {
        let _ = show_toast(&xml);
    });
}

/// То же, но синхронно и с текстом ошибки — для служебной проверки `--test-toast`.
pub fn notify_blocking(title: &str, body: &str) -> Result<(), String> {
    show_toast(&toast_xml(title, body)).map_err(|e| format!("{e:?}"))
}

#[cfg(windows)]
fn show_toast(xml: &str) -> windows::core::Result<()> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)
}

#[cfg(not(windows))]
fn show_toast(_xml: &str) -> Result<(), ()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_display_is_russian_and_compact() {
        assert_eq!(Combo::new(true, true, false, egui::Key::Space).display(), "Ctrl+Alt+Пробел");
        assert_eq!(Combo::new(true, false, true, egui::Key::ArrowUp).display(), "Ctrl+Shift+Стрелка вверх");
        assert_eq!(Combo::new(false, true, false, egui::Key::F5).display(), "Alt+F5");
    }

    #[test]
    fn single_keys_need_ctrl_or_alt_except_f13_to_f24() {
        assert!(!Combo::new(false, false, false, egui::Key::Q).is_acceptable());
        assert!(!Combo::new(false, false, true, egui::Key::Q).is_acceptable());
        assert!(Combo::new(true, false, false, egui::Key::Q).is_acceptable());
        assert!(Combo::new(false, false, false, egui::Key::F13).is_acceptable());
        assert!(!Combo::new(false, false, false, egui::Key::F5).is_acceptable());
    }

    #[test]
    fn default_bindings_have_no_duplicates() {
        let b = Bindings::default();
        for a in Action::ALL {
            if let Some(c) = b.get(a) {
                assert_eq!(b.owner_of(c, a), None, "дубль у {:?}", a);
                assert!(c.to_hotkey().is_some());
            }
        }
    }

    #[test]
    fn owner_of_finds_conflicts() {
        let b = Bindings::default();
        let menu = Combo::new(true, true, false, egui::Key::O);
        assert_eq!(b.owner_of(&menu, Action::MusicNext), Some(Action::ToggleMenu));
        assert_eq!(b.owner_of(&menu, Action::ToggleMenu), None);
    }

    #[test]
    fn bindings_survive_json_with_cleared_slot() {
        let mut b = Bindings::default();
        b.set(Action::Silence, None);
        let json = serde_json::to_string(&b).unwrap();
        let back: Bindings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
        // Отсутствующее поле (старый конфиг) — значение по умолчанию.
        let old: Bindings = serde_json::from_str("{}").unwrap();
        assert_eq!(old, Bindings::default());
    }

    #[test]
    fn tray_icon_has_transparent_corners_and_opaque_center() {
        let size = 16;
        let px = tray_icon_rgba(size, t::SURFACE_3(), t::TEXT_PRIMARY());
        assert_eq!(px.len(), (size * size * 4) as usize);
        assert_eq!(px[3], 0, "угол прозрачный");
        let center = ((8 * size + 8) * 4 + 3) as usize;
        assert_eq!(px[center], 255, "центр непрозрачный");
    }

    #[test]
    fn media_keys_are_accepted_alone_and_registered() {
        let play = Combo::special("MediaPlayPause", false, false, false);
        assert!(play.is_acceptable());
        assert_eq!(play.display(), "Медиа: играть/пауза");
        assert!(play.to_hotkey().is_some());
        assert_eq!(Combo::special("MediaTrackNext", true, false, false).display(), "Ctrl+Медиа: следующий");
    }

    #[test]
    fn numpad_keys_are_distinct_from_top_row_digits() {
        let num5 = Combo::special("Numpad5", true, false, false);
        assert!(num5.is_acceptable());
        assert_eq!(num5.display(), "Ctrl+Num 5");
        assert!(num5.to_hotkey().is_some());
        assert_ne!(num5.to_hotkey(), Combo::new(true, false, false, egui::Key::Num5).to_hotkey());
        // Как и обычная клавиша, без Ctrl/Alt нумпад не назначается.
        assert!(!Combo::special("Numpad5", false, false, false).is_acceptable());
    }

    #[test]
    fn captured_keys_map_from_windows_virtual_codes() {
        capture::set_active(true);
        assert!(capture::on_key_down(0x51, true, true, false)); // Ctrl+Alt+Q
        match capture::take() {
            Some(capture::Captured::Combo(c)) => assert_eq!(c.display(), "Ctrl+Alt+Q"),
            _ => panic!("сочетание не поймано"),
        }
        assert!(capture::on_key_down(0xB3, false, false, false)); // ⏯
        match capture::take() {
            Some(capture::Captured::Combo(c)) => assert_eq!(c.key, "MediaPlayPause"),
            _ => panic!("медиаклавиша не поймана"),
        }
        assert!(capture::on_key_down(0x65, true, false, false)); // Ctrl+Num 5
        match capture::take() {
            Some(capture::Captured::Combo(c)) => assert_eq!(c.display(), "Ctrl+Num 5"),
            _ => panic!("клавиша нумпада не поймана"),
        }
        assert!(capture::on_key_down(0x1B, false, false, false)); // Esc
        assert!(matches!(capture::take(), Some(capture::Captured::Cancel)));
        assert!(capture::on_key_down(0x11, true, false, false)); // один Ctrl — ждём дальше
        assert!(capture::take().is_none());
        capture::set_active(false);
        assert!(!capture::on_key_down(0x51, true, true, false), "вне записи нажатия не трогаем");
    }

    #[test]
    fn xml_is_escaped() {
        assert_eq!(xml_escape("A & <B>"), "A &amp; &lt;B&gt;");
    }
}
