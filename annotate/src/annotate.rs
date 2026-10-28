//! Writes highlight annotations into a PDF with lopdf.

use anyhow::{anyhow, Context, Result};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};

use crate::extract::Glyph;

/// An axis-aligned rectangle in PDF user space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    fn union(&mut self, other: &Rect) {
        self.x0 = self.x0.min(other.x0);
        self.y0 = self.y0.min(other.y0);
        self.x1 = self.x1.max(other.x1);
        self.y1 = self.y1.max(other.y1);
    }

    fn from_glyph(g: &Glyph) -> Self {
        Self {
            x0: g.x0,
            y0: g.y0,
            x1: g.x1,
            y1: g.y1,
        }
    }
}

/// One highlight: a set of line rectangles on one page.
#[derive(Debug, Clone)]
pub struct Highlight {
    pub page: u32,
    pub rects: Vec<Rect>,
    pub color: (f32, f32, f32),
    /// Shown as the annotation's title (author field).
    pub title: String,
    /// Shown as the annotation's popup text.
    pub contents: String,
}

/// A sticky note (`/Text` annotation) anchored at a point on a page.
#[derive(Debug, Clone)]
pub struct Note {
    pub page: u32,
    /// Where to place the icon, in PDF user space. `None` puts it in the
    /// page's top-left corner.
    pub anchor: Option<(f64, f64)>,
    pub color: (f32, f32, f32),
    /// Icon name: `Note`, `Comment`, `Help`, `Insert`, `Key`, `Paragraph`...
    pub icon: &'static str,
    pub title: String,
    pub contents: String,
    /// Unique annotation name (`/NM`).
    pub name: String,
}

/// Groups a run of glyphs into per-page, per-line rectangles.
pub fn group_lines(glyphs: &[Glyph]) -> Vec<(u32, Vec<Rect>)> {
    let mut pages: Vec<(u32, Vec<Rect>)> = Vec::new();
    let mut current: Option<(u32, Rect, f64)> = None; // page, rect, baseline y0

    for g in glyphs {
        let rect = Rect::from_glyph(g);
        match current.as_mut() {
            Some((page, cur, _)) if *page == g.page && same_line(cur, g) => {
                cur.union(&rect);
            }
            _ => {
                if let Some((page, cur, _)) = current.take() {
                    push_rect(&mut pages, page, cur);
                }
                current = Some((g.page, rect, g.y0));
            }
        }
    }
    if let Some((page, cur, _)) = current.take() {
        push_rect(&mut pages, page, cur);
    }
    pages
}

fn same_line(cur: &Rect, g: &Glyph) -> bool {
    let tolerance = g.font_size * 0.5;
    let vertical_overlap = (g.y0 - cur.y0).abs() <= tolerance;
    // A glyph far to the left of the current line's right edge started a new
    // visual line even if its baseline is close (e.g. multi-column layouts).
    let wrapped_back = g.x0 < cur.x1 - g.font_size * 2.0 && g.x0 < cur.x0;
    vertical_overlap && !wrapped_back
}

fn push_rect(pages: &mut Vec<(u32, Vec<Rect>)>, page: u32, rect: Rect) {
    match pages.last_mut() {
        Some((p, rects)) if *p == page => rects.push(rect),
        _ => pages.push((page, vec![rect])),
    }
}

/// An annotation to write, in the order it should appear in the page's
/// `/Annots` array (which is the order viewers list annotations in).
#[derive(Debug, Clone)]
pub enum Annotation {
    Highlight(Highlight),
    Note(Note),
}

/// Adds every annotation to `doc`. Returns the number written.
pub fn apply(doc: &mut Document, annotations: &[Annotation]) -> Result<usize> {
    let pages = doc.get_pages();
    let mut added = 0usize;

    for (n, annotation) in annotations.iter().enumerate() {
        let (page_num, annot_id) = match annotation {
            Annotation::Highlight(h) => {
                let page_id = *pages
                    .get(&h.page)
                    .ok_or_else(|| anyhow!("page {} not found in document", h.page))?;
                (h.page, add_highlight(doc, page_id, h, n)?)
            }
            Annotation::Note(note) => {
                let page_id = *pages
                    .get(&note.page)
                    .ok_or_else(|| anyhow!("page {} not found in document", note.page))?;
                (note.page, add_note(doc, page_id, note)?)
            }
        };
        append_annot(doc, pages[&page_num], annot_id)?;
        added += 1;
    }
    Ok(added)
}

fn add_highlight(
    doc: &mut Document,
    page_id: ObjectId,
    h: &Highlight,
    n: usize,
) -> Result<ObjectId> {
    let mut bbox = h.rects[0];
    for r in &h.rects[1..] {
        bbox.union(r);
    }

    let mut quad_points = Vec::with_capacity(h.rects.len() * 8);
    for r in &h.rects {
        // Acrobat's order: upper-left, upper-right, lower-left, lower-right.
        quad_points.extend([
            real(r.x0),
            real(r.y1),
            real(r.x1),
            real(r.y1),
            real(r.x0),
            real(r.y0),
            real(r.x1),
            real(r.y0),
        ]);
    }

    let (red, green, blue) = h.color;
    let appearance = appearance_stream(doc, &bbox, &h.rects, h.color);

    let annot = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Highlight",
        "Rect" => Object::Array(vec![real(bbox.x0), real(bbox.y0), real(bbox.x1), real(bbox.y1)]),
        "QuadPoints" => Object::Array(quad_points),
        "C" => Object::Array(vec![Object::Real(red), Object::Real(green), Object::Real(blue)]),
        "CA" => Object::Real(1.0),
        "F" => Object::Integer(4),
        "T" => Object::string_literal(h.title.as_str()),
        "Contents" => Object::string_literal(pdf_text(&h.contents)),
        "NM" => Object::string_literal(format!("pangram-{n}")),
        "P" => Object::Reference(page_id),
        "AP" => dictionary! { "N" => Object::Reference(appearance) },
    };
    Ok(doc.add_object(annot))
}

