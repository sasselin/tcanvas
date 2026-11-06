//! Draws a terminal's grid at any zoom level.

use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use super::pty::Proxy;
use crate::shell::theme;

const ANSI: [[u8; 3]; 16] = [
    [29, 31, 36],
    [224, 108, 117],
    [152, 195, 121],
    [229, 192, 123],
    [97, 175, 239],
    [198, 120, 221],
    [86, 182, 194],
    [200, 204, 212],
    [92, 99, 112],
    [240, 130, 138],
    [174, 216, 144],
    [240, 208, 145],
    [125, 193, 250],
    [214, 147, 234],
    [110, 202, 214],
    [240, 242, 245],
];

fn c32(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

/// Color of the 256 xterm indices: 16 ANSI, 6×6×6 cube, then 24 grays.
pub fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => theme::rgb(ANSI[i as usize]),
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

fn named(n: NamedColor) -> Color32 {
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground => theme::TEXT,
        NamedColor::Background => theme::CARD_BG,
        NamedColor::Cursor => Color32::from_rgb(200, 210, 230),
        NamedColor::DimForeground => theme::TEXT_DIM,
        other => {
            let idx = other as usize;
            if idx < 16 {
                indexed(idx as u8)
            } else {
                // Dim*: the normal color, darkened.
                let base = indexed((idx - NamedColor::DimBlack as usize) as u8);
                base.gamma_multiply(0.66)
            }
        }
    }
}

pub fn resolve(c: AnsiColor, colors: &Colors) -> Color32 {
    match c {
        AnsiColor::Spec(rgb) => c32(rgb),
        AnsiColor::Indexed(i) => colors[i as usize].map(c32).unwrap_or_else(|| indexed(i)),
        AnsiColor::Named(n) => colors[n].map(c32).unwrap_or_else(|| named(n)),
    }
}

/// Display geometry of the grid on screen.
pub struct GridMetrics {
    pub origin: Pos2,
    pub cell: Vec2,
    pub font: FontId,
    pub clip: Rect,
}

struct Run {
    line: i32,
    col: usize,
    fg: Color32,
    underline: bool,
    strike: bool,
    text: String,
}

pub fn paint_grid(painter: &Painter, term: &Term<Proxy>, m: &GridMetrics, focused: bool) {
    let painter = painter.with_clip_rect(m.clip);
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let colors = content.colors;
    let default_bg = named(NamedColor::Background);
    let cell_pos = |line: i32, col: usize| m.origin + Vec2::new(col as f32 * m.cell.x, line as f32 * m.cell.y);

    let mut runs: Vec<Run> = Vec::new();
    let mut bg_run: Option<(i32, usize, usize, Color32)> = None;
    let flush_bg = |run: Option<(i32, usize, usize, Color32)>| {
        if let Some((line, start, end, color)) = run {
            let min = cell_pos(line, start);
            painter.rect_filled(Rect::from_min_max(min, cell_pos(line + 1, end)), CornerRadius::ZERO, color);
        }
    };

    for indexed in content.display_iter {
        let cell = indexed.cell;
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let line = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

        let mut fg = resolve(cell.fg, colors);
        let mut bg = resolve(cell.bg, colors);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg = fg.gamma_multiply(0.66);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }

        // Backgrounds: rectangles merged per run of the same color.
        let bg = (bg != default_bg).then_some(bg);
        match (&mut bg_run, bg) {
            (Some((l, _, end, color)), Some(b)) if *l == line && *end == col && *color == b => *end = col + width,
            (_, b) => {
                flush_bg(bg_run.take());
                bg_run = b.map(|b| (line, col, col + width, b));
            }
        }

        // Text: one run per style; non-ASCII characters are isolated because the fallback
        // font has a different advance and would shift the rest of the line.
        let c = cell.c;
        let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
        let strike = cell.flags.contains(Flags::STRIKEOUT);
        if c == ' ' && !underline && !strike {
            continue;
        }
        let ascii = c.is_ascii();
        match runs.last_mut() {
            Some(r) if ascii && r.line == line && r.col + r.text.len() == col && r.fg == fg && r.underline == underline && r.strike == strike && r.text.is_ascii() => r.text.push(c),
            _ => runs.push(Run { line, col, fg, underline, strike, text: c.to_string() }),
        }
        if !ascii {
            // Prevents merging with the next character.
            runs.push(Run { line, col: usize::MAX, fg, underline: false, strike: false, text: String::new() });
        }
        if let Some(zw) = cell.zerowidth()
            && let Some(r) = runs.iter_mut().rev().find(|r| !r.text.is_empty())
        {
            r.text.extend(zw);
        }
    }
    flush_bg(bg_run.take());

    for run in runs.iter().filter(|r| !r.text.is_empty()) {
        let pos = cell_pos(run.line, run.col);
        painter.text(pos, Align2::LEFT_TOP, &run.text, m.font.clone(), run.fg);
        let width = run.text.chars().count() as f32 * m.cell.x;
        let stroke = Stroke::new((m.cell.y / 14.0).max(1.0), run.fg);
        if run.underline {
            let y = pos.y + m.cell.y - stroke.width;
            painter.line_segment([Pos2::new(pos.x, y), Pos2::new(pos.x + width, y)], stroke);
        }
        if run.strike {
            let y = pos.y + m.cell.y * 0.55;
            painter.line_segment([Pos2::new(pos.x, y), Pos2::new(pos.x + width, y)], stroke);
        }
    }

    let cursor = content.cursor;
    if cursor.shape != CursorShape::Hidden {
        let min = cell_pos(cursor.point.line.0 + offset, cursor.point.column.0);
        let rect = Rect::from_min_size(min, m.cell);
        let color = colors[NamedColor::Cursor].map(c32).unwrap_or_else(|| named(NamedColor::Cursor));
        let thin = (m.cell.x / 6.0).max(1.0);
        match (focused, cursor.shape) {
            (false, _) | (true, CursorShape::HollowBlock) => {
                painter.rect_stroke(rect, CornerRadius::ZERO, Stroke::new(1.0, color), StrokeKind::Inside);
            }
            (true, CursorShape::Beam) => {
                painter.rect_filled(Rect::from_min_size(min, Vec2::new(thin, m.cell.y)), CornerRadius::ZERO, color);
            }
            (true, CursorShape::Underline) => {
                painter.rect_filled(Rect::from_min_max(Pos2::new(min.x, rect.max.y - thin), rect.max), CornerRadius::ZERO, color);
            }
            (true, _) => {
                painter.rect_filled(rect, CornerRadius::ZERO, color.gamma_multiply(0.7));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_256_palette() {
        assert_eq!(indexed(1), theme::rgb(ANSI[1]));
        assert_eq!(indexed(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(indexed(196), Color32::from_rgb(255, 0, 0));
        assert_eq!(indexed(232), Color32::from_rgb(8, 8, 8));
        assert_eq!(indexed(255), Color32::from_rgb(238, 238, 238));
    }

    #[test]
    fn named_colors() {
        assert_eq!(named(NamedColor::Red), indexed(1));
        assert_eq!(named(NamedColor::BrightBlue), indexed(12));
        assert_eq!(named(NamedColor::Background), theme::CARD_BG);
        assert_ne!(named(NamedColor::DimRed), named(NamedColor::Red));
    }
}
