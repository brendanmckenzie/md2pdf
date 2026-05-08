mod model;
mod outline;
mod parser;
mod renderer;

use clap::{Parser as ClapParser, ValueEnum};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use parser::parse_markdown;
use renderer::Renderer;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Clone, ValueEnum)]
pub enum FontFamily {
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

// ── Entry point ───────────────────────────────────────────────────────────────

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
    let base_dir = cli
        .input
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let renderer = Renderer::new(title, &cli.sans, &cli.serif, &cli.mono, cli.margin, base_dir);
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
