//! Собственные виджеты по спецификации макета. Все рисуются в заданном
//! прямоугольнике: размеры в макете точные, поэтому раскладка абсолютная.

use crate::theme::{self as t, ease_out};
use eframe::egui::{
    self, text::LayoutJob, Align2, Color32, CursorIcon, FontId, Galley, Id, Painter, Pos2, Rect,
    Response, Sense, Stroke, StrokeKind, TextFormat, Ui,
};
use std::sync::Arc;

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

/// Плавные hover (120 мс) и нажатие (80 мс) для любого интерактивного элемента.
pub struct Interaction {
    pub hover: f32,
    pub press: f32,
}

pub fn interaction(ui: &Ui, resp: &Response, enabled: bool) -> Interaction {
    let ctx = ui.ctx();
    let hover = ctx.animate_bool_with_time(resp.id.with("hover"), enabled && resp.hovered(), t::ANIM_HOVER);
    let press = ctx.animate_bool_with_time(
        resp.id.with("press"),
        enabled && resp.is_pointer_button_down_on(),
        t::ANIM_PRESS,
    );
    Interaction { hover, press }
}

// ---------------------------------------------------------------- текст

pub fn galley(ui: &Ui, text: &str, font: FontId, color: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_owned(), font, color)
}

/// Однострочный текст, обрезанный многоточием по ширине.
pub fn galley_trunc(ui: &Ui, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat { font_id: font, color, ..Default::default() },
    );
    job.wrap.max_width = max_width.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.painter().layout_job(job)
}

