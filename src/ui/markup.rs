//! Safe conversion of Telegram's character-offset spans to Pango markup.
//!
//! Message text and span attributes are untrusted.  This module escapes both
//! before adding the small, explicit tag set understood by `gtk::Label`.

use std::collections::HashSet;

use crate::tg::{Span, SpanKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMarkup {
    pub markup: String,
    pub has_spoiler: bool,
    pub has_blockquote: bool,
}

#[derive(Clone)]
struct SafeSpan<'a> {
    span: &'a Span,
    ordinal: usize,
    anchor: bool,
}

/// Only URI schemes that Telegram messages are allowed to launch.
///
/// The private `omg-*` targets are deliberately absent: those are renderer
/// capabilities, not URL schemes supplied by a message.
pub fn is_allowed_link(target: &str) -> bool {
    target
        .split_once(':')
        .map(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http")
                || scheme.eq_ignore_ascii_case("https")
                || scheme.eq_ignore_ascii_case("tg")
        })
        .unwrap_or(false)
}

/// Convert `text` plus Telegram character-offset spans to Pango markup.
pub fn render(text: &str, spans: &[Span]) -> RenderedMarkup {
    render_with_revealed(text, spans, &HashSet::new())
}

/// Render with selected spoiler ranges revealed.  The range key is the
/// original half-open character offset pair from the backend.
pub fn render_with_revealed(
    text: &str,
    spans: &[Span],
    revealed: &HashSet<(usize, usize)>,
) -> RenderedMarkup {
    render_impl(text, spans, revealed, None)
}

/// Render inline code on the current theme's darker background. The value is
/// supplied by `MessagesView` from `theme::load_colors`; Pango cannot resolve
/// GTK CSS variables inside markup attributes.
pub fn render_with_code_background(
    text: &str,
    spans: &[Span],
    revealed: &HashSet<(usize, usize)>,
    darker_background: &str,
) -> RenderedMarkup {
    render_impl(text, spans, revealed, Some(darker_background))
}

fn render_impl(
    text: &str,
    spans: &[Span],
    revealed: &HashSet<(usize, usize)>,
    code_background: Option<&str>,
) -> RenderedMarkup {
    let chars: Vec<char> = text.chars().collect();
    let char_count = chars.len();
    let mut safe = spans
        .iter()
        .enumerate()
        .filter(|(_, span)| span.start < span.end && span.end <= char_count)
        .map(|(ordinal, span)| SafeSpan {
            span,
            ordinal,
            anchor: false,
        })
        .collect::<Vec<_>>();
    safe.sort_by_key(|item| {
        (
            item.span.start,
            std::cmp::Reverse(item.span.end),
            item.ordinal,
        )
    });

    // Pango does not define nested links. Give the first anchor in document
    // order ownership of an overlapping range and render later anchors as
    // ordinary spans. Spoilers are never anchors, so spoiler+link overlap is
    // well-defined too.
    let mut anchor_ranges = Vec::new();
    for item in &mut safe {
        let can_anchor = match &item.span.kind {
            SpanKind::Mention(_) => true,
            SpanKind::Link(target) => is_allowed_link(target),
            _ => false,
        };
        if can_anchor
            && !anchor_ranges
                .iter()
                .any(|(start, end)| item.span.start < *end && *start < item.span.end)
        {
            item.anchor = true;
            anchor_ranges.push((item.span.start, item.span.end));
        }
    }

    let has_spoiler = safe.iter().any(|item| {
        matches!(item.span.kind, SpanKind::Spoiler)
            && !revealed.contains(&(item.span.start, item.span.end))
    });
    let has_blockquote = safe
        .iter()
        .any(|item| matches!(item.span.kind, SpanKind::Blockquote));

    let mut out = String::new();
    let mut active: Vec<SafeSpan<'_>> = Vec::new();
    for offset in 0..char_count {
        let desired = safe
            .iter()
            .filter(|item| item.span.start <= offset && offset < item.span.end)
            .cloned()
            .collect::<Vec<_>>();
        let common = active
            .iter()
            .zip(&desired)
            .take_while(|(left, right)| left.ordinal == right.ordinal)
            .count();
        for item in active[common..].iter().rev() {
            out.push_str(&close_tag(item, revealed, code_background));
        }
        for item in &desired[common..] {
            out.push_str(&open_tag(item, revealed, code_background));
        }
        active = desired;
        escape_char(chars[offset], &mut out);
    }
    for item in active.iter().rev() {
        out.push_str(&close_tag(item, revealed, code_background));
    }

    RenderedMarkup {
        markup: out,
        has_spoiler,
        has_blockquote,
    }
}

fn open_tag(
    item: &SafeSpan<'_>,
    revealed: &HashSet<(usize, usize)>,
    code_background: Option<&str>,
) -> String {
    let span = item.span;
    match &span.kind {
        SpanKind::Bold => "<b>".into(),
        SpanKind::Italic => "<i>".into(),
        SpanKind::Underline => "<u>".into(),
        SpanKind::Strike => "<s>".into(),
        SpanKind::Code => code_background.map_or_else(
            || "<tt>".into(),
            |background| {
                format!(
                    "<span background=\"{}\"><tt>",
                    escape_attribute(background)
                )
            },
        ),
        SpanKind::Pre(_) => "<tt>".into(),
        SpanKind::Link(target) if item.anchor => {
            format!("<a href=\"{}\">", escape_attribute(target))
        }
        SpanKind::Mention(user_id) if item.anchor => format!("<a href=\"omg-user:{user_id}\">"),
        SpanKind::Link(_) | SpanKind::Mention(_) => "<span>".into(),
        SpanKind::Spoiler if revealed.contains(&(span.start, span.end)) => "<span>".into(),
        SpanKind::Spoiler => "<span alpha=\"1%\">".into(),
        SpanKind::Blockquote => "<span>".into(),
    }
}

