//! Дизайн-токены из макета `design/Timer Pro.dc.html` (раздел «6. Токены»).
//! Все размеры — логические пиксели при масштабе 1x.

use eframe::egui::{self, Color32, FontFamily, FontId, Stroke};
use std::sync::Arc;

// ---------------------------------------------------------------- цвета

pub const BLOCK_BG: Color32 = Color32::from_rgb(0x1C, 0x1E, 0x2B);
pub const BLOCK_STROKE: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0x0F);
pub const SURFACE_1: Color32 = Color32::from_rgb(0x22, 0x24, 0x2F);
pub const SURFACE_2: Color32 = Color32::from_rgb(0x26, 0x29, 0x38);
pub const SURFACE_3: Color32 = Color32::from_rgb(0x2F, 0x33, 0x44);
pub const SURFACE_SUNKEN: Color32 = Color32::from_rgb(0x16, 0x18, 0x24);
pub const LINE: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0x14);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xE9, 0xE9, 0xED);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x93, 0x97, 0xAB);
pub const TEXT_TERTIARY: Color32 = Color32::from_rgb(0x6A, 0x6E, 0x80);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x46, 0x4A, 0x5A);
pub const KNOB: Color32 = Color32::from_rgb(0xF5, 0xF6, 0xFA);
pub const TOGGLE_OFF: Color32 = Color32::from_rgb(0x3A, 0x3D, 0x4D);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);
pub const DANGER_TINT: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0x6B, 0x6B, 0x29);
pub const SHADOW: Color32 = Color32::from_rgba_unmultiplied_const(0x00, 0x00, 0x00, 0x73);
pub const SCROLL_HANDLE: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 0x29);
const ON_ACCENT_DARK: Color32 = Color32::from_rgb(0x14, 0x15, 0x1F);

/// Обводка виджета при звонке: альфа пульсирует 0x66 ↔ 0xCC.
pub fn danger_stroke(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(0xFF, 0x6B, 0x6B, alpha)
}

/// Оттенок акцента: тот же цвет с альфой поверх фона блока.
pub fn tint(accent: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha)
}

/// accent.tint — фон активной кнопки (play, меню открыто).
pub fn accent_tint(accent: Color32) -> Color32 {
    tint(accent, 0x3D)
}

/// Нажатая кнопка с оттенком акцента: tint 3D → 52.
pub fn accent_tint_pressed(accent: Color32) -> Color32 {
    tint(accent, 0x52)
}

fn relative_luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let x = v as f32 / 255.0;
        if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// Текст на заливке акцентом: #F5F6FA или #14151F — что контрастнее по WCAG.
pub fn on_accent(accent: Color32) -> Color32 {
    let l = relative_luminance(accent);
    let vs_light = 1.05 / (l + 0.05);
    let vs_dark = (l + 0.05) / (relative_luminance(ON_ACCENT_DARK) + 0.05);
    if vs_light > vs_dark { KNOB } else { ON_ACCENT_DARK }
}

/// Готовые цвета акцента из «Опций».
pub const SWATCHES: [[u8; 3]; 8] = [
    [0x64, 0x78, 0xFF],
    [0x91, 0x84, 0xD9],
    [0x3F, 0xB8, 0xAF],
    [0x5B, 0xBF, 0x6A],
    [0xE0, 0xA8, 0x4F],
    [0xE3, 0x73, 0x5E],
    [0xD9, 0x6B, 0xA8],
    [0x8A, 0x93, 0xA8],
];

// ---------------------------------------------------------------- размеры

pub const WINDOW_W: f32 = 350.0;
pub const WIDGET_H: f32 = 64.0;
/// Плашка «Сейчас играет» чуть уже виджета и стоит по его центру: её края
/// совпадают с внутренним отступом виджета (12 px) и с треком вкладок меню.
pub const MUSIC_W: f32 = 326.0;
pub const MUSIC_H: f32 = 56.0;
pub const MUSIC_MINI_H: f32 = 32.0;
/// v2: самая длинная вкладка, «Система», занимает 531 px.
pub const MENU_H: f32 = 540.0;
/// Панель заметок сбоку от виджета — той же ширины. Высота — от виджета
/// до дальнего края меню настроек (считается в app.rs).
pub const NOTES_W: f32 = WINDOW_W;
/// Зазор между блоками настраивается в «Опциях» в этих пределах:
/// не впритык и не слишком далеко.
pub const BLOCK_GAP_MIN: f32 = 4.0;
pub const BLOCK_GAP_MAX: f32 = 16.0;

/// Поля окна вокруг стопки блоков. Раньше здесь было место под тень блоков,
/// но поверх светлых обоев тень читалась как тёмная подложка вокруг и между
/// блоками, поэтому тени нет и окно ровно по размеру стопки.
pub const PAD_X: f32 = 0.0;
pub const PAD_TOP: f32 = 0.0;
pub const PAD_BOTTOM: f32 = 0.0;

pub const RADIUS_BLOCK: u8 = 14;
pub const RADIUS_BTN_LG: u8 = 10;
pub const RADIUS_CTRL: u8 = 8;
pub const RADIUS_CHIP: u8 = 6;

