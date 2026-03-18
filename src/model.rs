use pulldown_cmark::Alignment;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Block {
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
pub enum Span {
    Text(String),
    Bold(Vec<Span>),
    Italic(Vec<Span>),
    Code(String),
    /// url is None for anchor links (#…) which are meaningless in PDF.
    Link { label: Vec<Span>, url: Option<String> },
    SoftBreak,
    HardBreak,
}
