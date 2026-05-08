use std::path::PathBuf;

use printpdf::*;
use pulldown_cmark::Alignment;

use crate::model::{Block, Span};
use crate::outline::{HeadingPos, inject_outline};
use crate::FontFamily;

// ── Page & typography constants ───────────────────────────────────────────────

// A4 dimensions in mm (fixed)
pub const PG_W: f32 = 210.0;
pub const PG_H: f32 = 297.0;

// Font sizes in pt
const BODY_SIZE: f32 = 11.0;
const CODE_SIZE: f32 = 9.0;
const TABLE_SIZE: f32 = 9.5;
const HEADING_SIZES: [f32; 6] = [24.0, 20.0, 16.0, 14.0, 12.0, 11.0];

// Vertical rhythm in mm (independent of margin)
const LINE_H: f32 = 5.5;
const PARA_GAP: f32 = 3.0;
const CODE_LINE_H: f32 = 4.5;
const TABLE_ROW_H: f32 = 6.5;      // minimum single-line row height
const TABLE_LINE_H: f32 = 4.2;     // line-to-line spacing within a cell
const TABLE_CELL_PAD_V: f32 = 1.5; // vertical padding above/below cell text
const TABLE_PAD: f32 = 1.5;

fn black() -> Color { Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)) }
fn grey(v: f32) -> Color { Color::Rgb(Rgb::new(v, v, v, None)) }

/// Composite an image onto an opaque white background and return RGB8 pixels.
/// See call site for why this is necessary.
fn flatten_to_rgb(img: &::image::DynamicImage) -> ::image::RgbImage {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out = ::image::RgbImage::new(w, h);
    for (x, y, px) in rgba.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        let a = a as f32 / 255.0;
        let blend = |c: u8| -> u8 {
            let v = c as f32 * a + 255.0 * (1.0 - a);
            v.round().clamp(0.0, 255.0) as u8
        };
        out.put_pixel(x, y, ::image::Rgb([blend(r), blend(g), blend(b)]));
    }
    out
}

// ── Font sets ─────────────────────────────────────────────────────────────────

pub struct FontSet {
    pub regular: IndirectFontRef,
    pub bold: IndirectFontRef,
    pub italic: IndirectFontRef,
    pub bold_italic: IndirectFontRef,
}

impl FontSet {
    pub fn choose(&self, bold: bool, italic: bool) -> &IndirectFontRef {
        match (bold, italic) {
            (true, true) => &self.bold_italic,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (false, false) => &self.regular,
        }
    }

    pub fn load(doc: &PdfDocumentReference, family: &FontFamily) -> Self {
        match family {
            FontFamily::Helvetica => Self {
                regular: doc.add_builtin_font(BuiltinFont::Helvetica).unwrap(),
                bold: doc.add_builtin_font(BuiltinFont::HelveticaBold).unwrap(),
                italic: doc.add_builtin_font(BuiltinFont::HelveticaOblique).unwrap(),
                bold_italic: doc.add_builtin_font(BuiltinFont::HelveticaBoldOblique).unwrap(),
            },
            FontFamily::Times => Self {
                regular: doc.add_builtin_font(BuiltinFont::TimesRoman).unwrap(),
                bold: doc.add_builtin_font(BuiltinFont::TimesBold).unwrap(),
                italic: doc.add_builtin_font(BuiltinFont::TimesItalic).unwrap(),
                bold_italic: doc.add_builtin_font(BuiltinFont::TimesBoldItalic).unwrap(),
            },
            FontFamily::Courier => Self {
                regular: doc.add_builtin_font(BuiltinFont::Courier).unwrap(),
                bold: doc.add_builtin_font(BuiltinFont::CourierBold).unwrap(),
                italic: doc.add_builtin_font(BuiltinFont::CourierOblique).unwrap(),
                bold_italic: doc.add_builtin_font(BuiltinFont::CourierBoldOblique).unwrap(),
            },
        }
    }
}

// ── Renderer ──────────────────────────────────────────────────────────────────