fn appearance_stream(
    doc: &mut Document,
    bbox: &Rect,
    rects: &[Rect],
    color: (f32, f32, f32),
) -> ObjectId {
    let gs = doc.add_object(dictionary! {
        "Type" => "ExtGState",
        "BM" => "Multiply",
        "CA" => Object::Real(1.0),
        "ca" => Object::Real(1.0),
    });
    let mut content = format!("/GS gs {} {} {} rg\n", color.0, color.1, color.2);
    for r in rects {
        content.push_str(&format!(
            "{:.3} {:.3} {:.3} {:.3} re\n",
            r.x0,
            r.y0,
            r.x1 - r.x0,
            r.y1 - r.y0
        ));
    }
    content.push_str("f\n");

    let dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "FormType" => Object::Integer(1),
        "BBox" => Object::Array(vec![real(bbox.x0), real(bbox.y0), real(bbox.x1), real(bbox.y1)]),
        "Group" => dictionary! { "S" => "Transparency", "CS" => "DeviceRGB" },
        "Resources" => dictionary! {
            "ExtGState" => dictionary! { "GS" => Object::Reference(gs) },
        },
    };
    doc.add_object(Stream::new(dict, content.into_bytes()))
}

/// Icon size used for note annotations, in points. Viewers draw their own
/// fixed-size icon but use the rect's top-left corner for placement.
const NOTE_SIZE: f64 = 18.0;

fn add_note(doc: &mut Document, page_id: ObjectId, note: &Note) -> Result<ObjectId> {
    let media = page_media_box(doc, page_id).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let (x, y) = match note.anchor {
        Some((x, y)) => (
            x.clamp(media[0] + 2.0, media[2] - NOTE_SIZE - 2.0),
            y.clamp(media[1] + 2.0, media[3] - NOTE_SIZE - 2.0),
        ),
        None => (media[0] + 10.0, media[3] - 30.0),
    };
    let (red, green, blue) = note.color;
    let annot = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Text",
        "Name" => Object::Name(note.icon.as_bytes().to_vec()),
        "Rect" => Object::Array(vec![real(x), real(y), real(x + NOTE_SIZE), real(y + NOTE_SIZE)]),
        "C" => Object::Array(vec![Object::Real(red), Object::Real(green), Object::Real(blue)]),
        "F" => Object::Integer(4),
        "T" => Object::string_literal(pdf_text(&note.title)),
        "Contents" => Object::string_literal(pdf_text(&note.contents)),
        "NM" => Object::string_literal(note.name.as_str()),
        "Open" => Object::Boolean(false),
        "P" => Object::Reference(page_id),
    };
    Ok(doc.add_object(annot))
}

fn page_media_box(doc: &Document, page_id: ObjectId) -> Option<[f64; 4]> {
    let page = doc.get_dictionary(page_id).ok()?;
    let mb = doc
        .get_object(
            page.get(b"MediaBox")
                .ok()?
                .as_reference()
                .unwrap_or(page_id),
        )
        .ok();
    let arr = match page.get(b"MediaBox").ok()? {
        Object::Array(a) => a.clone(),
        Object::Reference(_) => mb?.as_array().ok()?.clone(),
        _ => return None,
    };
    if arr.len() != 4 {
        return None;
    }
    let mut out = [0.0; 4];
    for (i, o) in arr.iter().enumerate() {
        out[i] = o.as_float().ok()? as f64;
    }
    Some(out)
}

/// Appends `annot_id` to the page's `/Annots`, creating the array if needed
/// and following an indirect reference to an existing array.
fn append_annot(doc: &mut Document, page_id: ObjectId, annot_id: ObjectId) -> Result<()> {
    let existing = {
        let page = doc
            .get_dictionary(page_id)
            .with_context(|| format!("page object {page_id:?} is not a dictionary"))?;
        page.get(b"Annots").ok().cloned()
    };

    match existing {
        Some(Object::Reference(array_id)) => {
            let obj = doc.get_object_mut(array_id)?;
            let arr = obj
                .as_array_mut()
                .map_err(|e| anyhow!("page /Annots reference is not an array: {e}"))?;
            arr.push(Object::Reference(annot_id));
        }
        Some(Object::Array(mut arr)) => {
            arr.push(Object::Reference(annot_id));
            set_annots(doc, page_id, Object::Array(arr))?;
        }
        _ => {
            set_annots(
                doc,
                page_id,
                Object::Array(vec![Object::Reference(annot_id)]),
            )?;
        }
    }
    Ok(())
}

fn set_annots(doc: &mut Document, page_id: ObjectId, annots: Object) -> Result<()> {
    let page: &mut Dictionary = doc
        .get_object_mut(page_id)?
        .as_dict_mut()
        .map_err(|e| anyhow!("page object is not a dictionary: {e}"))?;
    page.set("Annots", annots);
    Ok(())
}

fn real(v: f64) -> Object {
    Object::Real(v as f32)
}

/// Encodes text for a PDF text string: PDFDocEncoding when ASCII, otherwise
/// UTF-16BE with a byte order mark.
fn pdf_text(s: &str) -> Vec<u8> {
    if s.is_ascii() {
        return s.as_bytes().to_vec();
    }
    let mut out = vec![0xFE, 0xFF];
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}
