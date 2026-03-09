use std::{fs, path::PathBuf};

use rushdown::{new_markdown_to_html, parser, renderer::html, test::MarkdownTestSuite};
use rushdown_footnote::{
    footnote_html_renderer_extension, footnote_parser_extension, FootnoteHtmlRendererOptions,
};

fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn test_footnote() {
    let path = data_path("footnote.txt");
    let s = fs::read_to_string(&path).expect("failed to read footnote.txt");
    let suite = MarkdownTestSuite::with_str(s.as_str()).unwrap();
    let markdown_to_html = new_markdown_to_html(
        parser::Options::default(),
        html::Options {
            allows_unsafe: true,
            xhtml: false,
            ..html::Options::default()
        },
        footnote_parser_extension(),
        footnote_html_renderer_extension(FootnoteHtmlRendererOptions::default()),
    );
    suite.execute(&markdown_to_html)
}
