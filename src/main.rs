use clap::{Parser as ClapParser, ValueEnum};
use lopdf::Object as LObj;
use printpdf::*;
use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser as MdParser, Tag, TagEnd};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

// ─── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Clone, ValueEnum)]
enum FontFamily {
    Helvetica,
    Times,
    Courier,
}

#[derive(ClapParser)]
#[command(name = "md2pdf", about = "Convert a Markdown file to PDF")]
struct Cli {
    /// Input Markdown file
    input: PathBuf,

    /// Output PDF file (defaults to <input>.pdf)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Font for body text and headings
    #[arg(long, default_value = "helvetica", value_name = "FONT")]
    sans: FontFamily,

    /// Font for blockquotes
    #[arg(long, default_value = "times", value_name = "FONT")]
    serif: FontFamily,

    /// Font for code blocks
    #[arg(long, default_value = "courier", value_name = "FONT")]
    mono: FontFamily,

    /// Page margin in mm
    #[arg(long, default_value_t = 20.0, value_name = "MM")]
    margin: f32,
}

// ─── Document model ───────────────────────────────────────────────────────────

#[derive(Debug)]
#[allow(dead_code)]
enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph(Vec<Span>),
    CodeBlock { lang: Option<String>, code: String },
    Table { col_align: Vec<Alignment>, headers: Vec<Vec<Span>>, rows: Vec<Vec<Vec<Span>>> },
    BulletList(Vec<Vec<Block>>),
    OrderedList(Vec<Vec<Block>>),
    BlockQuote(Vec<Block>),
    Rule,
}

#[derive(Debug, Clone)]
enum Span {
    Text(String),
    Bold(Vec<Span>),
    Italic(Vec<Span>),
    Code(String),
    /// url is None for anchor links (#…) which are meaningless in PDF.
    Link { label: Vec<Span>, url: Option<String> },
    SoftBreak,
    HardBreak,
}

// ─── Markdown → model ─────────────────────────────────────────────────────────

struct ModelBuilder {
    block_stack: Vec<BlockContext>,
    span_stack: Vec<SpanContext>,
}

enum BlockContext {
    Root(Vec<Block>),
    BlockQuote(Vec<Block>),
    ListItem(Vec<Block>),
    BulletList(Vec<Vec<Block>>),
    OrderedList(Vec<Vec<Block>>),
    Table {
        col_align: Vec<Alignment>,
        headers: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
        in_head: bool,
        cur_row: Vec<Vec<Span>>,
    },
}

enum SpanContext {
    Paragraph(Vec<Span>),
    Heading { level: u8, spans: Vec<Span> },
    Bold(Vec<Span>),
    Italic(Vec<Span>),
    Link { url: String, spans: Vec<Span> },
    CodeBlock { lang: Option<String>, code: String },
    TableCell(Vec<Span>),
}

impl ModelBuilder {
    fn new() -> Self {
        Self {
            block_stack: vec![BlockContext::Root(Vec::new())],
            span_stack: Vec::new(),
        }
    }

    fn push_span(&mut self, s: Span) {
        // Tight list items emit inline events directly inside Item with no
        // enclosing Paragraph tag.  Auto-create one so content isn't dropped.
        if self.span_stack.is_empty() {
            if matches!(self.block_stack.last(), Some(BlockContext::ListItem(_))) {
                self.span_stack.push(SpanContext::Paragraph(Vec::new()));
            }
        }
        match self.span_stack.last_mut() {
            Some(SpanContext::Paragraph(v))
            | Some(SpanContext::Heading { spans: v, .. })
            | Some(SpanContext::Bold(v))
            | Some(SpanContext::Italic(v))
            | Some(SpanContext::Link { spans: v, .. })
            | Some(SpanContext::TableCell(v)) => v.push(s),
            _ => {}
        }
    }

    fn push_block(&mut self, b: Block) {
        let ctx = self.block_stack.last_mut().expect("block stack empty");
        match ctx {
            BlockContext::Root(v) | BlockContext::BlockQuote(v) | BlockContext::ListItem(v) => {
                v.push(b)
            }
            _ => {}
        }
    }

    fn build(self) -> Vec<Block> {
        match self.block_stack.into_iter().next().unwrap() {
            BlockContext::Root(v) => v,
            _ => vec![],
        }
    }
}

