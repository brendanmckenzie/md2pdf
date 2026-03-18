# md2pdf

A command-line tool to convert Markdown files to PDF.

## Features

- Full Markdown support (headings, paragraphs, code blocks, tables, lists, blockquotes, rules)
- Styled output with customizable fonts
- PDF outline/bookmark generation from headings
- A4 page format with configurable margins

## Installation

```bash
cargo install --git https://github.com/brendanmckenzie/md2pdf
```

Or clone and build locally:

```bash
git clone https://github.com/brendanmckenzie/md2pdf.git
cd md2pdf
cargo install --path .
```

## Usage

```bash
md2pdf input.md                    # output to input.pdf
md2pdf input.md -o output.pdf     # specify output file
md2pdf input.md --margin 30       # set page margin in mm (default: 20)
md2pdf input.md --sans helvetica   # font for body (default: helvetica)
md2pdf input.md --serif times     # font for blockquotes (default: times)
md2pdf input.md --mono courier    # font for code (default: courier)
```

### Font Options

Available fonts: `helvetica`, `times`, `courier`

- `--sans` — Font for body text and headings
- `--serif` — Font for blockquotes  
- `--mono` — Font for code blocks

## Example

```bash
md2pdf README.md --margin 25 --sans times
```

## License

MIT
