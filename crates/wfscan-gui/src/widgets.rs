//! Small drawing helpers.

use egui::{Color32, Pos2, RichText, Sense, Stroke, Ui, Vec2};
use wfscan_core::{Kind, Status};

/// `12.3 MiB`, `456 B`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

pub fn kind_color(kind: Kind) -> Color32 {
    match kind {
        Kind::Model => Color32::from_rgb(120, 170, 255),
        Kind::Texture => Color32::from_rgb(120, 210, 140),
        Kind::Material => Color32::from_rgb(230, 180, 90),
        Kind::Audio => Color32::from_rgb(210, 140, 220),
        Kind::Level | Kind::LevelStatic | Kind::Landscape => Color32::from_rgb(230, 130, 110),
        Kind::Shader | Kind::Icon => Color32::from_rgb(160, 200, 210),
    }
}

pub fn status_color(status: Status, ui: &Ui) -> Color32 {
    match status {
        Status::Ok => Color32::from_rgb(110, 200, 120),
        Status::Skipped => Color32::from_rgb(140, 170, 140),
        Status::Failed => ui.visuals().error_fg_color,
        Status::Unsupported => ui.visuals().weak_text_color(),
    }
}

/// One line of a hex dump: offset, sixteen bytes, their text.
pub fn hex_line(offset: usize, bytes: &[u8]) -> String {
    let mut line = format!("{offset:06x}  ");
    for i in 0..16 {
        match bytes.get(i) {
            Some(b) => line.push_str(&format!("{b:02x} ")),
            None => line.push_str("   "),
        }
        if i == 7 {
            line.push(' ');
        }
    }
    line.push(' ');
    line.extend(bytes.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }));
    line
}

/// A scrolling hex dump of `bytes`, drawing only the rows in view.
pub fn hex_view(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, bytes: &[u8]) {
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    let rows = bytes.len().div_ceil(16);
    egui::ScrollArea::both().id_salt(id).auto_shrink(false).show_rows(ui, row_height, rows, |ui, range| {
        for row in range {
            let start = row * 16;
            ui.add(egui::Label::new(egui::RichText::new(hex_line(start, &bytes[start..bytes.len().min(start + 16)])).monospace()).extend());
        }
    });
}

/// Text split into lines for [`text_view`]. Only the first `limit` bytes are kept.
pub struct TextLines {
    text: String,
    /// Start and end of each line in `text`.
    lines: Vec<(usize, usize)>,
    pub truncated: bool,
}

impl TextLines {
    pub fn new(bytes: &[u8], limit: usize) -> Self {
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]).into_owned();
        let mut lines = Vec::new();
        let mut start = 0;
        for (i, _) in text.match_indices('\n') {
            lines.push((start, i));
            start = i + 1;
        }
        if start < text.len() {
            lines.push((start, text.len()));
        }
        Self { text, lines, truncated: bytes.len() > limit }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Scrolling text, drawing only the lines in view. Bytes that aren't text show as dots.
pub fn text_view(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, text: &TextLines) {
    const MAX_LINE: usize = 2000;
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::both().id_salt(id).auto_shrink(false).show_rows(ui, row_height, text.lines.len(), |ui, range| {
        for &(a, b) in &text.lines[range] {
            let shown = |c: char| if c == '\t' { ' ' } else if c.is_control() || c == '\u{fffd}' { '.' } else { c };
            let line: String = text.text[a..b].chars().take(MAX_LINE).map(shown).collect();
            ui.add(egui::Label::new(RichText::new(line).monospace()).extend());
        }
    });
}

/// A waveform from peaks (0 to 1), with the play position as a line. Returns where it
/// was clicked, from 0 to 1.
pub fn waveform(ui: &mut Ui, peaks: &[f32], position: Option<f32>, height: f32) -> Option<f32> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "waveform"));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    let mid = rect.center().y;
    let half = rect.height() / 2.0 - 2.0;
    let color = ui.visuals().selection.bg_fill;
    let columns = rect.width().max(1.0) as usize;
    if !peaks.is_empty() {
        for column in 0..columns {
            let from = column * peaks.len() / columns;
            let to = ((column + 1) * peaks.len() / columns).max(from + 1).min(peaks.len());
            let peak = peaks[from..to].iter().fold(0f32, |a, &b| a.max(b));
            let x = rect.left() + column as f32 + 0.5;
            painter.line_segment([Pos2::new(x, mid - peak * half - 0.5), Pos2::new(x, mid + peak * half + 0.5)], Stroke::new(1.0, color));
        }
    }
    if let Some(position) = position {
        let x = rect.left() + position.clamp(0.0, 1.0) * rect.width();
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, ui.visuals().strong_text_color()));
    }
    let clicked = response.clicked().then(|| response.interact_pointer_pos()).flatten();
    clicked.map(|pos| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 << 30), "5.0 GiB");
        assert_eq!(human_size(5000 << 30), "5000.0 GiB");
    }

    #[test]
    fn hex_lines() {
        // Seven bytes, then the gap nine missing ones leave, then the text.
        assert_eq!(hex_line(0x10, b"DDS |\x00\x01"), format!("000010  44 44 53 20 7c 00 01{}DDS |..", " ".repeat(30)));
        let full: Vec<u8> = (0x41..0x51).collect();
        assert_eq!(hex_line(0, &full), "000000  41 42 43 44 45 46 47 48  49 4a 4b 4c 4d 4e 4f 50  ABCDEFGHIJKLMNOP");
    }
}
