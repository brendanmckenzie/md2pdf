use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser as MdParser, Tag, TagEnd};

use crate::model::{Block, Span};

// ── Internal builder types ────────────────────────────────────────────────────

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
        // enclosing Paragraph tag. Auto-create one so content isn't dropped.
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

// ── Public API ────────────────────────────────────────────────────────────────

pub fn parse_markdown(src: &str) -> Vec<Block> {
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
