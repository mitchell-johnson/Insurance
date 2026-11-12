//! A document set as a PDF: where the lines go, and what is in the file.

use std::io::Read;

use ic_documents::pdf::{
    layout, pdf, Fonts, Letter, PdfError, Weight, ADDRESS_TOP, FOOTER_BASELINE, MARGIN_BOTTOM, MARGIN_SIDE, MARGIN_TOP, MAX_PAGES, PAGE_HEIGHT, PAGE_WIDTH,
};

const TEXT: &str =
    "Tēnā koe Aroha\n\nYour premium of $120.50 was due on 1 November 2027. If it is not paid by 15 December 2027 your policy will end.\n\nNgā mihi";

fn address() -> Vec<String> {
    vec!["12 Kōwhai Street".into(), "Ngaio".into(), "Wellington 6035".into()]
}

fn letter<'a>(text: &'a str, address: &'a [String]) -> Letter<'a> {
    Letter {
        document: "doc-1",
        title: "Your policy is in arrears",
        name: "Aroha Ngata",
        address,
        text,
        language: "en-NZ",
        large_print: false,
    }
}

/// Every deflated stream in the file, inflated.
fn streams(file: &[u8]) -> Vec<Vec<u8>> {
    let mut found = vec![];
    let mut at = 0;
    while let Some(start) = file[at..].windows(7).position(|w| w == b"stream\n").map(|p| at + p + 7) {
        let end = start + file[start..].windows(10).position(|w| w == b"\nendstream").expect("a stream ends");
        let mut out = vec![];
        if flate2::read::ZlibDecoder::new(&file[start..end]).read_to_end(&mut out).is_ok() {
            found.push(out);
        } else {
            found.push(file[start..end].to_vec());
        }
        at = end + 10;
    }
    found
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack.windows(needle.len()).filter(|w| *w == needle).count()
}

/// Purpose: the first page carries the name and address where an envelope
/// window shows them, then the title in bold, the text, and a foot line
/// with the document id and page number, all inside the margins.
/// Why it matters: a posted notice whose address misses the window is
/// returned undelivered, and a notice that cannot be proven delivered
/// holds up the lapse it warns of.
#[test]
fn a_letter_is_laid_out_for_the_envelope() {
    let fonts = Fonts::bundled();
    let address = address();
    let pages = layout(&letter(TEXT, &address), &fonts).unwrap();
    assert_eq!(pages.len(), 1);
    let lines = &pages[0].lines;
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        &texts[..5],
        ["Aroha Ngata", "12 Kōwhai Street", "Ngaio", "Wellington 6035", "Your policy is in arrears"]
    );
    assert_eq!(texts[5], "Tēnā koe Aroha");
    assert_eq!(*texts.last().unwrap(), "doc-1    1 / 1");

    // The name sits just below the 45 mm line, at body size.
    assert!((lines[0].y - (PAGE_HEIGHT - ADDRESS_TOP - 11.0 * 1.4)).abs() < 0.01);
    assert_eq!((lines[4].weight, lines[4].size), (Weight::Bold, 14.0));
    assert!(lines[..4].iter().all(|l| l.weight == Weight::Regular && l.size == 11.0));
    // The blank line in the text is a gap: twice the usual step.
    let step = lines[5].y - lines[6].y;
    assert!((step - 2.0 * 11.0 * 1.4).abs() < 0.01, "{step}");
    let last = lines.last().unwrap();
    assert_eq!((last.y, last.size), (FOOTER_BASELINE, 9.0));
    for line in &lines[..lines.len() - 1] {
        assert!(line.x == MARGIN_SIDE && line.y >= MARGIN_BOTTOM && line.y <= PAGE_HEIGHT - MARGIN_TOP);
    }
    // The long paragraph was wrapped, and no word was lost in the wrapping.
    let body: Vec<&str> = texts[6..texts.len() - 2].to_vec();
    assert!(body.len() > 1);
    assert_eq!(body.join(" "), TEXT.lines().nth(2).unwrap());
}

/// Purpose: a copy with no name or address starts at the top margin with
/// the title, and one with no title starts with the text.
/// Why it matters: a customer downloading a document from the app is not
/// being posted anything; a blank address block would push the text down
/// the page for no reason, and an empty line set in bold would embed a
/// font nobody reads.
#[test]
fn a_copy_that_is_not_posted_has_no_address_block() {
    let fonts = Fonts::bundled();
    let mut l = letter("One line.", &[]);
    l.name = " ";
    let pages = layout(&l, &fonts).unwrap();
    assert_eq!(pages[0].lines[0].text, "Your policy is in arrears");
    assert!((pages[0].lines[0].y - (PAGE_HEIGHT - MARGIN_TOP - 14.0 * 1.4)).abs() < 0.01);

    l.title = "";
    let pages = layout(&l, &fonts).unwrap();
    assert_eq!(pages[0].lines[0].text, "One line.");
    let file = pdf(&l, &fonts).unwrap();
    assert_eq!(count(&file, b"/FontFile2"), 1, "only the face that is used is embedded");
}