pub fn paint_galley(painter: &Painter, pos: Pos2, anchor: Align2, galley: Arc<Galley>, color: Color32) -> Rect {
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

/// Многострочный текст, выровненный по центру относительно `top_center`.
/// При `halign = Center` egui уже раскладывает строки вокруг x = 0, поэтому
/// обычная привязка CENTER_TOP сдвигала текст влево второй раз и обрезала его.
pub fn paint_galley_centered(painter: &Painter, top_center: Pos2, galley: Arc<Galley>, color: Color32) {
    let x = top_center.x - galley.rect.width() / 2.0 - galley.rect.min.x;
    painter.galley(Pos2::new(x, top_center.y), galley, color);
}

pub fn text(ui: &Ui, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32) -> Rect {
    let g = galley(ui, s, font, color);
    paint_galley(ui.painter(), pos, anchor, g, color)
}

pub fn text_trunc(ui: &Ui, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32, max_width: f32) -> Rect {
    let g = galley_trunc(ui, s, font, color, max_width);
    paint_galley(ui.painter(), pos, anchor, g, color)
}

/// Заголовок группы: Inter Medium 11, ВЕРХНИЙ РЕГИСТР, разрядка 0.6.
pub fn overline(ui: &Ui, left_center: Pos2, s: &str) {
    let job = LayoutJob::single_section(
        s.to_uppercase(),
        TextFormat {
            font_id: t::medium(11.0),
            extra_letter_spacing: 0.6,
            color: t::TEXT_TERTIARY(),
            ..Default::default()
        },
    );
    let g = ui.painter().layout_job(job);
    paint_galley(ui.painter(), left_center, Align2::LEFT_CENTER, g, t::TEXT_TERTIARY());
}

// ---------------------------------------------------------------- блоки

/// Парящий блок: заливка block.bg, обводка 1 px внутрь, радиус 14.
/// Без тени: вокруг блоков должен быть виден рабочий стол, а не тёмный ореол.
pub fn paint_block(painter: &Painter, rect: Rect, stroke: Color32) {
    painter.rect(rect, t::RADIUS_BLOCK, t::BLOCK_BG(), Stroke::new(1.0, stroke), StrokeKind::Inside);
}

// ---------------------------------------------------------------- кнопки

#[derive(Clone, Copy)]
pub struct ButtonStyle {
    pub fill: Color32,
    pub hover_fill: Color32,
    pub press_fill: Color32,
    pub color: Color32,
    pub hover_color: Color32,
    pub stroke: Option<Color32>,
    pub radius: u8,
}

impl ButtonStyle {
    /// Вторичная кнопка: surface.2, при наведении surface.3.
    pub fn secondary(radius: u8) -> Self {
        Self {
            fill: t::SURFACE_2(),
            hover_fill: t::SURFACE_3(),
            press_fill: lerp(t::SURFACE_3(), Color32::BLACK, 0.15),
            color: t::TEXT_SECONDARY(),
            hover_color: t::TEXT_PRIMARY(),
            stroke: None,
            radius,
        }
    }

    /// Вторичная кнопка с основным цветом текста.
    pub fn secondary_bright(radius: u8) -> Self {
        Self { color: t::TEXT_PRIMARY(), ..Self::secondary(radius) }
    }

    /// Кнопка-иконка без фона: transparent → surface.2.
    pub fn ghost(color: Color32, radius: u8) -> Self {
        Self {
            fill: Color32::TRANSPARENT,
            hover_fill: t::SURFACE_2(),
            press_fill: t::SURFACE_3(),
            color,
            hover_color: t::TEXT_PRIMARY(),
            stroke: None,
            radius,
        }
    }

    /// Удаление: при наведении красное на красном оттенке.
    pub fn danger_ghost(radius: u8) -> Self {
        Self {
            fill: Color32::TRANSPARENT,
            hover_fill: t::DANGER_TINT(),
            press_fill: t::tint(t::DANGER(), 0x3D),
            color: t::TEXT_TERTIARY(),
            hover_color: t::DANGER(),
            stroke: None,
            radius,
        }
    }

    /// Активная кнопка: accent.tint + иконка акцентом. Нажатие: tint 3D → 52.
    pub fn accent(accent: Color32, radius: u8) -> Self {
        Self {
            fill: t::accent_tint(accent),
            hover_fill: t::tint(accent, 0x48),
            press_fill: t::accent_tint_pressed(accent),
            color: accent,
            hover_color: accent,
            stroke: None,
            radius,
        }
    }

    /// Контурная кнопка «+ Таймер»: обводка и текст акцентом.
    pub fn accent_outline(accent: Color32, radius: u8) -> Self {
        Self {
            fill: Color32::TRANSPARENT,
            hover_fill: t::tint(accent, 0x1F),
            press_fill: t::accent_tint(accent),
            color: accent,
            hover_color: accent,
            stroke: Some(accent),
            radius,
        }
    }

    /// Пресет: обводка line, при наведении surface.2.
    pub fn chip() -> Self {
        Self {
            fill: Color32::TRANSPARENT,
            hover_fill: t::SURFACE_2(),
            press_fill: t::SURFACE_3(),
            color: t::TEXT_SECONDARY(),
            hover_color: t::TEXT_PRIMARY(),
            stroke: Some(t::LINE()),
            radius: t::RADIUS_CHIP,
        }
    }

    /// «Тихо»: красный оттенок, красный текст.
    pub fn danger_solid() -> Self {
        Self {
            fill: t::DANGER_TINT(),
            hover_fill: t::tint(t::DANGER(), 0x3D),
            press_fill: t::tint(t::DANGER(), 0x52),
            color: t::DANGER(),
            hover_color: t::DANGER(),
            stroke: None,
            radius: t::RADIUS_BTN_LG,
        }
    }
}

pub enum Content<'a> {
    Icon { glyph: &'a str, size: f32, fill: bool },
    Text { label: &'a str, font: FontId },
    IconText { glyph: &'a str, size: f32, fill: bool, label: &'a str, font: FontId, gap: f32 },
}

fn paint_content(ui: &Ui, painter: &Painter, rect: Rect, content: &Content, color: Color32) {
    match content {
        Content::Icon { glyph, size, fill } => {
            painter.text(rect.center(), Align2::CENTER_CENTER, *glyph, t::icon(*size, *fill), color);
        }
        Content::Text { label, font } => {
            painter.text(rect.center(), Align2::CENTER_CENTER, *label, font.clone(), color);
        }
        Content::IconText { glyph, size, fill, label, font, gap } => {
            let ig = galley(ui, glyph, t::icon(*size, *fill), color);
            let tg = galley(ui, label, font.clone(), color);
            let total = ig.size().x + gap + tg.size().x;
            let x0 = rect.center().x - total / 2.0;
            let cy = rect.center().y;
            let ix = ig.size().x;
            paint_galley(painter, Pos2::new(x0, cy), Align2::LEFT_CENTER, ig, color);
            paint_galley(painter, Pos2::new(x0 + ix + gap, cy), Align2::LEFT_CENTER, tg, color);
        }
    }
}

/// Кнопка в заданном прямоугольнике. Неактивная — прозрачность 0.45, не кликается.
pub fn button(ui: &Ui, rect: Rect, id: Id, content: Content, style: ButtonStyle, enabled: bool) -> Response {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let it = interaction(ui, &resp, enabled);
    let fill = lerp(lerp(style.fill, style.hover_fill, it.hover), style.press_fill, it.press);
    let color = lerp(style.color, style.hover_color, it.hover);
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(0.45);
    }
    let stroke = style.stroke.map(|c| Stroke::new(1.0, c)).unwrap_or(Stroke::NONE);
    painter.rect(rect, style.radius, fill, stroke, StrokeKind::Inside);
    paint_content(ui, &painter, rect, &content, color);
    resp
}

// ---------------------------------------------------------------- переключатель

/// Переключатель 32×18: бегунок 14 px едет 2 → 16, трек toggle.off → accent.
pub fn paint_switch(painter: &Painter, rect: Rect, t_on: f32, accent: Color32) {
    let track = lerp(t::TOGGLE_OFF(), accent, t_on);
    let knob = lerp(t::TEXT_SECONDARY(), t::KNOB(), t_on);
    painter.rect_filled(rect, 9u8, track);
    let cx = rect.left() + 2.0 + 7.0 + 14.0 * t_on;
    painter.circle_filled(Pos2::new(cx, rect.center().y), 7.0, knob);
}

/// Строка опции высотой 32 с переключателем справа. Кликабельна вся строка.
pub fn toggle_row(
    ui: &Ui,
    rect: Rect,
    id: Id,
    label: &str,
    indent: f32,
    value: &mut bool,
    accent: Color32,
    enabled: bool,
) -> bool {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let mut changed = false;
    if enabled && resp.clicked() {
        *value = !*value;
        changed = true;
    }
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let on = ui.ctx().animate_bool_with_time(id.with("knob"), *value, t::ANIM_TOGGLE);
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(0.45);
    }
    let color = if indent > 0.0 { t::TEXT_SECONDARY() } else { t::TEXT_PRIMARY() };
    let g = galley_trunc(ui, label, t::regular(13.0), color, rect.width() - indent - 44.0);
    paint_galley(&painter, Pos2::new(rect.left() + indent, rect.center().y), Align2::LEFT_CENTER, g, color);
    let sw = Rect::from_min_size(
        Pos2::new(rect.right() - 32.0, rect.center().y - 9.0),
        egui::vec2(32.0, 18.0),
    );
    paint_switch(&painter, sw, on, accent);
    changed
}