fn close_tag(
    item: &SafeSpan<'_>,
    revealed: &HashSet<(usize, usize)>,
    code_background: Option<&str>,
) -> String {
    let span = item.span;
    match &span.kind {
        SpanKind::Bold => "</b>".into(),
        SpanKind::Italic => "</i>".into(),
        SpanKind::Underline => "</u>".into(),
        SpanKind::Strike => "</s>".into(),
        SpanKind::Code if code_background.is_some() => "</tt></span>".into(),
        SpanKind::Code | SpanKind::Pre(_) => "</tt>".into(),
        SpanKind::Link(_) | SpanKind::Mention(_) if item.anchor => "</a>".into(),
        SpanKind::Link(_) | SpanKind::Mention(_) => "</span>".into(),
        SpanKind::Spoiler if revealed.contains(&(span.start, span.end)) => "</span>".into(),
        SpanKind::Spoiler => "</span>".into(),
        SpanKind::Blockquote => "</span>".into(),
    }
}

fn escape_attribute(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        escape_char(ch, &mut escaped);
    }
    escaped
}

fn escape_char(ch: char, out: &mut String) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '\'' => out.push_str("&apos;"),
        '"' => out.push_str("&quot;"),
        _ => out.push(ch),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::tg::{Span, SpanKind};

    use super::{is_allowed_link, render, render_with_revealed};

    #[test]
    fn escapes_message_text_before_adding_markup() {
        let rendered = render("<b>& unsafe", &[]);
        assert_eq!(rendered.markup, "&lt;b&gt;&amp; unsafe");
    }

    #[test]
    fn overlapping_spans_are_well_nested_in_document_order() {
        let spans = vec![
            Span {
                start: 0,
                end: 4,
                kind: SpanKind::Bold,
            },
            Span {
                start: 2,
                end: 6,
                kind: SpanKind::Italic,
            },
        ];
        let rendered = render("abcdef", &spans);
        assert_eq!(rendered.markup, "<b>ab<i>cd</i></b><i>ef</i>");
        let parsed = gtk4::pango::parse_markup(&rendered.markup, '\0');
        assert!(parsed.is_ok(), "{}: {:?}", rendered.markup, parsed.err());
    }

    #[test]
    fn out_of_range_and_empty_spans_are_discarded() {
        let spans = vec![
            Span {
                start: 0,
                end: 99,
                kind: SpanKind::Bold,
            },
            Span {
                start: 2,
                end: 2,
                kind: SpanKind::Italic,
            },
        ];
        assert_eq!(render("éx", &spans).markup, "éx");
    }

    #[test]
    fn links_and_attribute_values_are_explicit_and_escaped() {
        let spans = vec![Span {
            start: 0,
            end: 4,
            kind: SpanKind::Link("https://example.test/?a=1&b=\"two\"".into()),
        }];
        let rendered = render("link", &spans);
        assert_eq!(
            rendered.markup,
            "<a href=\"https://example.test/?a=1&amp;b=&quot;two&quot;\">link</a>"
        );
        assert!(rendered.markup.contains("<a href="));
    }

    #[test]
    fn spoiler_uses_a_pango_valid_span_and_can_be_revealed() {
        let spans = vec![Span {
            start: 0,
            end: 6,
            kind: SpanKind::Spoiler,
        }];
        let hidden = render("secret", &spans);
        assert!(hidden.has_spoiler);
        assert!(!hidden.markup.contains("<a"));
        assert!(hidden.markup.contains("alpha=\"1%\""));
        assert!(gtk4::pango::parse_markup(&hidden.markup, '\0').is_ok());
        let revealed = render_with_revealed("secret", &spans, &HashSet::from([(0, 6)]));
        assert_eq!(revealed.markup, "<span>secret</span>");
        assert!(!revealed.has_spoiler);
    }

    #[test]
    fn unsafe_and_internal_link_schemes_are_plain_text() {
        for target in [
            "omg-user:123",
            "omg-spoiler:0:4",
            "javascript:alert(1)",
            "file:///tmp/x",
        ] {
            let rendered = render(
                "link",
                &[Span {
                    start: 0,
                    end: 4,
                    kind: SpanKind::Link(target.into()),
                }],
            );
            assert_eq!(rendered.markup, "<span>link</span>");
        }
        for target in [
            "http://example.test",
            "HTTPS://example.test",
            "tg://resolve?domain=test",
        ] {
            assert!(is_allowed_link(target));
        }
        let mention = render(
            "name",
            &[Span {
                start: 0,
                end: 4,
                kind: SpanKind::Mention(123),
            }],
        );
        assert_eq!(mention.markup, "<a href=\"omg-user:123\">name</a>");
    }

    #[test]
    fn spoiler_link_overlap_never_nests_anchors() {
        let rendered = render(
            "secret",
            &[
                Span {
                    start: 0,
                    end: 6,
                    kind: SpanKind::Link("https://example.test".into()),
                },
                Span {
                    start: 1,
                    end: 5,
                    kind: SpanKind::Spoiler,
                },
            ],
        );
        assert_eq!(rendered.markup.matches("<a ").count(), 1);
        assert_eq!(rendered.markup.matches("</a>").count(), 1);
        assert!(!rendered.markup.contains("<a href=\"omg-spoiler:"));
    }
}