/// Внутренний отступ блока: 11 + 1 px обводки.
pub const BLOCK_PAD: f32 = 12.0;

pub fn block_shadow() -> egui::Shadow {
    egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: SHADOW }
}

pub fn block_stroke() -> Stroke {
    Stroke::new(1.0, BLOCK_STROKE)
}

// ---------------------------------------------------------------- анимации

/// Все твины в макете — ease-out cubic: 1 − (1 − t)³.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub const ANIM_MENU_OPEN: f32 = 0.20;
pub const ANIM_MENU_CLOSE: f32 = 0.15;
pub const ANIM_MUSIC_SHOW: f32 = 0.20;
pub const ANIM_MUSIC_HIDE: f32 = 0.15;
pub const ANIM_MUSIC_RESIZE: f32 = 0.18;
pub const ANIM_TAB: f32 = 0.15;
pub const ANIM_HOVER: f32 = 0.12;
pub const ANIM_PRESS: f32 = 0.08;
pub const ANIM_TOGGLE: f32 = 0.15;
pub const ANIM_ROW_ADD: f32 = 0.18;
pub const ANIM_ROW_FADE: f32 = 0.12;
pub const ANIM_ROW_COLLAPSE: f32 = 0.15;
pub const ANIM_PROGRESS: f32 = 0.25;
/// Плашка доезжает до места после перетаскивания; меню сдвигается следом.
pub const ANIM_MUSIC_SNAP: f32 = 0.20;
pub const ANIM_MENU_SHIFT: f32 = 0.18;

// v2: перемотка, бегущая строка, поле горячей клавиши.
pub const ANIM_SEEK_ENTER: f32 = 0.12;
pub const ANIM_SEEK_LEAVE: f32 = 0.15;
pub const ANIM_SEEK_GRAB: f32 = 0.08;
pub const ANIM_SEEK_CLICK: f32 = 0.15;
pub const ANIM_TIME_LABEL_IN: f32 = 0.12;
pub const ANIM_TIME_LABEL_OUT: f32 = 0.15;
/// После отпускания подпись времени держится, потом сменяется исполнителем.
pub const SEEK_LABEL_HOLD: f32 = 0.80;
pub const MARQUEE_SPEED: f32 = 30.0;
pub const MARQUEE_START_PAUSE: f32 = 1.2;
pub const MARQUEE_END_PAUSE: f32 = 1.0;
pub const MARQUEE_RETURN: f32 = 0.30;
pub const MARQUEE_FADE: f32 = 16.0;
pub const ANIM_EDGE_FADE: f32 = 0.12;
pub const ANIM_HINT_ROW_IN: f32 = 0.18;
pub const ANIM_HINT_ROW_OUT: f32 = 0.15;
/// Конфликт с системой виден 2,5 с, затем поле возвращается к прежнему значению.
pub const HOTKEY_SYSTEM_CONFLICT_HOLD: f32 = 2.5;

// ---------------------------------------------------------------- шрифты

const FAMILY_REGULAR: &str = "inter";
const FAMILY_MEDIUM: &str = "inter-medium";
const FAMILY_MONO: &str = "mono";
const FAMILY_MONO_MEDIUM: &str = "mono-medium";
const FAMILY_ICON: &str = "phosphor";
const FAMILY_ICON_FILL: &str = "phosphor-fill";

fn family(name: &str) -> FontFamily {
    FontFamily::Name(name.into())
}

/// Inter Regular.
pub fn regular(size: f32) -> FontId {
    FontId::new(size, family(FAMILY_REGULAR))
}

/// Inter Medium.
pub fn medium(size: f32) -> FontId {
    FontId::new(size, family(FAMILY_MEDIUM))
}

/// JetBrains Mono Regular.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, family(FAMILY_MONO))
}

/// JetBrains Mono Medium.
pub fn mono_medium(size: f32) -> FontId {
    FontId::new(size, family(FAMILY_MONO_MEDIUM))
}

