//! Image output retained for browser rendering, with text-only terminal fallbacks.

use super::*;

#[derive(Debug)]
pub struct ImageOutputCell {
    pub images: Vec<String>,
    pub fallback: PlainHistoryCell,
}

impl HistoryCell for ImageOutputCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.fallback.display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.fallback.raw_lines()
    }
}