fn parse_markdown(src: &str) -> Vec<Block> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);

    let parser = MdParser::new_ext(src, opts);
    let mut mb = ModelBuilder::new();

    for event in parser {
        match event {
            // ── Block opens ──────────────────────────────────────────────────
            Event::Start(Tag::Heading { level, .. }) => {
                mb.span_stack.push(SpanContext::Heading {
                    level: heading_level(level),
                    spans: Vec::new(),
                });
            }
            Event::Start(Tag::Paragraph) => {
                mb.span_stack.push(SpanContext::Paragraph(Vec::new()));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(s) if !s.is_empty() => {
                        Some(s.to_string())
                    }
                    _ => None,
                };
                mb.span_stack.push(SpanContext::CodeBlock { lang, code: String::new() });
            }
            Event::Start(Tag::BlockQuote(_)) => {
                mb.block_stack.push(BlockContext::BlockQuote(Vec::new()));
            }
            Event::Start(Tag::List(None)) => {
                mb.block_stack.push(BlockContext::BulletList(Vec::new()));
            }
            Event::Start(Tag::List(Some(_))) => {
                mb.block_stack.push(BlockContext::OrderedList(Vec::new()));
            }
            Event::Start(Tag::Item) => {
                mb.block_stack.push(BlockContext::ListItem(Vec::new()));
            }
            Event::Start(Tag::Table(aligns)) => {
                mb.block_stack.push(BlockContext::Table {
                    col_align: aligns.to_vec(),
                    headers: Vec::new(),
                    rows: Vec::new(),
                    in_head: false,
                    cur_row: Vec::new(),
                });
            }
            Event::Start(Tag::TableHead) => {
                if let Some(BlockContext::Table { in_head, .. }) = mb.block_stack.last_mut() {
                    *in_head = true;
                }
            }
            Event::Start(Tag::TableRow) => {}
            Event::Start(Tag::TableCell) => {
                mb.span_stack.push(SpanContext::TableCell(Vec::new()));
            }

            // ── Block closes ─────────────────────────────────────────────────
            Event::End(TagEnd::Heading(_)) => {
                if let Some(SpanContext::Heading { level, spans }) = mb.span_stack.pop() {
                    mb.push_block(Block::Heading { level, spans });
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if let Some(SpanContext::Paragraph(spans)) = mb.span_stack.pop() {
                    mb.push_block(Block::Paragraph(spans));
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(SpanContext::CodeBlock { lang, code }) = mb.span_stack.pop() {
                    mb.push_block(Block::CodeBlock { lang, code });
                }
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                if let Some(BlockContext::BlockQuote(blocks)) = mb.block_stack.pop() {
                    mb.push_block(Block::BlockQuote(blocks));
                }
            }
            Event::End(TagEnd::List(false)) => {
                if let Some(BlockContext::BulletList(items)) = mb.block_stack.pop() {
                    mb.push_block(Block::BulletList(items));
                }
            }
            Event::End(TagEnd::List(true)) => {
                if let Some(BlockContext::OrderedList(items)) = mb.block_stack.pop() {
                    mb.push_block(Block::OrderedList(items));
                }
            }
            Event::End(TagEnd::Item) => {
                // Flush any implicit paragraph opened for tight list item content.
                if matches!(mb.span_stack.last(), Some(SpanContext::Paragraph(_))) {
                    if let Some(SpanContext::Paragraph(spans)) = mb.span_stack.pop() {
                        mb.push_block(Block::Paragraph(spans));
                    }
                }
                if let Some(BlockContext::ListItem(blocks)) = mb.block_stack.pop() {
                    if let Some(ctx) = mb.block_stack.last_mut() {
                        match ctx {
                            BlockContext::BulletList(items)
                            | BlockContext::OrderedList(items) => items.push(blocks),
                            _ => {}
                        }
                    }
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(SpanContext::TableCell(spans)) = mb.span_stack.pop() {
                    if let Some(BlockContext::Table { cur_row, .. }) = mb.block_stack.last_mut() {
                        cur_row.push(spans);
                    }
                }
            }
            Event::End(TagEnd::TableRow) | Event::End(TagEnd::TableHead) => {
                if let Some(BlockContext::Table { headers, rows, in_head, cur_row, .. }) =
                    mb.block_stack.last_mut()
                {
                    let row = cur_row.drain(..).collect::<Vec<_>>();
                    if *in_head {
                        *headers = row;
                        if matches!(event, Event::End(TagEnd::TableHead)) {
                            *in_head = false;
                        }
                    } else {
                        rows.push(row);
                    }
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(BlockContext::Table { col_align, headers, rows, .. }) =
                    mb.block_stack.pop()
                {
                    mb.push_block(Block::Table { col_align, headers, rows });
                }
            }

            // ── Inline opens ─────────────────────────────────────────────────
            Event::Start(Tag::Strong) => {
                mb.span_stack.push(SpanContext::Bold(Vec::new()));
            }
            Event::Start(Tag::Emphasis) => {
                mb.span_stack.push(SpanContext::Italic(Vec::new()));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                mb.span_stack.push(SpanContext::Link {
                    url: dest_url.to_string(),
                    spans: Vec::new(),
                });
            }

            // ── Inline closes ────────────────────────────────────────────────
            Event::End(TagEnd::Strong) => {
                if let Some(SpanContext::Bold(spans)) = mb.span_stack.pop() {
                    mb.push_span(Span::Bold(spans));
                }
            }
            Event::End(TagEnd::Emphasis) => {
                if let Some(SpanContext::Italic(spans)) = mb.span_stack.pop() {
                    mb.push_span(Span::Italic(spans));
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some(SpanContext::Link { url, spans }) = mb.span_stack.pop() {
                    let display_url = if url.starts_with('#') { None } else { Some(url) };
                    mb.push_span(Span::Link { label: spans, url: display_url });
                }
            }

            // ── Leaf content ─────────────────────────────────────────────────
            Event::Text(t) => {
                let text = t.to_string();
                if let Some(SpanContext::CodeBlock { code, .. }) = mb.span_stack.last_mut() {
                    code.push_str(&text);
                } else {
                    mb.push_span(Span::Text(text));
                }
            }
            Event::Code(t) => mb.push_span(Span::Code(t.to_string())),
            Event::SoftBreak => mb.push_span(Span::SoftBreak),
            Event::HardBreak => mb.push_span(Span::HardBreak),
            Event::Rule => mb.push_block(Block::Rule),

            _ => {}
        }
    }

    mb.build()
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

// ─── PDF renderer ─────────────────────────────────────────────────────────────

// A4 dimensions in mm (fixed)
const PG_W: f32 = 210.0;
const PG_H: f32 = 297.0;

// Font sizes in pt
const BODY_SIZE: f32 = 11.0;
const CODE_SIZE: f32 = 9.0;
const TABLE_SIZE: f32 = 9.5;
const HEADING_SIZES: [f32; 6] = [24.0, 20.0, 16.0, 14.0, 12.0, 11.0];

// Vertical rhythm in mm (independent of margin)
const LINE_H: f32 = 5.5;
const PARA_GAP: f32 = 3.0;
const CODE_LINE_H: f32 = 4.5;
const TABLE_ROW_H: f32 = 6.5;   // minimum single-line row height
const TABLE_LINE_H: f32 = 4.2;  // line-to-line spacing within a cell
const TABLE_CELL_PAD_V: f32 = 1.5; // vertical padding above/below cell text
const TABLE_PAD: f32 = 1.5;

fn black() -> Color { Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)) }
fn grey(v: f32) -> Color { Color::Rgb(Rgb::new(v, v, v, None)) }

// ── Font sets ────────────────────────────────────────────────────────────────

struct FontSet {
    regular: IndirectFontRef,
    bold: IndirectFontRef,
    italic: IndirectFontRef,
    bold_italic: IndirectFontRef,
}

impl FontSet {
    fn choose(&self, bold: bool, italic: bool) -> &IndirectFontRef {
        match (bold, italic) {
            (true, true) => &self.bold_italic,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (false, false) => &self.regular,
        }
    }

    fn load(doc: &PdfDocumentReference, family: &FontFamily) -> Self {
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

// ── Renderer ─────────────────────────────────────────────────────────────────

/// Position of a heading, collected during rendering for the PDF outline.
struct HeadingPos {
    title: String,
    level: u8,
    /// 0-based page index.
    page_idx: usize,
    /// Y from the top of the page in mm, pointing at the top of the heading text.
    y_top_mm: f32,
}

struct Renderer {
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
}

impl Renderer {
    fn new(title: &str, sans_family: &FontFamily, serif_family: &FontFamily, mono_family: &FontFamily, margin: f32) -> Self {
        let (doc, page, layer) = PdfDocument::new(title, Mm(PG_W), Mm(PG_H), "Layer 1");
        let sans = FontSet::load(&doc, sans_family);
        let serif = FontSet::load(&doc, serif_family);
        let mono = FontSet::load(&doc, mono_family);
        let content_w = PG_W - 2.0 * margin;
        Self { doc, page, layer, y: margin, margin, content_w, sans, serif, mono, headings: Vec::new(), page_num: 0 }
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

    // ── Span helpers ─────────────────────────────────────────────────────────

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
                Span::SoftBreak => out.push((" ".to_string(), b, i, m)),
                Span::HardBreak => out.push(("\n".to_string(), b, i, m)),
            }
        }
        out
    }

    // ── Paragraph layout ─────────────────────────────────────────────────────

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
            for tok in line {
                let font = if tok.mono {
                    self.mono.choose(tok.bold, tok.italic)
                } else {
                    body_fonts.choose(tok.bold, tok.italic)
                };
                layer.set_font(font, size);
                if !first { layer.write_text(" ", font); }
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

    // ── Block rendering ──────────────────────────────────────────────────────

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

                self.ensure_space(top_gap + lh + PARA_GAP);
                self.y += top_gap;

                if *level <= 2 {
                    let ry = self.by(self.y + lh + 1.5);
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

                let text: String = Self::flatten(spans, false, false, false)
                    .into_iter().map(|(t, ..)| t).collect::<Vec<_>>().join("");

                // Record position for the PDF outline (before advancing y).
                self.headings.push(HeadingPos {
                    title: text.clone(),
                    level: *level,
                    page_idx: self.page_num,
                    y_top_mm: self.y,
                });

                let y_pos = self.by(self.y + lh * 0.8);
                let layer = self.current_layer();
                layer.begin_text_section();
                layer.set_font(&self.sans.bold, size);
                layer.set_text_cursor(Mm(self.margin + indent), y_pos);
                layer.write_text(&text, &self.sans.bold);
                layer.end_text_section();

                self.y += lh + PARA_GAP;
            }

            Block::Paragraph(spans) => {
                if let Some(m) = marker {
                    // Ensure space before drawing the marker so it doesn't end
                    // up on a different page than the paragraph text.
                    self.ensure_space(LINE_H);
                    // SAFETY: body_fonts points into self which we hold mutably;
                    // the reference is only used before any mutation below.
                    let bf = unsafe { &*body_fonts };
                    let y_pos = self.by(self.y + LINE_H * 0.75);
                    let layer = self.current_layer();
                    layer.begin_text_section();
                    layer.set_font(&bf.regular, BODY_SIZE);
                    layer.set_text_cursor(Mm(self.margin + indent - 5.0), y_pos);
                    layer.write_text(m, &bf.regular);
                    layer.end_text_section();
                }
                let bf = unsafe { &*body_fonts };
                self.render_paragraph(spans, BODY_SIZE, indent, self.content_w - indent, bf);
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

    fn render(mut self, blocks: &[Block]) -> Vec<u8> {
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

// ─── PDF outline injection ────────────────────────────────────────────────────

/// 1mm in PDF user-space points.
const MM_TO_PT: f32 = 2.8346;

/// Build a hierarchical PDF outline (bookmark tree) from the collected headings
/// and inject it into the already-serialised PDF bytes via lopdf.
///
/// The tree is built with a parent-stack algorithm: as we process headings in
/// order, we pop the stack until the top has a strictly lower level than the
/// current heading, making it the current heading's parent.
fn inject_outline(pdf_bytes: Vec<u8>, headings: &[HeadingPos]) -> Vec<u8> {
    if headings.is_empty() {
        return pdf_bytes;
    }

    let mut doc = lopdf::Document::load_mem(&pdf_bytes)
        .expect("lopdf failed to parse printpdf output");

    // lopdf page map: 1-based page number → ObjectId
    let pages = doc.get_pages();

    // Pre-allocate one ObjectId per heading, plus one for the root.
    let root_id = doc.new_object_id();
    let item_ids: Vec<lopdf::ObjectId> =
        (0..headings.len()).map(|_| doc.new_object_id()).collect();

    // ── Build tree structure ──────────────────────────────────────────────────
    //
    // parent[i]   = index of parent item, or None (root-level)
    // children[i] = ordered list of child indices
    // prev[i]     = previous sibling index
    // next[i]     = next sibling index

    let n = headings.len();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut prev: Vec<Option<usize>> = vec![None; n];
    let mut next: Vec<Option<usize>> = vec![None; n];

    // Stack holds indices of "open" ancestor items (ordered lowest→highest level).
    let mut stack: Vec<usize> = Vec::new();

    for i in 0..n {
        let level = headings[i].level;
        // Pop ancestors whose level is >= current level — they are closed by this heading.
        while let Some(&top) = stack.last() {
            if headings[top].level >= level {
                stack.pop();
            } else {
                break;
            }
        }
        let par = stack.last().copied();
        parent[i] = par;

        // Link as sibling of the last child of our parent (if any).
        let siblings = match par {
            Some(p) => &mut children[p],
            None => {
                // We need a temporary workaround: collect root children separately.
                // We'll use a sentinel by abusing `children[n-1]` — but that's messy.
                // Instead, handle root children via a separate vec below.
                stack.push(i);
                continue;
            }
        };
        if let Some(&prev_sib) = siblings.last() {
            prev[i] = Some(prev_sib);
            next[prev_sib] = Some(i);
        }
        siblings.push(i);
        stack.push(i);
    }

    // Re-run to collect root-level items properly (items with parent = None).
    let root_children: Vec<usize> = (0..n).filter(|&i| parent[i].is_none()).collect();
    // Re-link root children's prev/next (the loop above skipped them).
    for (pos, &i) in root_children.iter().enumerate() {
        if pos > 0 {
            let p = root_children[pos - 1];
            prev[i] = Some(p);
            next[p] = Some(i);
        }
    }

    // ── Create lopdf objects ──────────────────────────────────────────────────

    for (i, heading) in headings.iter().enumerate() {
        // PDF page numbers are 1-based.
        let page_num = (heading.page_idx + 1) as u32;
        let page_ref = pages.get(&page_num).copied().unwrap_or_else(|| {
            // Fall back to the last page if somehow out of range.
            *pages.iter().next_back().unwrap().1
        });

        // Destination: [page_ref /XYZ 0 y 0]
        // Y is bottom-relative in PDF points.
        let y_pt = (PG_H - heading.y_top_mm) * MM_TO_PT;
        let dest = LObj::Array(vec![
            LObj::Reference(page_ref),
            LObj::Name(b"XYZ".to_vec()),
            LObj::Integer(0),
            LObj::Real(y_pt),
            LObj::Integer(0),
        ]);

        let par_ref = match parent[i] {
            Some(p) => LObj::Reference(item_ids[p]),
            None => LObj::Reference(root_id),
        };

        let mut dict = lopdf::Dictionary::new();
        dict.set("Title", LObj::string_literal(heading.title.as_bytes().to_vec()));
        dict.set("Parent", par_ref);
        dict.set("Dest", dest);
        // Positive count = children are open/visible in the panel.
        dict.set("Count", LObj::Integer(children[i].len() as i64));

        if let Some(p) = prev[i] {
            dict.set("Prev", LObj::Reference(item_ids[p]));
        }
        if let Some(nx) = next[i] {
            dict.set("Next", LObj::Reference(item_ids[nx]));
        }
        if let Some(&first) = children[i].first() {
            dict.set("First", LObj::Reference(item_ids[first]));
        }
        if let Some(&last) = children[i].last() {
            dict.set("Last", LObj::Reference(item_ids[last]));
        }

        doc.objects.insert(item_ids[i], LObj::Dictionary(dict));
    }

    // ── Root outlines dictionary ──────────────────────────────────────────────

    let mut root_dict = lopdf::Dictionary::new();
    root_dict.set("Type", LObj::Name(b"Outlines".to_vec()));
    root_dict.set("Count", LObj::Integer(root_children.len() as i64));
    if let Some(&first) = root_children.first() {
        root_dict.set("First", LObj::Reference(item_ids[first]));
    }
    if let Some(&last) = root_children.last() {
        root_dict.set("Last", LObj::Reference(item_ids[last]));
    }
    doc.objects.insert(root_id, LObj::Dictionary(root_dict));

    // ── Patch the document catalog ────────────────────────────────────────────

    let catalog_id = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .expect("PDF has no /Root catalog");

    if let Some(LObj::Dictionary(catalog)) = doc.objects.get_mut(&catalog_id) {
        catalog.set("Outlines", LObj::Reference(root_id));
        // Ask viewers to show the bookmarks panel on open.
        catalog.set("PageMode", LObj::Name(b"UseOutlines".to_vec()));
    }

    // ── Serialise ────────────────────────────────────────────────────────────

    let mut out = Vec::new();
    doc.save_to(&mut out).expect("lopdf serialization failed");
    out
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    let src = std::fs::read_to_string(&cli.input).unwrap_or_else(|e| {
        eprintln!("Error reading {:?}: {}", cli.input, e);
        std::process::exit(1);
    });

    let output = cli.output.unwrap_or_else(|| cli.input.with_extension("pdf"));

    let title = cli
        .input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document");

    let blocks = parse_markdown(&src);
    let renderer = Renderer::new(title, &cli.sans, &cli.serif, &cli.mono, cli.margin);
    let pdf_bytes = renderer.render(&blocks);

    let mut file = File::create(&output).unwrap_or_else(|e| {
        eprintln!("Error creating {:?}: {}", output, e);
        std::process::exit(1);
    });
    file.write_all(&pdf_bytes).unwrap_or_else(|e| {
        eprintln!("Error writing {:?}: {}", output, e);
        std::process::exit(1);
    });

    println!("Wrote {}", output.display());
}