pub struct Renderer {
    doc: PdfDocumentReference,
    page: PdfPageIndex,
    layer: PdfLayerIndex,
    /// Current Y, measured from the top of the page in mm.
    y: f32,
    margin: f32,
    content_w: f32,

    /// Body text and headings.
    sans: FontSet,
    /// Blockquote text.
    serif: FontSet,
    /// Code blocks and inline code.
    mono: FontSet,

    /// Headings recorded in document order, used to build the PDF outline.
    headings: Vec<HeadingPos>,
    /// 0-based count of the current page (incremented each time we call new_page).
    page_num: usize,

    /// Directory used to resolve relative image paths.
    base_dir: PathBuf,
}

impl Renderer {
    pub fn new(
        title: &str,
        sans_family: &FontFamily,
        serif_family: &FontFamily,
        mono_family: &FontFamily,
        margin: f32,
        base_dir: PathBuf,
    ) -> Self {
        let (doc, page, layer) = PdfDocument::new(title, Mm(PG_W), Mm(PG_H), "Layer 1");
        let sans = FontSet::load(&doc, sans_family);
        let serif = FontSet::load(&doc, serif_family);
        let mono = FontSet::load(&doc, mono_family);
        let content_w = PG_W - 2.0 * margin;
        Self { doc, page, layer, y: margin, margin, content_w, sans, serif, mono, headings: Vec::new(), page_num: 0, base_dir }
    }

    fn current_layer(&self) -> PdfLayerReference {
        self.doc.get_page(self.page).get_layer(self.layer)
    }

    fn new_page(&mut self) {
        let (page, layer) = self.doc.add_page(Mm(PG_W), Mm(PG_H), "Layer 1");
        self.page = page;
        self.layer = layer;
        self.y = self.margin;
        self.page_num += 1;
    }

    fn ensure_space(&mut self, needed: f32) {
        if self.y + needed > PG_H - self.margin {
            self.new_page();
        }
    }

    /// top-relative Y → bottom-relative Mm for printpdf.
    fn by(&self, top_y: f32) -> Mm {
        Mm(PG_H - top_y)
    }

    /// Reset fill and stroke colors to black so prior graphic state doesn't bleed into text.
    fn reset_colors(&self) {
        let layer = self.current_layer();
        layer.set_fill_color(black());
        layer.set_outline_color(black());
    }

    // ── Span helpers ──────────────────────────────────────────────────────────

    fn char_w(size: f32, mono: bool) -> f32 {
        let factor = if mono { 0.6 } else { 0.55 };
        size * factor * 0.353
    }

    /// Flatten a span tree into (text, bold, italic, mono) leaf segments.
    fn flatten(spans: &[Span], b: bool, i: bool, m: bool) -> Vec<(String, bool, bool, bool)> {
        let mut out = Vec::new();
        for span in spans {
            match span {
                Span::Text(t) => out.push((t.clone(), b, i, m)),
                Span::Code(t) => out.push((t.clone(), false, false, true)),
                Span::Bold(inner) => out.extend(Self::flatten(inner, true, i, m)),
                Span::Italic(inner) => out.extend(Self::flatten(inner, b, true, m)),
                Span::Link { label, url } => {
                    let mut inner = Self::flatten(label, b, true, m);
                    if let Some(u) = url {
                        match inner.last_mut() {
                            Some(last) => last.0.push_str(&format!(" ({})", u)),
                            None => inner.push((format!("({})", u), b, true, m)),
                        }
                    }
                    out.extend(inner);
                }
                Span::Image { alt, .. } => {
                    // Fallback: when an image appears outside paragraph-level
                    // (e.g. nested in a link or inside a table cell) emit its
                    // alt text in italic so something readable remains.
                    if !alt.is_empty() { out.push((alt.clone(), b, true, m)); }
                }
                Span::SoftBreak => out.push((" ".to_string(), b, i, m)),
                Span::HardBreak => out.push(("\n".to_string(), b, i, m)),
            }
        }
        out
    }

