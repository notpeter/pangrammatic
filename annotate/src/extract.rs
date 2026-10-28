//! Text extraction that remembers where every character was drawn.
//!
//! `pdf-extract` walks the content streams and calls [`OutputDev`] for each
//! glyph with its text rendering matrix. We collect the glyph boxes in PDF
//! user space and build the plain text with the same whitespace heuristics as
//! `pdf_extract::PlainTextOutput`, so that character index `i` of the text
//! corresponds to `glyphs[i]`.

use pdf_extract::{MediaBox, OutputDev, OutputError, Transform};

/// A glyph's bounding box on a page, in PDF user space (origin bottom-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    /// 1-based page number as returned by `lopdf::Document::get_pages`.
    pub page: u32,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    /// Effective font size in user space, used to group glyphs into lines.
    pub font_size: f64,
}

/// The extracted text and, for every char of it, the glyph that produced it.
///
/// Whitespace inserted by the layout heuristics has no glyph.
#[derive(Debug, Default)]
pub struct Extracted {
    pub chars: Vec<char>,
    pub glyphs: Vec<Option<Glyph>>,
    pub page_count: u32,
}

impl Extracted {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_blank(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }
}

/// One visual line of text on a page, in content-stream order.
#[derive(Debug, Default)]
struct Line {
    /// Baseline of the first glyph.
    y: f64,
    /// Largest effective font size on the line.
    font_size: f64,
    x0: f64,
    x1: f64,
    chars: Vec<char>,
    glyphs: Vec<Option<Glyph>>,
}

impl Line {
    fn push(&mut self, c: char, glyph: Option<Glyph>) {
        self.chars.push(c);
        self.glyphs.push(glyph);
    }

    fn same_baseline(&self, other: &Line) -> bool {
        (self.y - other.y).abs() <= 0.5 * self.font_size.min(other.font_size)
    }

    /// True if this line is a short run (a bullet or list number) that sits
    /// entirely to the left of `target` on the same baseline.
    fn is_marker_for(&self, target: &Line) -> bool {
        const MAX_MARKER_CHARS: usize = 6;
        self.same_baseline(target)
            && self.x1 <= target.x0 + 0.5 * target.font_size
            && self.chars.iter().filter(|c| !c.is_whitespace()).count() <= MAX_MARKER_CHARS
    }
}

#[derive(Default)]
struct Collector {
    out: Extracted,
    page: u32,
    /// Lines of the current page, in the order the content stream drew them.
    lines: Vec<Line>,
    // Layout state mirroring pdf_extract::PlainTextOutput.
    last_end: f64,
    last_y: f64,
    first_char: bool,
}

impl Collector {
    fn new() -> Self {
        Self {
            last_end: 100000.0,
            ..Default::default()
        }
    }

    fn push(&mut self, c: char, glyph: Option<Glyph>) {
        self.out.chars.push(c);
        self.out.glyphs.push(glyph);
    }

    /// Reattaches list markers that the content stream drew after the
    /// items they belong to (common in PDFs produced by browsers and word
    /// processors): a short run on the same baseline as, and left of, an
    /// earlier line is prepended to that line.
    fn merge_markers(lines: &mut Vec<Line>) {
        let mut i = 0;
        while i < lines.len() {
            let target = (0..i).find(|&j| lines[i].is_marker_for(&lines[j]));
            match target {
                Some(j) => {
                    let marker = lines.remove(i);
                    let target = &mut lines[j];
                    let trimmed = marker
                        .chars
                        .iter()
                        .rposition(|c| !c.is_whitespace())
                        .map_or(0, |p| p + 1);
                    let mut chars = marker.chars[..trimmed].to_vec();
                    let mut glyphs = marker.glyphs[..trimmed].to_vec();
                    chars.push(' ');
                    glyphs.push(None);
                    chars.append(&mut target.chars);
                    glyphs.append(&mut target.glyphs);
                    target.chars = chars;
                    target.glyphs = glyphs;
                    target.x0 = target.x0.min(marker.x0);
                }
                None => i += 1,
            }
        }
    }

    /// Writes the current page's lines to the output.
    fn flush_page(&mut self) {
        let mut lines = std::mem::take(&mut self.lines);
        Self::merge_markers(&mut lines);
        let mut prev: Option<(f64, f64)> = None;
        for line in lines {
            if let Some((py, _)) = prev {
                self.push('\n', None);
                // A gap well beyond normal leading separates paragraphs.
                if (py - line.y).abs() > line.font_size * 2.2 {
                    self.push('\n', None);
                }
            }
            for (c, g) in line.chars.iter().zip(line.glyphs.iter()) {
                self.push(*c, *g);
            }
            prev = Some((line.y, line.font_size));
        }
    }
}