/// Purpose: text too long for a page continues on the next, every page
/// numbered against the total, and a word wider than the line is cut
/// rather than run off the paper.
/// Why it matters: a policy wording runs to many pages; a line set past
/// the foot or the right edge is simply missing from what the customer
/// receives, and a loose page must say which document it belongs to.
#[test]
fn long_text_runs_onto_numbered_pages() {
    let fonts = Fonts::bundled();
    let paragraph = "The insurer will pay the sum insured when the life insured dies while this policy is in force. ".repeat(6);
    let text = format!("{}\n{}", [paragraph.as_str(); 12].join("\n\n"), "x".repeat(400));
    let pages = layout(&letter(&text, &[]), &fonts).unwrap();
    assert!(pages.len() >= 3, "{}", pages.len());
    for (n, page) in pages.iter().enumerate() {
        let (foot, body) = page.lines.split_last().unwrap();
        assert_eq!(foot.text, format!("doc-1    {} / {}", n + 1, pages.len()));
        assert!(!body.is_empty(), "no page is blank");
        assert!(body.iter().all(|l| l.y >= MARGIN_BOTTOM && l.y <= PAGE_HEIGHT - MARGIN_TOP));
    }
    // The unbroken run of 400 letters is cut into full lines.
    let cut: Vec<&str> = pages
        .iter()
        .flat_map(|p| &p.lines)
        .map(|l| l.text.as_str())
        .filter(|t| t.len() > 10 && t.chars().all(|c| c == 'x'))
        .collect();
    assert!(cut.len() > 1);
    assert_eq!(cut.iter().map(|t| t.len()).sum::<usize>(), 400);
    // No line is wider than the measure: at 11 point an x is about 5.5 points.
    assert!(cut.iter().all(|t| t.len() as f32 * 5.5 <= PAGE_WIDTH - 2.0 * MARGIN_SIDE + 1.0));

    let file = pdf(&letter(&text, &[]), &fonts).unwrap();
    assert_eq!(count(&file, b"/Type /Page\n") + count(&file, b"/Type /Page "), pages.len());

    let endless = "word\n\n".repeat(MAX_PAGES * 40);
    assert_eq!(layout(&letter(&endless, &[]), &fonts), Err(PdfError::TooLong));
}

/// Purpose: large print sets the text at 18 point and the title and foot
/// larger in proportion, on more lines.
/// Why it matters: large print is an alternate format customers with low
/// sight are entitled to ask for; a file marked large print but set at the
/// ordinary size fails them while the record says they were served.
#[test]
fn large_print_is_set_at_eighteen_point() {
    let fonts = Fonts::bundled();
    let address = address();
    let ordinary = layout(&letter(TEXT, &address), &fonts).unwrap();
    let mut l = letter(TEXT, &address);
    l.large_print = true;
    let large = layout(&l, &fonts).unwrap();
    let sizes: Vec<f32> = large[0].lines.iter().map(|l| l.size).collect();
    assert!(sizes.iter().all(|s| *s >= 14.0));
    assert_eq!(large[0].lines[4].size, 22.0);
    assert_eq!(large[0].lines[5].size, 18.0);
    assert_eq!(large[0].lines.last().unwrap().size, 14.0);
    assert!(large[0].lines.len() > ordinary[0].lines.len());
}

/// Purpose: a character the font has no glyph for is refused, wherever it
/// appears, and tabs, non-breaking spaces and control characters are set
/// as spaces or dropped.
/// Why it matters: a PDF that silently leaves out the characters of a name
/// or a clause says something different from the document on record; it is
/// better to send the text another way than to send it wrong.
#[test]
fn a_character_the_font_cannot_draw_is_refused() {
    let fonts = Fonts::bundled();
    let address = address();
    assert_eq!(layout(&letter("保险", &address), &fonts), Err(PdfError::NoGlyph('保')));
    assert_eq!(pdf(&letter("ok", &["保".to_string()]), &fonts).unwrap_err(), PdfError::NoGlyph('保'));
    let mut l = letter("ok", &address);
    l.title = "保";
    assert_eq!(layout(&l, &fonts), Err(PdfError::NoGlyph('保')));
    l.title = "ok";
    l.document = "保";
    assert_eq!(layout(&l, &fonts), Err(PdfError::NoGlyph('保')));
    assert_eq!(PdfError::NoGlyph('保').to_string(), "the font cannot draw `保` (U+4FDD)");

    let pages = layout(&letter("a\tb\u{a0}c\r\nd\u{7}e", &[]), &fonts).unwrap();
    assert_eq!(pages[0].lines[2].text, "a b c");
    assert_eq!(pages[0].lines[3].text, "de");
}