    // ── Paragraph layout ──────────────────────────────────────────────────────

    /// Render mixed-style spans as word-wrapped lines.
    /// `body_fonts` is used for non-code text; inline code always uses `self.mono`.
    fn render_paragraph(
        &mut self,
        spans: &[Span],
        size: f32,
        indent: f32,
        width: f32,
        body_fonts: &FontSet,
    ) {
        struct Token { text: String, bold: bool, italic: bool, mono: bool }
        struct LTok<'a> { text: &'a str, bold: bool, italic: bool, mono: bool }

        let segments = Self::flatten(spans, false, false, false);
        let mut tokens: Vec<Token> = Vec::new();
        for (text, bold, italic, mono) in &segments {
            if text == "\n" {
                tokens.push(Token { text: "\n".to_owned(), bold: *bold, italic: *italic, mono: *mono });
            } else {
                for word in text.split_whitespace() {
                    tokens.push(Token { text: word.to_owned(), bold: *bold, italic: *italic, mono: *mono });
                }
            }
        }

        let mut lines: Vec<Vec<LTok>> = Vec::new();
        let mut cur: Vec<LTok> = Vec::new();
        let mut cur_w = 0.0_f32;

        for tok in &tokens {
            if tok.text == "\n" {
                lines.push(cur.drain(..).collect());
                cur_w = 0.0;
                continue;
            }
            let cw = Self::char_w(size, tok.mono);
            let ww = tok.text.chars().count() as f32 * cw;
            let sw = if cur.is_empty() { 0.0 } else { cw };
            if !cur.is_empty() && cur_w + sw + ww > width {
                lines.push(cur.drain(..).collect());
                cur_w = 0.0;
            }
            cur_w += (if cur.is_empty() { 0.0 } else { sw }) + ww;
            cur.push(LTok { text: &tok.text, bold: tok.bold, italic: tok.italic, mono: tok.mono });
        }
        if !cur.is_empty() {
            lines.push(cur);
        }