// ---------------------------------------------------------------- слайдер

pub struct SliderOutput {
    pub changed: bool,
    pub released: bool,
}

/// Слайдер: трек 4 px toggle.off, заливка акцентом, бегунок 14 px.
pub fn slider(ui: &Ui, rect: Rect, id: Id, value: &mut f32, accent: Color32) -> SliderOutput {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let left = rect.left() + 7.0;
    let right = rect.right() - 7.0;
    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let v = ((p.x - left) / (right - left)).clamp(0.0, 1.0);
            if (v - *value).abs() > f32::EPSILON {
                *value = v;
                changed = true;
            }
        }
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    let cy = rect.center().y;
    let track = Rect::from_min_max(Pos2::new(rect.left(), cy - 2.0), Pos2::new(rect.right(), cy + 2.0));
    painter.rect_filled(track, 2u8, t::TOGGLE_OFF());
    let kx = left + (right - left) * value.clamp(0.0, 1.0);
    let filled = Rect::from_min_max(track.min, Pos2::new(kx, track.max.y));
    painter.rect_filled(filled, 2u8, accent);
    painter.circle_filled(Pos2::new(kx, cy), 7.0, t::KNOB());
    SliderOutput { changed, released: resp.drag_stopped() || resp.clicked() }
}

// ---------------------------------------------------------------- поля Ч / М / С