impl OutputDev for Collector {
    fn begin_page(
        &mut self,
        page_num: u32,
        _media_box: &MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.page = page_num;
        self.out.page_count = self.out.page_count.max(page_num);
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        self.flush_page();
        // Separate pages with a blank line so paragraphs never merge across
        // a page break.
        if !self.out.chars.is_empty() && !self.out.chars.ends_with(&['\n', '\n']) {
            if !self.out.chars.ends_with(&['\n']) {
                self.push('\n', None);
            }
            self.push('\n', None);
        }
        self.last_end = 100000.0;
        self.first_char = false;
        Ok(())
    }

    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        _spacing: f64,
        font_size: f64,
        text: &str,
    ) -> Result<(), OutputError> {
        let (x, y) = (trm.m31, trm.m32);
        // Length of one side of the square with the same area as the
        // transformed (font_size, font_size) vector, as PlainTextOutput does.
        let vx = font_size * trm.m11 + font_size * trm.m21;
        let vy = font_size * trm.m12 + font_size * trm.m22;
        let fs = (vx * vy).abs().sqrt().max(f64::EPSILON);

        let mut new_line = self.lines.is_empty();
        let mut space = false;
        if self.first_char && !new_line {
            let dy = (y - self.last_y).abs();
            // A vertical move of at least half a line height is a new line,
            // whether we wrapped back to the left margin or jumped further.
            new_line = dy > fs * 0.5 && (x < self.last_end || dy > fs * 1.5);
            space = !new_line && x > self.last_end + fs * 0.1;
        }
        if new_line {
            self.lines.push(Line {
                y,
                font_size: fs,
                x0: x,
                x1: x,
                ..Default::default()
            });
        }
        let line = self.lines.last_mut().expect("a current line");
        if space {
            line.push(' ', None);
        }

        let advance = width * fs;
        let glyph = Glyph {
            page: self.page,
            x0: x.min(x + advance),
            x1: x.max(x + advance),
            // Approximate ascent/descent from the font size.
            y0: y - 0.22 * fs,
            y1: y + 0.78 * fs,
            font_size: fs,
        };
        for c in text.chars() {
            // Fonts occasionally decode to control characters; keep the text
            // clean so offsets stay stable through Pangram's normalization.
            if c.is_control() && c != '\n' && c != '\t' {
                continue;
            }
            line.push(c, Some(glyph));
        }
        line.x0 = line.x0.min(glyph.x0);
        line.x1 = line.x1.max(glyph.x1);
        line.font_size = line.font_size.max(fs);

        self.first_char = false;
        self.last_y = y;
        self.last_end = x + advance;
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), OutputError> {
        self.first_char = true;
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

/// Extracts text and glyph positions from every page of `doc`.
pub fn extract(doc: &lopdf::Document) -> Result<Extracted, OutputError> {
    let mut collector = Collector::new();
    pdf_extract::output_doc(doc, &mut collector)?;
    Ok(collector.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(y: f64, x0: f64, text: &str) -> Line {
        let x1 = x0 + 5.0 * text.chars().count() as f64;
        Line {
            y,
            font_size: 10.0,
            x0,
            x1,
            chars: text.chars().collect(),
            glyphs: vec![None; text.chars().count()],
        }
    }

    fn text(l: &Line) -> String {
        l.chars.iter().collect()
    }

    #[test]
    fn markers_drawn_later_are_prepended() {
        let mut lines = vec![
            line(700.0, 100.0, "first item"),
            line(686.0, 100.0, "second item"),
            line(660.0, 72.0, "A paragraph after the list."),
            line(700.0, 80.0, "1. "),
            line(686.0, 80.0, "2."),
        ];
        Collector::merge_markers(&mut lines);
        let texts: Vec<_> = lines.iter().map(text).collect();
        assert_eq!(
            texts,
            [
                "1. first item",
                "2. second item",
                "A paragraph after the list."
            ]
        );
        assert_eq!(lines[0].x0, 80.0);
        assert_eq!(lines[0].chars.len(), lines[0].glyphs.len());
    }

    #[test]
    fn runs_to_the_right_or_long_runs_are_kept() {
        let mut lines = vec![
            line(700.0, 100.0, "body text"),
            line(700.0, 300.0, "7"),             // page number, to the right
            line(700.0, 20.0, "a long sidebar"), // too long to be a marker
            line(650.0, 80.0, "-"),              // different baseline
        ];
        Collector::merge_markers(&mut lines);
        assert_eq!(lines.len(), 4);
    }
}