/// Иконка Phosphor: Regular или Fill.
pub fn icon(size: f32, fill: bool) -> FontId {
    FontId::new(size, family(if fill { FAMILY_ICON_FILL } else { FAMILY_ICON }))
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let default_prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let default_mono = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();

    #[cfg(has_brand_fonts)]
    let (regular_keys, medium_keys, mono_keys, mono_medium_keys) = {
        let mut add = |key: &str, bytes: &'static [u8]| {
            fonts.font_data.insert(key.to_owned(), Arc::new(egui::FontData::from_static(bytes)));
        };
        add("Inter-Regular", include_bytes!("../resources/fonts/Inter-Regular.ttf"));
        add("Inter-Medium", include_bytes!("../resources/fonts/Inter-Medium.ttf"));
        add("JetBrainsMono-Regular", include_bytes!("../resources/fonts/JetBrainsMono-Regular.ttf"));
        add("JetBrainsMono-Medium", include_bytes!("../resources/fonts/JetBrainsMono-Medium.ttf"));
        // Встроенные шрифты egui остаются запасными — для эмодзи и редких символов.
        let with = |first: &str, rest: &[String]| {
            let mut v = vec![first.to_owned()];
            v.extend(rest.iter().cloned());
            v
        };
        (
            with("Inter-Regular", &default_prop),
            with("Inter-Medium", &default_prop),
            with("JetBrainsMono-Regular", &default_mono),
            with("JetBrainsMono-Medium", &default_mono),
        )
    };

    // Шрифтов из макета нет — подставляем встроенные шрифты egui.
    #[cfg(not(has_brand_fonts))]
    let (regular_keys, medium_keys, mono_keys, mono_medium_keys) = (
        default_prop.clone(),
        default_prop.clone(),
        default_mono.clone(),
        default_mono.clone(),
    );

    // Стандартные семейства тоже переключаем — ими пользуются поля ввода и подсказки.
    fonts.families.insert(FontFamily::Proportional, regular_keys.clone());
    fonts.families.insert(FontFamily::Monospace, mono_keys.clone());
    fonts.families.insert(family(FAMILY_REGULAR), regular_keys);
    fonts.families.insert(family(FAMILY_MEDIUM), medium_keys);
    fonts.families.insert(family(FAMILY_MONO), mono_keys);
    fonts.families.insert(family(FAMILY_MONO_MEDIUM), mono_medium_keys);

    // Regular и Fill у Phosphor используют одни и те же коды символов,
    // поэтому каждый вариант живёт в собственном семействе.
    fonts.font_data.insert(
        FAMILY_ICON.into(),
        Arc::new(egui_phosphor::Variant::Regular.font_data()),
    );
    fonts.font_data.insert(
        FAMILY_ICON_FILL.into(),
        Arc::new(egui_phosphor::Variant::Fill.font_data()),
    );
    fonts.families.insert(family(FAMILY_ICON), vec![FAMILY_ICON.into()]);
    fonts.families.insert(family(FAMILY_ICON_FILL), vec![FAMILY_ICON_FILL.into()]);

    ctx.set_fonts(fonts);
}

pub fn apply_style(ctx: &egui::Context, accent: Color32) {
    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::dark();
        let v = &mut style.visuals;
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = BLOCK_BG;
        v.window_stroke = block_stroke();
        v.window_shadow = block_shadow();
        v.popup_shadow = block_shadow();
        v.window_corner_radius = RADIUS_CTRL.into();
        v.menu_corner_radius = RADIUS_CTRL.into();
        v.extreme_bg_color = SURFACE_2;
        v.override_text_color = None;
        v.hyperlink_color = accent;
        v.selection.bg_fill = accent_tint_pressed(accent);
        v.selection.stroke = Stroke::new(1.0, accent);
        v.text_cursor.stroke = Stroke::new(1.5, TEXT_PRIMARY);

        // Полоса прокрутки берёт цвет из bg_fill виджетов.
        v.widgets.inactive.bg_fill = SCROLL_HANDLE;
        v.widgets.inactive.weak_bg_fill = SURFACE_2;
        v.widgets.hovered.bg_fill = tint(Color32::WHITE, 0x40);
        v.widgets.hovered.weak_bg_fill = SURFACE_3;
        v.widgets.active.bg_fill = tint(Color32::WHITE, 0x52);
        v.widgets.active.weak_bg_fill = SURFACE_3;
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

        style.spacing.item_spacing = egui::vec2(8.0, 4.0);
        style.spacing.scroll = egui::style::ScrollStyle {
            floating: true,
            bar_width: 3.0,
            floating_width: 3.0,
            floating_allocated_width: 0.0,
            handle_min_length: 24.0,
            bar_inner_margin: 0.0,
            bar_outer_margin: 0.0,
            foreground_color: false,
            dormant_background_opacity: 0.0,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.0,
            dormant_handle_opacity: 1.0,
            active_handle_opacity: 1.0,
            interact_handle_opacity: 1.0,
            ..egui::style::ScrollStyle::floating()
        };

        use egui::TextStyle;
        style.text_styles = [
            (TextStyle::Small, regular(11.0)),
            (TextStyle::Body, regular(13.0)),
            (TextStyle::Button, medium(13.0)),
            (TextStyle::Heading, medium(15.0)),
            (TextStyle::Monospace, mono(13.0)),
        ]
        .into();

        style.interaction.selectable_labels = false;
        style.animation_time = ANIM_HOVER;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_hits_endpoints_and_is_monotonic() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "ease-out быстрее линейного в середине");
        assert_eq!(ease_out(2.0), 1.0);
    }

    #[test]
    fn on_accent_picks_readable_text() {
        // Тёмный фиолетовый → светлый текст, жёлтый → тёмный. Формула та же,
        // что в макете, поэтому и для акцента по умолчанию #6478FF выходит тёмный.
        assert_eq!(on_accent(Color32::from_rgb(0x8A, 0x2B, 0xE2)), KNOB);
        assert_eq!(on_accent(Color32::from_rgb(0xE0, 0xA8, 0x4F)), ON_ACCENT_DARK);
        assert_eq!(on_accent(Color32::from_rgb(0x64, 0x78, 0xFF)), ON_ACCENT_DARK);
    }
}
