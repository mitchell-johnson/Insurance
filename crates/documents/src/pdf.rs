//! A document as a PDF: the letter a customer downloads, the attachment to
//! an email and the file the print house prints.
//!
//! The text is the document. It is what was rendered, hashed and stored, and
//! the PDF is that text set on A4 pages, made again whenever it is wanted
//! and never stored. Nothing here can change a word: a character the font
//! cannot draw is an error, not a gap.
//!
//! The pages are deterministic. The same letter and fonts give the same
//! bytes, with no date or random id in the file.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use subsetter::GlyphRemapper;
use ttf_parser::Face;

use crate::template::sha256_hex;

/// A4, in points.
pub const PAGE_WIDTH: f32 = 595.28;
pub const PAGE_HEIGHT: f32 = 841.89;
/// 25 mm at the sides and the foot, 20 mm at the head.
pub const MARGIN_SIDE: f32 = 70.87;
pub const MARGIN_TOP: f32 = 56.69;
pub const MARGIN_BOTTOM: f32 = 70.87;
/// The address begins 45 mm down the first page, where the window of a
/// DL envelope shows it.
pub const ADDRESS_TOP: f32 = 127.56;
/// The baseline of the line at the foot of every page.
pub const FOOTER_BASELINE: f32 = 36.0;
/// The most pages one document may run to.
pub const MAX_PAGES: usize = 200;

const LEADING: f32 = 1.4;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PdfError {
    #[error("the font is not a TrueType font this service can use: {0}")]
    Font(String),
    /// The text has a character the font has no glyph for. Setting the
    /// document without it would change what the document says.
    #[error("the font cannot draw `{0}` (U+{code:04X})", code = *.0 as u32)]
    NoGlyph(char),
    #[error("the document would run to more than {MAX_PAGES} pages")]
    TooLong,
}

/// The two faces a document is set in. The bundled faces are Liberation
/// Sans, which covers the Latin alphabets including the macron vowels of
/// te reo Māori and the languages of the Pacific.
#[derive(Clone)]
pub struct Fonts {
    regular: Arc<[u8]>,
    bold: Arc<[u8]>,
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fonts({} and {} bytes)", self.regular.len(), self.bold.len())
    }
}

impl Fonts {
    /// The faces built into the service.
    pub fn bundled() -> Self {
        Self {
            regular: Arc::from(&include_bytes!("../fonts/LiberationSans-Regular.ttf")[..]),
            bold: Arc::from(&include_bytes!("../fonts/LiberationSans-Bold.ttf")[..]),
        }
    }

    /// The insurer's own faces, as the bytes of two TrueType files.
    pub fn new(regular: Vec<u8>, bold: Vec<u8>) -> Result<Self, PdfError> {
        for data in [&regular, &bold] {
            let face = Face::parse(data, 0).map_err(|e| PdfError::Font(e.to_string()))?;
            if face.tables().glyf.is_none() {
                return Err(PdfError::Font("it has no TrueType outlines".into()));
            }
        }
        Ok(Self {
            regular: regular.into(),
            bold: bold.into(),
        })
    }

    fn data(&self, weight: Weight) -> &[u8] {
        match weight {
            Weight::Regular => &self.regular,
            Weight::Bold => &self.bold,
        }
    }