/// Purpose: the file is a PDF with one page object per page, both faces
/// embedded as small subsets, the title and language recorded, a map from
/// glyphs back to characters including the macron vowels, and no date; the
/// same letter gives the same bytes.
/// Why it matters: the subset keeps an emailed notice small, the character
/// map is what lets a screen reader and a search read the text, and a file
/// that differed on every rendering could not be compared with the copy a
/// customer says they received.
#[test]
fn the_file_embeds_what_a_reader_needs() {
    let fonts = Fonts::bundled();
    let address = address();
    let file = pdf(&letter(TEXT, &address), &fonts).unwrap();
    assert!(file.starts_with(b"%PDF-1.7"));
    assert!(file.ends_with(b"%%EOF"));
    assert_eq!(pdf(&letter(TEXT, &address), &fonts).unwrap(), file);
    assert!(file.len() < 60_000, "{} bytes: the fonts were not subset", file.len());
    assert_eq!(count(&file, b"/FontFile2"), 2);
    assert_eq!(count(&file, b"/Identity-H"), 2);
    assert_eq!(count(&file, b"/CreationDate"), 0);
    assert_eq!(count(&file, b"/Lang (en-NZ)"), 1);
    assert_eq!(count(&file, b"/Title (Your policy is in arrears)"), 1);
    assert_eq!(count(&file, b"/DisplayDocTitle true"), 1);

    let streams = streams(&file);
    let maps: Vec<String> = streams
        .iter()
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .filter(|s| s.contains("begincmap"))
        .collect();
    assert_eq!(maps.len(), 2);
    // ē and ā (U+0113, U+0101) are in the text face's map.
    assert!(maps.iter().any(|m| m.contains("<0113>") && m.contains("<0101>")), "{maps:?}");
    // One page of content, with one show operator per line.
    let pages = layout(&letter(TEXT, &address), &fonts).unwrap();
    let content: Vec<&Vec<u8>> = streams.iter().filter(|s| s.starts_with(b"BT")).collect();
    assert_eq!(content.len(), 1);
    assert_eq!(count(content[0], b" Tj"), pages[0].lines.len());
    // The embedded font programs are TrueType.
    assert_eq!(streams.iter().filter(|s| s.starts_with(&[0, 1, 0, 0])).count(), 2);

    let mut plain = letter(TEXT, &address);
    plain.language = "";
    assert_eq!(count(&pdf(&plain, &fonts).unwrap(), b"/Lang"), 0);
}

/// Purpose: the insurer's own faces are accepted when they are TrueType
/// fonts and refused when they are not.
/// Why it matters: the brand's font will replace the bundled one; a file
/// that is not a usable font must be refused when the service starts, not
/// when the first notice is due.
#[test]
fn the_insurers_own_fonts_are_checked() {
    let regular = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/fonts/LiberationSans-Regular.ttf")).unwrap();
    let bold = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/fonts/LiberationSans-Bold.ttf")).unwrap();
    let own = Fonts::new(regular.clone(), bold).unwrap();
    assert!(format!("{own:?}").starts_with("Fonts("));
    let address = address();
    assert_eq!(
        pdf(&letter(TEXT, &address), &own).unwrap(),
        pdf(&letter(TEXT, &address), &Fonts::bundled()).unwrap()
    );
    assert!(matches!(Fonts::new(b"not a font".to_vec(), regular.clone()), Err(PdfError::Font(_))));
    assert!(matches!(Fonts::new(regular.clone(), vec![]), Err(PdfError::Font(_))));
    // A font whose outlines are not TrueType cannot be embedded as one.
    let mut other = regular;
    let at = other.windows(4).position(|w| w == b"glyf").unwrap();
    other[at..at + 4].copy_from_slice(b"xlyf");
    assert_eq!(
        Fonts::new(other.clone(), other).unwrap_err().to_string(),
        "the font is not a TrueType font this service can use: it has no TrueType outlines"
    );
}
