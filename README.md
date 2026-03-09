# rushdown-footnote
rushdown-footnote is a simple footnote plugin for [rushdown](https://github.com/yuin/rushdown), a markdown parser. It allows you to easily add footnotes to your markdown documents.

## Installation
Add dependency to your `Cargo.toml`:

```toml
[dependencies]
rushdown-footnote = "x.y.z"
```

rushdown-footnote can also be used in `no_std` environments. To enable this feature, add the following line to your `Cargo.toml`:

```toml
rushdown-footnote = { version = "x.y.z", default-features = false, features = ["no-std"] }
```

## Syntax
rushdown-footnote uses the following syntax for footnotes: [PHP Markdown Extra](https://michelf.ca/projects/php-markdown/extra/#footnotes)

```markdown
That's some text with a footnote.[^1]
That's some text with a named footnote.[^named]

[^1]: And that's the footnote.

    That's the second paragraph.

[^named]: And that's a named footnote.
```

## Usage
### Example

```rust
use core::fmt::Write;
use rushdown::{
    new_markdown_to_html,
    parser::{self, ParserExtension},
    renderer::html::{self, RendererExtension},
    Result,
};
use rushdown_footnote::{
    footnote_html_renderer_extension, footnote_parser_extension, FootnoteHtmlRendererOptions,
};

let markdown_to_html = new_markdown_to_html(
    parser::Options::default(),
    html::Options::default(),
    footnote_parser_extension(),
    footnote_html_renderer_extension(FootnoteHtmlRendererOptions::default()),
);
let mut output = String::new();
let input = r#"
That's some text with a footnote.[^1]

[^1]: And that's the footnote.
"#;
match markdown_to_html(&mut output, input) {
    Ok(_) => {
        println!("HTML output:\n{}", output);
    }
    Err(e) => {
        println!("Error: {:?}", e);
    }
}
```

### Options

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `id_prefix`| `FootnoteIdPrefix` | `FootnoteIdPrefix::None` | The prefix for footnote IDs. |
| `link_class` | `String` | `footnote-ref` | The class for footnote links. |
| `backlink_class` | `String` | `footnote-backref` | The class for footnote backlinks. |
| `backlink_html` | `String` | `&#x21a9;&#xfe0e;` | The HTML for footnote backlinks. |

## Donation
BTC: 1NEDSyUmo4SMTDP83JJQSWi1MvQUGGNMZB

Github sponsors also welcome.

## License
MIT

## Author
Yusuke Inuzuka