    fn face(&self, weight: Weight) -> Result<Face<'_>, PdfError> {
        Face::parse(self.data(weight), 0).map_err(|e| PdfError::Font(e.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Weight {
    Regular,
    Bold,
}

/// What is set on the pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Letter<'a> {
    /// The document's id, printed at the foot of every page so that a
    /// loose page can be traced.
    pub document: &'a str,
    pub title: &'a str,
    /// The person the copy is addressed to. Empty for a copy that is not posted.
    pub name: &'a str,
    /// The lines of the postal address. Empty for a copy that is not posted.
    pub address: &'a [String],
    /// The document exactly as issued.
    pub text: &'a str,
    /// The language of the text, as a BCP 47 tag, for screen readers.
    pub language: &'a str,
    /// Set in 18 point, for a reader who asked for large print.
    pub large_print: bool,
}

/// One line of text at a place on a page. `x` and `y` are the start of
/// the baseline, in points from the bottom left corner.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub weight: Weight,
    pub size: f32,
    pub x: f32,
    pub y: f32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    pub lines: Vec<Line>,
}

struct Sizes {
    body: f32,
    title: f32,
    footer: f32,
}

impl Sizes {
    fn of(large_print: bool) -> Self {
        if large_print {
            Self {
                body: 18.0,
                title: 22.0,
                footer: 14.0,
            }
        } else {
            Self {
                body: 11.0,
                title: 14.0,
                footer: 9.0,
            }
        }
    }
}

/// Text as it is set: tabs are spaces, and characters that draw nothing
/// (carriage returns, other controls) are dropped.
fn clean(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\t' | '\u{a0}' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

fn advance(face: &Face<'_>, c: char) -> Result<f32, PdfError> {
    let glyph = face.glyph_index(c).ok_or(PdfError::NoGlyph(c))?;
    Ok(f32::from(face.glyph_hor_advance(glyph).unwrap_or(0)) / f32::from(face.units_per_em()))
}

/// The width of text at a size, in points.
fn width(face: &Face<'_>, text: &str, size: f32) -> Result<f32, PdfError> {
    let mut em = 0.0;
    for c in text.chars() {
        em += advance(face, c)?;
    }
    Ok(em * size)
}

/// Break one paragraph into lines no wider than `max`. A word wider than a
/// whole line is broken where it must be.
fn wrap(face: &Face<'_>, paragraph: &str, size: f32, max: f32) -> Result<Vec<String>, PdfError> {
    let mut lines = vec![];
    let mut line = String::new();
    for word in paragraph.split(' ').filter(|w| !w.is_empty()) {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if width(face, &candidate, size)? <= max {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        // The word starts a line. If it is too wide alone, it is cut.
        for c in word.chars() {
            line.push(c);
            if width(face, &line, size)? > max && line.chars().count() > 1 {
                line.pop();
                lines.push(std::mem::replace(&mut line, c.to_string()));
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    Ok(lines)
}

/// Where every line of the letter goes. A pure function of the letter and
/// the fonts' measurements.
pub fn layout(letter: &Letter<'_>, fonts: &Fonts) -> Result<Vec<Page>, PdfError> {
    let regular = fonts.face(Weight::Regular)?;
    let bold = fonts.face(Weight::Bold)?;
    let sizes = Sizes::of(letter.large_print);
    let measure = PAGE_WIDTH - 2.0 * MARGIN_SIDE;
    let floor = MARGIN_BOTTOM;
    let mut pages = vec![Page::default()];
    let mut y = PAGE_HEIGHT - MARGIN_TOP;

    // One line set below the last: move down, turn the page if it is full.
    let set = |pages: &mut Vec<Page>, y: &mut f32, weight: Weight, size: f32, text: String| -> Result<(), PdfError> {
        *y -= size * LEADING;
        if *y < floor {
            if pages.len() == MAX_PAGES {
                return Err(PdfError::TooLong);
            }
            pages.push(Page::default());
            *y = PAGE_HEIGHT - MARGIN_TOP - size * LEADING;
        }
        if let Some(page) = pages.last_mut() {
            page.lines.push(Line {
                weight,
                size,
                x: MARGIN_SIDE,
                y: *y,
                text,
            });
        }
        Ok(())
    };

    let name = clean(letter.name);
    let address: Vec<String> = letter.address.iter().map(|l| clean(l)).filter(|l| !l.trim().is_empty()).collect();
    if !name.trim().is_empty() || !address.is_empty() {
        y = PAGE_HEIGHT - ADDRESS_TOP;
        for line in std::iter::once(name).filter(|n| !n.trim().is_empty()).chain(address) {
            // An address line is never wrapped into two by the page, but
            // one too long for it is.
            for part in wrap(&regular, &line, sizes.body, measure)? {
                set(&mut pages, &mut y, Weight::Regular, sizes.body, part)?;
            }
        }
        y -= sizes.body * LEADING * 2.0;
    }

    let title = clean(letter.title);
    if !title.trim().is_empty() {
        for part in wrap(&bold, &title, sizes.title, measure)? {
            set(&mut pages, &mut y, Weight::Bold, sizes.title, part)?;
        }
        y -= sizes.body * LEADING * 0.5;
    }

    for paragraph in letter.text.lines() {
        let paragraph = clean(paragraph);
        if paragraph.trim().is_empty() {
            // A blank line in the text is a gap on the page.
            y -= sizes.body * LEADING;
            continue;
        }
        for part in wrap(&regular, &paragraph, sizes.body, measure)? {
            set(&mut pages, &mut y, Weight::Regular, sizes.body, part)?;
        }
    }

    let count = pages.len();
    let document = clean(letter.document);
    for (n, page) in pages.iter_mut().enumerate() {
        let footer = format!("{document}    {} / {count}", n + 1);
        // Measured, so that an id the font cannot draw is refused.
        width(&regular, &footer, sizes.footer)?;
        page.lines.push(Line {
            weight: Weight::Regular,
            size: sizes.footer,
            x: MARGIN_SIDE,
            y: FOOTER_BASELINE,
            text: footer,
        });
    }
    Ok(pages)
}

/// The glyphs of one face a document uses, renumbered from 1 for the
/// subset embedded in the file.
struct Used {
    remapper: GlyphRemapper,
    /// New glyph id to the character it draws, for text extraction.
    chars: BTreeMap<u16, char>,
}

impl Used {
    fn new() -> Self {
        Self {
            remapper: GlyphRemapper::new(),
            chars: BTreeMap::new(),
        }
    }

    fn encode(&mut self, face: &Face<'_>, text: &str) -> Result<Vec<u8>, PdfError> {
        let mut bytes = Vec::with_capacity(text.len() * 2);
        for c in text.chars() {
            let glyph = face.glyph_index(c).ok_or(PdfError::NoGlyph(c))?;
            let new = self.remapper.remap(glyph.0);
            self.chars.entry(new).or_insert(c);
            bytes.extend(new.to_be_bytes());
        }
        Ok(bytes)
    }
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    // Writing to a vector cannot fail.
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

fn resource(weight: Weight) -> Name<'static> {
    match weight {
        Weight::Regular => Name(b"F1"),
        Weight::Bold => Name(b"F2"),
    }
}

/// Embed the subset of one face: the font program, its descriptor, the
/// widths, and the map back to text.
fn embed(pdf: &mut Pdf, next: &mut Ref, font_id: Ref, fonts: &Fonts, weight: Weight, used: &Used) -> Result<(), PdfError> {
    let face = fonts.face(weight)?;
    let data = fonts.data(weight);
    let subset = subsetter::subset(data, 0, &used.remapper).map_err(|e| PdfError::Font(e.to_string()))?;
    let scale = 1000.0 / f32::from(face.units_per_em());

    // A subset is named with six letters that stand for its contents.
    let digest = sha256_hex(&subset);
    let tag: String = digest.bytes().take(6).map(|b| char::from(b'A' + (b % 26))).collect();
    let family = match weight {
        Weight::Regular => "DocumentSans",
        Weight::Bold => "DocumentSans-Bold",
    };
    let base = format!("{tag}+{family}");
    let base = Name(base.as_bytes());

    let (cid_id, descriptor_id, file_id, cmap_id) = (next.bump(), next.bump(), next.bump(), next.bump());
    let system = SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    };

    pdf.type0_font(font_id)
        .base_font(base)
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(cid_id)
        .to_unicode(cmap_id);

    let mut cid = pdf.cid_font(cid_id);
    cid.subtype(CidFontType::Type2)
        .base_font(base)
        .system_info(system)
        .font_descriptor(descriptor_id)
        .default_width(0.0)
        .cid_to_gid_map_predefined(Name(b"Identity"));
    // The subset's glyphs are numbered in the order they were first used.
    let widths = used
        .remapper
        .remapped_gids()
        .map(|old| f32::from(face.glyph_hor_advance(ttf_parser::GlyphId(old)).unwrap_or(0)) * scale);
    cid.widths().consecutive(0, widths);
    cid.finish();

    let bbox = face.global_bounding_box();
    pdf.font_descriptor(descriptor_id)
        .name(base)
        .flags(FontFlags::NON_SYMBOLIC)
        .bbox(Rect::new(
            f32::from(bbox.x_min) * scale,
            f32::from(bbox.y_min) * scale,
            f32::from(bbox.x_max) * scale,
            f32::from(bbox.y_max) * scale,
        ))
        .italic_angle(0.0)
        .ascent(f32::from(face.ascender()) * scale)
        .descent(f32::from(face.descender()) * scale)
        .cap_height(f32::from(face.capital_height().unwrap_or(face.ascender())) * scale)
        .stem_v(if weight == Weight::Bold { 140.0 } else { 80.0 })
        .font_file2(file_id);

    let packed = deflate(&subset);
    let mut file = pdf.stream(file_id, &packed);
    file.filter(Filter::FlateDecode);
    file.pair(Name(b"Length1"), subset.len() as i32);
    file.finish();

    let mut cmap = UnicodeCmap::new(Name(b"Custom"), system);
    for (glyph, c) in &used.chars {
        cmap.pair(*glyph, *c);
    }
    let cmap = cmap.finish();
    pdf.cmap(cmap_id, cmap.as_slice());
    Ok(())
}

/// The letter as the bytes of a PDF file.
pub fn pdf(letter: &Letter<'_>, fonts: &Fonts) -> Result<Vec<u8>, PdfError> {
    let pages = layout(letter, fonts)?;
    let faces = [(Weight::Regular, fonts.face(Weight::Regular)?), (Weight::Bold, fonts.face(Weight::Bold)?)];
    let mut used = [Used::new(), Used::new()];
    // Glyph 0, the font's "missing" box, is in every subset and is never shown.
    for u in &mut used {
        u.remapper.remap(0);
    }

    let mut next = Ref::new(1);
    let (catalog_id, tree_id, info_id) = (next.bump(), next.bump(), next.bump());
    let font_ids = [next.bump(), next.bump()];
    let mut out = Pdf::new();

    let mut page_ids = vec![];
    let mut shown = [false, false];
    for page in &pages {
        let (page_id, content_id) = (next.bump(), next.bump());
        page_ids.push(page_id);
        let mut content = Content::new();
        for line in &page.lines {
            let i = line.weight as usize;
            let bytes = used[i].encode(&faces[i].1, &line.text)?;
            shown[i] = true;
            content
                .begin_text()
                .set_font(resource(line.weight), line.size)
                .next_line(line.x, line.y)
                .show(Str(&bytes))
                .end_text();
        }
        let packed = deflate(content.finish().as_slice());
        out.stream(content_id, &packed).filter(Filter::FlateDecode);

        let mut object = out.page(page_id);
        object
            .media_box(Rect::new(0.0, 0.0, PAGE_WIDTH, PAGE_HEIGHT))
            .parent(tree_id)
            .contents(content_id);
        let mut resources = object.resources();
        let mut dict = resources.fonts();
        for (i, (weight, _)) in faces.iter().enumerate() {
            if page.lines.iter().any(|l| l.weight == *weight) {
                dict.pair(resource(*weight), font_ids[i]);
            }
        }
    }

    for (i, (weight, _)) in faces.iter().enumerate() {
        if shown[i] {
            embed(&mut out, &mut next, font_ids[i], fonts, *weight, &used[i])?;
        }
    }

    out.pages(tree_id).kids(page_ids.iter().copied()).count(page_ids.len() as i32);
    let mut catalog = out.catalog(catalog_id);
    catalog.pages(tree_id);
    if !letter.language.is_empty() {
        catalog.lang(TextStr(letter.language));
    }
    catalog.viewer_preferences().display_doc_title(true);
    catalog.finish();
    out.document_info(info_id).title(TextStr(&clean(letter.title)));
    Ok(out.finish())
}