        for line in &lines {
            self.ensure_space(LINE_H);
            let y_pos = self.by(self.y + LINE_H * 0.75);
            let layer = self.current_layer();
            layer.begin_text_section();
            layer.set_text_cursor(Mm(self.margin + indent), y_pos);
            let mut first = true;
            // Always write the separator space using the body regular font so
            // that inline code tokens (mono) don't introduce a wider mono space
            // — which reads as extra horizontal padding around `code`.
            let sep_font = body_fonts.choose(false, false);
            for tok in line {
                let font = if tok.mono {
                    self.mono.choose(tok.bold, tok.italic)
                } else {
                    body_fonts.choose(tok.bold, tok.italic)
                };
                if !first {
                    layer.set_font(sep_font, size);
                    layer.write_text(" ", sep_font);
                }
                layer.set_font(font, size);
                layer.write_text(tok.text, font);
                first = false;
            }
            layer.end_text_section();
            self.y += LINE_H;
        }
    }

    // ── Table rendering ───────────────────────────────────────────────────────

    /// Ideal single-line width for a cell (including padding).
    fn cell_ideal_w(spans: &[Span]) -> f32 {
        let mut w = 0.0_f32;
        let mut first = true;
        for (text, _, _, mono) in Self::flatten(spans, false, false, false) {
            for word in text.split_whitespace() {
                let cw = Self::char_w(TABLE_SIZE, mono);
                let ww = word.chars().count() as f32 * cw;
                if !first { w += cw; }
                w += ww;
                first = false;
            }
        }
        w + TABLE_PAD * 2.0
    }

    /// Wrap cell text into lines fitting within `available_w` (inner, no padding).
    fn wrap_cell(spans: &[Span], available_w: f32) -> Vec<String> {
        let segs = Self::flatten(spans, false, false, false);
        let mut words: Vec<(String, bool)> = Vec::new();
        for (text, _, _, mono) in &segs {
            for word in text.split_whitespace() {
                words.push((word.to_owned(), *mono));
            }
        }
        if words.is_empty() {
            return vec![String::new()];
        }
        let mut lines: Vec<String> = Vec::new();
        let mut cur_line = String::new();
        let mut cur_w = 0.0_f32;
        for (word, mono) in &words {
            let cw = Self::char_w(TABLE_SIZE, *mono);
            let ww = word.chars().count() as f32 * cw;
            let sw = if cur_line.is_empty() { 0.0 } else { cw };
            if !cur_line.is_empty() && cur_w + sw + ww > available_w {
                lines.push(std::mem::take(&mut cur_line));
                cur_line = word.clone();
                cur_w = ww;
            } else {
                if !cur_line.is_empty() { cur_line.push(' '); }
                cur_line.push_str(word);
                cur_w += sw + ww;
            }
        }
        if !cur_line.is_empty() {
            lines.push(cur_line);
        }
        lines
    }

    fn render_table(
        &mut self,
        headers: &[Vec<Span>],
        rows: &[Vec<Vec<Span>>],
        _col_align: &[Alignment],
    ) {
        if headers.is_empty() { return; }
        let ncols = headers.len();

        // ── Compute proportional column widths ────────────────────────────────
        let min_col_w = TABLE_PAD * 2.0 + Self::char_w(TABLE_SIZE, false) * 4.0;
        let ideal: Vec<f32> = (0..ncols).map(|c| {
            let hdr_w = Self::cell_ideal_w(&headers[c]);
            let body_w = rows.iter()
                .map(|row| row.get(c).map_or(0.0, |cell| Self::cell_ideal_w(cell)))
                .fold(0.0_f32, f32::max);
            hdr_w.max(body_w).max(min_col_w)
        }).collect();
        let ideal_sum: f32 = ideal.iter().sum();
        let col_ws: Vec<f32> = ideal.iter().map(|w| w / ideal_sum * self.content_w).collect();

        // ── Pre-wrap all cells and compute row heights ────────────────────────
        let header_lines: Vec<Vec<String>> = (0..ncols).map(|c| {
            let available = (col_ws[c] - TABLE_PAD * 2.0).max(1.0);
            Self::wrap_cell(&headers[c], available)
        }).collect();
        let header_line_count = header_lines.iter().map(|l| l.len()).max().unwrap_or(1);
        let header_h = (TABLE_CELL_PAD_V * 2.0 + header_line_count as f32 * TABLE_LINE_H).max(TABLE_ROW_H);

        let body_wrapped: Vec<Vec<Vec<String>>> = rows.iter().map(|row| {
            (0..ncols).map(|c| {
                let available = (col_ws[c] - TABLE_PAD * 2.0).max(1.0);
                row.get(c).map_or_else(|| vec![String::new()], |cell| Self::wrap_cell(cell, available))
            }).collect()
        }).collect();
        let row_heights: Vec<f32> = body_wrapped.iter().map(|row| {
            let max_lines = row.iter().map(|cell| cell.len()).max().unwrap_or(1);
            (TABLE_CELL_PAD_V * 2.0 + max_lines as f32 * TABLE_LINE_H).max(TABLE_ROW_H)
        }).collect();

        let table_h = header_h + row_heights.iter().sum::<f32>();
        self.ensure_space(table_h + PARA_GAP);

        let table_top = self.y;
        let table_left = self.margin;
        let table_right = self.margin + self.content_w;

        // ── Header row ────────────────────────────────────────────────────────
        {
            let layer = self.current_layer();
            layer.set_fill_color(grey(0.88));
            layer.set_outline_color(grey(0.60));
            layer.set_outline_thickness(0.3);
            layer.add_rect(Rect::new(
                Mm(table_left),
                self.by(table_top + header_h),
                Mm(table_right),
                self.by(table_top),
            ));
            self.reset_colors();

            let mut col_x = table_left;
            for (c, lines) in header_lines.iter().enumerate() {
                let cell_x = col_x + TABLE_PAD;
                for (li, line) in lines.iter().enumerate() {
                    let y_pos = self.by(table_top + TABLE_CELL_PAD_V + (li as f32 + 0.82) * TABLE_LINE_H);
                    let layer = self.current_layer();
                    layer.begin_text_section();
                    layer.set_font(&self.sans.bold, TABLE_SIZE);
                    layer.set_text_cursor(Mm(cell_x), y_pos);
                    layer.write_text(line, &self.sans.bold);
                    layer.end_text_section();
                }
                col_x += col_ws[c];
            }
        }

        // ── Body rows ─────────────────────────────────────────────────────────
        let mut row_top = table_top + header_h;
        for (r, (row_cells, &row_h)) in body_wrapped.iter().zip(row_heights.iter()).enumerate() {
            if r % 2 == 1 {
                let layer = self.current_layer();
                layer.set_fill_color(grey(0.96));
                layer.set_outline_thickness(0.0);
                layer.add_rect(Rect::new(
                    Mm(table_left),
                    self.by(row_top + row_h),
                    Mm(table_right),
                    self.by(row_top),
                ));
                self.reset_colors();
            }

            let mut col_x = table_left;
            for (c, lines) in row_cells.iter().enumerate() {
                let cell_x = col_x + TABLE_PAD;
                for (li, line) in lines.iter().enumerate() {
                    let y_pos = self.by(row_top + TABLE_CELL_PAD_V + (li as f32 + 0.82) * TABLE_LINE_H);
                    let layer = self.current_layer();
                    layer.begin_text_section();
                    layer.set_font(&self.sans.regular, TABLE_SIZE);
                    layer.set_text_cursor(Mm(cell_x), y_pos);
                    layer.write_text(line, &self.sans.regular);
                    layer.end_text_section();
                }
                col_x += col_ws[c];
            }
            row_top += row_h;
        }

        // ── Grid lines ────────────────────────────────────────────────────────
        let layer = self.current_layer();
        layer.set_outline_color(grey(0.60));
        layer.set_outline_thickness(0.3);

        // Horizontal lines
        layer.add_line(Line {
            points: vec![
                (Point::new(Mm(table_left), self.by(table_top)), false),
                (Point::new(Mm(table_right), self.by(table_top)), false),
            ],
            is_closed: false,
        });
        let mut ry = table_top + header_h;
        layer.add_line(Line {
            points: vec![
                (Point::new(Mm(table_left), self.by(ry)), false),
                (Point::new(Mm(table_right), self.by(ry)), false),
            ],
            is_closed: false,
        });
        for &row_h in &row_heights {
            ry += row_h;
            layer.add_line(Line {
                points: vec![
                    (Point::new(Mm(table_left), self.by(ry)), false),
                    (Point::new(Mm(table_right), self.by(ry)), false),
                ],
                is_closed: false,
            });
        }

        // Vertical lines
        let grid_top = self.by(table_top);
        let grid_bot = self.by(table_top + table_h);
        let mut cx = table_left;
        for c in 0..=ncols {
            layer.add_line(Line {
                points: vec![
                    (Point::new(Mm(cx), grid_top), false),
                    (Point::new(Mm(cx), grid_bot), false),
                ],
                is_closed: false,
            });
            if c < ncols { cx += col_ws[c]; }
        }

        self.reset_colors();
        self.y = table_top + table_h + PARA_GAP;
    }

    // ── Block rendering ───────────────────────────────────────────────────────

    /// `body_fonts` is the active font set for paragraph text.
    /// Headings always use `self.sans`; blockquotes switch to `self.serif`.
    fn render_block(&mut self, block: &Block, indent: f32, marker: Option<&str>, body_fonts: &FontSet) {
        // Safety: we borrow `body_fonts` from `self.sans` or `self.serif`, but
        // we need to call `&self.sans` / `&self.serif` later. Use raw pointer
        // indirection to satisfy the borrow checker without cloning font refs.
        let body_fonts = body_fonts as *const FontSet;

        match block {
            Block::Heading { level, spans } => {
                let size = HEADING_SIZES[(*level as usize).saturating_sub(1)];
                let lh = size * 0.353 * 1.4;
                let top_gap = if *level == 1 { 6.0 } else { 4.0 };

                let text: String = Self::flatten(spans, false, false, false)
                    .into_iter().map(|(t, ..)| t).collect::<Vec<_>>().join("");

                // Word-wrap the heading so long titles don't run off the page.
                // Heading font is bold sans; estimate width with the same
                // char_w model used elsewhere.
                let avail_w = self.content_w - indent;
                let cw = Self::char_w(size, false);
                let mut wrapped: Vec<String> = Vec::new();
                let mut cur = String::new();
                let mut cur_w = 0.0_f32;
                for word in text.split_whitespace() {
                    let ww = word.chars().count() as f32 * cw;
                    let sw = if cur.is_empty() { 0.0 } else { cw };
                    if !cur.is_empty() && cur_w + sw + ww > avail_w {
                        wrapped.push(std::mem::take(&mut cur));
                        cur = word.to_owned();
                        cur_w = ww;
                    } else {
                        if !cur.is_empty() { cur.push(' '); }
                        cur.push_str(word);
                        cur_w += sw + ww;
                    }
                }
                if !cur.is_empty() { wrapped.push(cur); }
                if wrapped.is_empty() { wrapped.push(String::new()); }

                let total_h = wrapped.len() as f32 * lh;
                self.ensure_space(top_gap + total_h + PARA_GAP);
                self.y += top_gap;

                if *level <= 2 {
                    let ry = self.by(self.y + total_h + 1.5);
                    let layer = self.current_layer();
                    layer.set_outline_color(grey(0.75));
                    layer.set_outline_thickness(0.3);
                    layer.add_line(Line {
                        points: vec![
                            (Point::new(Mm(self.margin + indent), ry), false),
                            (Point::new(Mm(PG_W - self.margin), ry), false),
                        ],
                        is_closed: false,
                    });
                    self.reset_colors();
                }

                // Record position for the PDF outline (before advancing y).
                self.headings.push(HeadingPos {
                    title: text,
                    level: *level,
                    page_idx: self.page_num,
                    y_top_mm: self.y,
                });

                for line in &wrapped {
                    let y_pos = self.by(self.y + lh * 0.8);
                    let layer = self.current_layer();
                    layer.begin_text_section();
                    layer.set_font(&self.sans.bold, size);
                    layer.set_text_cursor(Mm(self.margin + indent), y_pos);
                    layer.write_text(line, &self.sans.bold);
                    layer.end_text_section();
                    self.y += lh;
                }

                self.y += PARA_GAP;
            }

            Block::Paragraph(spans) => {
                // Split the paragraph at any top-level image span and render
                // each image as its own block — this matches typical Markdown
                // usage of `![alt](path)` on its own line as a figure.
                enum Chunk<'a> { Text(Vec<&'a Span>), Image(&'a str, &'a str) }
                let mut chunks: Vec<Chunk> = Vec::new();
                let mut buf: Vec<&Span> = Vec::new();
                for s in spans {
                    if let Span::Image { path, alt } = s {
                        if !buf.is_empty() {
                            chunks.push(Chunk::Text(std::mem::take(&mut buf)));
                        }
                        chunks.push(Chunk::Image(path, alt));
                    } else {
                        buf.push(s);
                    }
                }
                if !buf.is_empty() { chunks.push(Chunk::Text(buf)); }

                let mut first = true;
                for chunk in chunks {
                    match chunk {
                        Chunk::Text(refs) => {
                            let owned: Vec<Span> = refs.into_iter().cloned().collect();
                            if first {
                                if let Some(m) = marker {
                                    self.ensure_space(LINE_H);
                                    let bf = unsafe { &*body_fonts };
                                    let y_pos = self.by(self.y + LINE_H * 0.75);
                                    let layer = self.current_layer();
                                    layer.begin_text_section();
                                    layer.set_font(&bf.regular, BODY_SIZE);
                                    layer.set_text_cursor(Mm(self.margin + indent - 5.0), y_pos);
                                    layer.write_text(m, &bf.regular);
                                    layer.end_text_section();
                                }
                            }
                            let bf = unsafe { &*body_fonts };
                            self.render_paragraph(&owned, BODY_SIZE, indent, self.content_w - indent, bf);
                        }
                        Chunk::Image(path, alt) => {
                            self.render_image(path, alt, indent);
                        }
                    }
                    first = false;
                }
                self.y += PARA_GAP;
            }

            Block::CodeBlock { code, .. } => {
                let code_lines: Vec<&str> = code.lines().collect();
                let block_h = code_lines.len() as f32 * CODE_LINE_H + 4.0;
                self.ensure_space(block_h + PARA_GAP);

                let layer = self.current_layer();
                layer.set_fill_color(grey(0.94));
                layer.set_outline_color(grey(0.80));
                layer.set_outline_thickness(0.3);
                layer.add_rect(Rect::new(
                    Mm(self.margin - 2.0),
                    self.by(self.y + block_h),
                    Mm(PG_W - self.margin + 2.0),
                    self.by(self.y),
                ));
                // Reset fill to black: set_fill_color is the PDF non-stroking color,
                // which applies to text fill as well as shape fills.
                self.reset_colors();

                self.y += 2.0;
                for cl in &code_lines {
                    self.ensure_space(CODE_LINE_H);
                    let y_pos = self.by(self.y + CODE_LINE_H * 0.8);
                    let layer = self.current_layer();
                    layer.begin_text_section();
                    layer.set_font(&self.mono.regular, CODE_SIZE);
                    layer.set_text_cursor(Mm(self.margin + 1.0), y_pos);
                    layer.write_text(*cl, &self.mono.regular);
                    layer.end_text_section();
                    self.y += CODE_LINE_H;
                }
                self.y += 2.0 + PARA_GAP;
            }

            Block::Table { col_align, headers, rows } => {
                self.render_table(headers, rows, col_align);
            }

            Block::BulletList(items) => {
                let bf = unsafe { &*body_fonts };
                for item_blocks in items {
                    self.render_list_item(item_blocks, indent + 5.0, false, 0, bf);
                }
                self.y += PARA_GAP * 0.5;
            }

            Block::OrderedList(items) => {
                let bf = unsafe { &*body_fonts };
                for (i, item_blocks) in items.iter().enumerate() {
                    self.render_list_item(item_blocks, indent + 5.0, true, i + 1, bf);
                }
                self.y += PARA_GAP * 0.5;
            }

            Block::BlockQuote(blocks) => {
                let start_y = self.y;
                // Blockquotes use the serif font set regardless of body font.
                let serif_ptr = &self.serif as *const FontSet;
                for b in blocks {
                    let sf = unsafe { &*serif_ptr };
                    self.render_block(b, indent + 8.0, None, sf);
                }
                let end_y = self.y;
                let layer = self.current_layer();
                layer.set_outline_color(grey(0.60));
                layer.set_outline_thickness(1.5);
                layer.add_line(Line {
                    points: vec![
                        (Point::new(Mm(self.margin + indent + 2.0), self.by(start_y)), false),
                        (Point::new(Mm(self.margin + indent + 2.0), self.by(end_y)), false),
                    ],
                    is_closed: false,
                });
                self.reset_colors();
            }

            Block::Rule => {
                self.ensure_space(6.0);
                self.y += 3.0;
                let y_pos = self.by(self.y);
                let layer = self.current_layer();
                layer.set_outline_color(grey(0.70));
                layer.set_outline_thickness(0.5);
                layer.add_line(Line {
                    points: vec![
                        (Point::new(Mm(self.margin), y_pos), false),
                        (Point::new(Mm(PG_W - self.margin), y_pos), false),
                    ],
                    is_closed: false,
                });
                self.reset_colors();
                self.y += 3.0 + PARA_GAP;
            }
        }
    }

    // ── Image rendering ───────────────────────────────────────────────────────

    fn render_image(&mut self, rel_path: &str, alt: &str, indent: f32) {
        let full_path = self.base_dir.join(rel_path);
        let bytes = match std::fs::read(&full_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("warning: image {} not read: {}", full_path.display(), e);
                self.render_image_fallback(alt, indent);
                return;
            }
        };
        let dyn_img = match ::image::load_from_memory(&bytes) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("warning: image {} not decoded: {}", full_path.display(), e);
                self.render_image_fallback(alt, indent);
                return;
            }
        };

        // printpdf 0.7 emits RGBA images with an inline-stream /SMask dict that
        // breaks the lopdf round-trip used for the PDF outline (the image
        // XObject silently fails to re-load and the page resource ends up
        // dangling). Flatten the alpha against white so we ship a plain RGB
        // image — PNG/JPEG plots from matplotlib have white backgrounds, so
        // this is visually identical for the report case and works correctly
        // through the outline injection step.
        let dyn_img = ::image::DynamicImage::ImageRgb8(flatten_to_rgb(&dyn_img));

        let px_w = dyn_img.width().max(1) as f32;
        let px_h = dyn_img.height().max(1) as f32;
        let aspect = px_h / px_w;

        // Fit width to the available column, capping height so a single image
        // never exceeds 80% of the printable page height.
        let avail_w = (self.content_w - indent).max(20.0);
        let max_h = (PG_H - 2.0 * self.margin) * 0.85;
        let mut w_mm = avail_w;
        let mut h_mm = w_mm * aspect;
        if h_mm > max_h {
            h_mm = max_h;
            w_mm = h_mm / aspect;
        }

        // If we don't have room on the current page, push to the next.
        if self.y + h_mm > PG_H - self.margin {
            self.new_page();
        }

        // dpi chosen so px_w / dpi * 25.4 == w_mm.
        let dpi = px_w * 25.4 / w_mm;

        let pdf_img = Image::from_dynamic_image(&dyn_img);
        let layer = self.current_layer();
        pdf_img.add_to_layer(layer, ImageTransform {
            translate_x: Some(Mm(self.margin + indent)),
            translate_y: Some(self.by(self.y + h_mm)),
            rotate: None,
            scale_x: Some(1.0),
            scale_y: Some(1.0),
            dpi: Some(dpi),
        });
        self.reset_colors();
        self.y += h_mm;
    }

    fn render_image_fallback(&mut self, alt: &str, indent: f32) {
        let label = if alt.is_empty() { "[image]".to_string() } else { format!("[image: {}]", alt) };
        let spans = vec![Span::Italic(vec![Span::Text(label)])];
        let sans_ptr = &self.sans as *const FontSet;
        let bf = unsafe { &*sans_ptr };
        self.render_paragraph(&spans, BODY_SIZE, indent, self.content_w - indent, bf);
    }

    fn render_list_item(
        &mut self,
        blocks: &[Block],
        indent: f32,
        ordered: bool,
        n: usize,
        body_fonts: &FontSet,
    ) {
        let marker = if ordered { format!("{}.", n) } else { "•".to_string() };
        let mut first = true;
        let bf = body_fonts as *const FontSet;
        for block in blocks {
            let body_fonts = unsafe { &*bf };
            self.render_block(block, indent, if first { Some(&marker) } else { None }, body_fonts);
            first = false;
        }
    }

    pub fn render(mut self, blocks: &[Block]) -> Vec<u8> {
        let sans_ptr = &self.sans as *const FontSet;
        for block in blocks {
            let sans = unsafe { &*sans_ptr };
            self.render_block(block, 0.0, None, sans);
        }
        let headings = self.headings;
        let bytes = self.doc.save_to_bytes().expect("printpdf serialization failed");
        inject_outline(bytes, &headings)
    }
}
