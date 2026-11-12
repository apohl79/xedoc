//! File citation directives rendered through the browser's existing Markdown links.

use std::borrow::Cow;
use std::sync::LazyLock;

use pulldown_cmark::Event;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use regex_lite::Regex;

pub(crate) fn normalize_file_citations(source: &str) -> Cow<'_, str> {
    static CITATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#":codex-file-citation\{path="([^"\r\n]+)"(?:[ \t]+purpose="[^"\r\n]*")?[ \t]*\}"#,
        )
        .expect("valid file citation pattern")
    });
    if !source.contains(":codex-file-citation{") {
        return Cow::Borrowed(source);
    }
    let protected: Vec<_> = Parser::new(source)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(
                event,
                Event::Code(_)
                    | Event::Html(_)
                    | Event::InlineHtml(_)
                    | Event::Start(Tag::CodeBlock(_) | Tag::Link { .. } | Tag::Image { .. })
            )
            .then_some(range)
        })
        .collect();
    let mut normalized = String::with_capacity(source.len());
    let mut offset = 0;
    for captures in CITATION.captures_iter(source) {
        let citation = captures.get(0).expect("citation match");
        if protected
            .iter()
            .any(|range| range.start < citation.end() && citation.start() < range.end)
        {
            continue;
        }
        let path = &captures[1];
        let filename = path.rsplit(['/', '\\']).next().unwrap_or(path);
        normalized.push_str(&source[offset..citation.start()]);
        normalized.push('[');
        for character in filename.chars() {
            if character.is_ascii_punctuation() {
                normalized.push('\\');
            }
            normalized.push(character);
        }
        normalized.push_str("](<");
        for character in path.chars() {
            match character {
                '&' => normalized.push_str("&amp;"),
                '<' => normalized.push_str("%3C"),
                '>' => normalized.push_str("%3E"),
                '\\' => normalized.push_str("\\\\"),
                character => normalized.push(character),
            }
        }
        normalized.push_str(">)");
        offset = citation.end();
    }
    normalized.push_str(&source[offset..]);
    Cow::Owned(normalized)
}
