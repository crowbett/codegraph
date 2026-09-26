//! Output plumbing: the `--json` envelope, ANSI styling, aligned tables.

use serde::Serialize;
use serde_json::json;

use crate::config::ColorMode;

/// Decide whether to emit ANSI colors.
pub fn color_enabled(
    mode: ColorMode,
    no_color_flag: bool,
    no_color_env: bool,
    is_tty: bool,
) -> bool {
    if no_color_flag || no_color_env {
        return false;
    }
    match mode {
        ColorMode::Never => false,
        ColorMode::Always => true,
        ColorMode::Auto => is_tty,
    }
}

/// ANSI styling that collapses to plain text when disabled.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    on: bool,
}

/// SGR codes.
pub mod code {
    /// Bold.
    pub const BOLD: &str = "1";
    /// Dim.
    pub const DIM: &str = "2";
    /// Red.
    pub const RED: &str = "31";
    /// Green.
    pub const GREEN: &str = "32";
    /// Yellow.
    pub const YELLOW: &str = "33";
    /// Blue.
    pub const BLUE: &str = "34";
    /// Magenta.
    pub const MAGENTA: &str = "35";
    /// Cyan.
    pub const CYAN: &str = "36";
}

impl Style {
    /// New style; `on == false` never emits escapes.
    pub fn new(on: bool) -> Self {
        Self { on }
    }

    /// Plain (MCP text, pipes).
    pub fn plain() -> Self {
        Self { on: false }
    }

    /// Whether escapes are emitted.
    pub fn enabled(self) -> bool {
        self.on
    }

    /// Wrap `s` in an SGR sequence.
    pub fn paint(self, code: &str, s: &str) -> String {
        if self.on && !s.is_empty() {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Bold.
    pub fn bold(self, s: &str) -> String {
        self.paint(code::BOLD, s)
    }
    /// Dim.
    pub fn dim(self, s: &str) -> String {
        self.paint(code::DIM, s)
    }
    /// Red.
    pub fn red(self, s: &str) -> String {
        self.paint(code::RED, s)
    }
    /// Green.
    pub fn green(self, s: &str) -> String {
        self.paint(code::GREEN, s)
    }
    /// Yellow.
    pub fn yellow(self, s: &str) -> String {
        self.paint(code::YELLOW, s)
    }
    /// Blue.
    pub fn blue(self, s: &str) -> String {
        self.paint(code::BLUE, s)
    }
    /// Magenta.
    pub fn magenta(self, s: &str) -> String {
        self.paint(code::MAGENTA, s)
    }
    /// Cyan.
    pub fn cyan(self, s: &str) -> String {
        self.paint(code::CYAN, s)
    }
}

/// A table cell: text plus an optional SGR code applied after padding.
#[derive(Debug, Clone)]
pub struct Cell {
    text: String,
    code: Option<&'static str>,
}

impl Cell {
    /// Plain cell.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            code: None,
        }
    }
    /// Styled cell.
    pub fn styled(text: impl Into<String>, code: &'static str) -> Self {
        Self {
            text: text.into(),
            code: Some(code),
        }
    }
}

impl From<String> for Cell {
    fn from(s: String) -> Self {
        Self::plain(s)
    }
}

impl From<&str> for Cell {
    fn from(s: &str) -> Self {
        Self::plain(s)
    }
}

/// Left-aligned text table with an optional bold header row.
#[derive(Debug, Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<Cell>>,
}

impl Table {
    /// New table with headers (empty slice = no header row).
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|h| (*h).to_string()).collect(),
            rows: Vec::new(),
        }
    }

    /// Append a row.
    pub fn row(&mut self, cells: Vec<Cell>) {
        self.rows.push(cells);
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// No rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Render with two-space gutters and `indent` leading spaces; the last column is not padded.
    pub fn render(&self, style: Style, indent: usize) -> String {
        let cols = self
            .headers
            .len()
            .max(self.rows.iter().map(Vec::len).max().unwrap_or(0));
        let mut widths = vec![0usize; cols];
        for (i, h) in self.headers.iter().enumerate() {
            widths[i] = h.chars().count();
        }
        for r in &self.rows {
            for (i, c) in r.iter().enumerate() {
                widths[i] = widths[i].max(c.text.chars().count());
            }
        }
        let pad = " ".repeat(indent);
        let line = |cells: Vec<(String, Option<&'static str>)>| -> String {
            let n = cells.len();
            let mut s = pad.clone();
            for (i, (text, code)) in cells.into_iter().enumerate() {
                let fill = if i + 1 == n {
                    0
                } else {
                    widths[i].saturating_sub(text.chars().count()) + 2
                };
                match code {
                    Some(c) => s.push_str(&style.paint(c, &text)),
                    None => s.push_str(&text),
                }
                s.push_str(&" ".repeat(fill));
            }
            s.trim_end().to_string()
        };
        let mut out = String::new();
        if !self.headers.is_empty() {
            out.push_str(&line(
                self.headers
                    .iter()
                    .map(|h| (h.clone(), Some(code::BOLD)))
                    .collect(),
            ));
            out.push('\n');
        }
        for r in &self.rows {
            out.push_str(&line(r.iter().map(|c| (c.text.clone(), c.code)).collect()));
            out.push('\n');
        }
        out
    }
}

/// `{"ok":true,"data":...}` (field order of `data` preserved).
pub fn ok_envelope<T: Serialize>(data: &T) -> String {
    match serde_json::to_string(data) {
        Ok(d) => format!("{{\"ok\":true,\"data\":{d}}}"),
        Err(e) => err_envelope("internal", &format!("serialize output: {e}")),
    }
}

/// `{"ok":false,"error":{"code":...,"message":...}}`.
pub fn err_envelope(code: &str, message: &str) -> String {
    json!({"ok": false, "error": {"code": code, "message": message}}).to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn color_decision() {
        assert!(color_enabled(ColorMode::Auto, false, false, true));
        assert!(!color_enabled(ColorMode::Auto, false, false, false));
        assert!(!color_enabled(ColorMode::Auto, true, false, true));
        assert!(!color_enabled(ColorMode::Always, false, true, true));
        assert!(color_enabled(ColorMode::Always, false, false, false));
        assert!(!color_enabled(ColorMode::Never, false, false, true));
    }

    #[test]
    fn style_off_is_plain() {
        assert_eq!(Style::new(false).red("x"), "x");
        assert_eq!(Style::new(true).red("x"), "\x1b[31mx\x1b[0m");
    }

    #[test]
    fn table_alignment() {
        let mut t = Table::new(&["KIND", "NAME"]);
        t.row(vec!["fn".into(), "main".into()]);
        t.row(vec!["struct".into(), Cell::styled("Store", code::CYAN)]);
        let out = t.render(Style::new(false), 0);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines, ["KIND    NAME", "fn      main", "struct  Store"]);
        let bare = Table::new(&[]);
        assert!(bare.is_empty() && bare.render(Style::plain(), 2).is_empty());
    }

    #[test]
    fn envelopes() {
        let ok: Value = serde_json::from_str(&ok_envelope(&json!({"a": 1}))).unwrap();
        assert_eq!(ok, json!({"ok": true, "data": {"a": 1}}));
        let err: Value = serde_json::from_str(&err_envelope("not_found", "nope")).unwrap();
        assert_eq!(
            err,
            json!({"ok": false, "error": {"code": "not_found", "message": "nope"}})
        );
    }
}