/// Поле часов/минут/секунд 52 px высотой.
/// Клик — фокус и ввод цифр, Tab — следующее поле, Enter/Esc — выход.
/// Перетаскивание по вертикали — шаг 1 на 8 px (вверх больше), колесо — ±1.
pub fn hms_field(ui: &Ui, rect: Rect, id: Id, value: &mut u64, max: u64, unit: &str, accent: Color32) -> bool {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let fresh_id = id.with("fresh");
    let accum_id = id.with("drag_accum");
    let mut changed = false;
    let assign = |v: &mut u64, nv: u64| -> bool {
        let nv = nv.min(max);
        if *v != nv {
            *v = nv;
            true
        } else {
            false
        }
    };

    ui.memory_mut(|m| m.interested_in_focus(id, ui.layer_id()));
    if resp.clicked() {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let focused = ui.memory(|m| m.has_focus(id));
    if resp.gained_focus() || resp.clicked() {
        // Первая цифра после фокуса заменяет значение, а не дописывается.
        ui.data_mut(|d| d.insert_temp(fresh_id, true));
    }

    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if resp.drag_started() {
        ui.data_mut(|d| d.insert_temp(accum_id, 0.0f32));
    }
    if resp.dragged() {
        let mut acc: f32 = ui.data(|d| d.get_temp(accum_id)).unwrap_or(0.0);
        acc += -resp.drag_delta().y;
        let steps = (acc / 8.0).trunc();
        if steps != 0.0 {
            let nv = (*value as i64 + steps as i64).clamp(0, max as i64) as u64;
            changed |= assign(value, nv);
            acc -= steps * 8.0;
        }
        ui.data_mut(|d| d.insert_temp(accum_id, acc));
    }

    if resp.hovered() {
        let wheel: f32 = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                    _ => None,
                })
                .sum()
        });
        let current = *value;
        if wheel > 0.0 {
            changed |= assign(value, current + 1);
        } else if wheel < 0.0 {
            changed |= assign(value, current.saturating_sub(1));
        }
    }

    if focused {
        let events = ui.input(|i| i.events.clone());
        let mut fresh: bool = ui.data(|d| d.get_temp(fresh_id)).unwrap_or(false);
        for e in events {
            let current = *value;
            match e {
                egui::Event::Text(s) => {
                    for d in s.chars().filter_map(|c| c.to_digit(10)) {
                        let d = d as u64;
                        let current = *value;
                        let nv = if fresh {
                            d
                        } else {
                            let joined = current * 10 + d;
                            if joined <= max { joined } else { d }
                        };
                        fresh = false;
                        changed |= assign(value, nv);
                    }
                }
                egui::Event::Key { key, pressed: true, .. } => match key {
                    egui::Key::Backspace => {
                        fresh = false;
                        changed |= assign(value, current / 10);
                    }
                    egui::Key::ArrowUp => changed |= assign(value, current + 1),
                    egui::Key::ArrowDown => changed |= assign(value, current.saturating_sub(1)),
                    egui::Key::Enter | egui::Key::Escape => {
                        ui.memory_mut(|m| m.surrender_focus(id));
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        ui.data_mut(|d| d.insert_temp(fresh_id, fresh));
    }

    let it = interaction(ui, &resp, true);
    let stroke = if focused { accent } else { lerp(t::BLOCK_STROKE(), t::LINE(), it.hover) };
    let painter = ui.painter();
    painter.rect(rect, t::RADIUS_CTRL, t::SURFACE_2(), Stroke::new(1.0, stroke), StrokeKind::Inside);
    let value_color = if focused { t::TEXT_PRIMARY() } else { t::TEXT_SECONDARY() };
    let top = rect.center().y - 19.0;
    text(ui, Pos2::new(rect.center().x, top + 13.0), Align2::CENTER_CENTER, &format!("{:02}", value), t::mono_medium(22.0), value_color);
    text(ui, Pos2::new(rect.center().x, top + 32.0), Align2::CENTER_CENTER, unit, t::regular(10.0), t::TEXT_TERTIARY());
    changed
}

// ---------------------------------------------------------------- анимации строк

/// Прогресс твина по времени, уже с ease-out.
pub fn tween(start: std::time::Instant, secs: f32) -> f32 {
    ease_out(start.elapsed().as_secs_f32() / secs)
}
