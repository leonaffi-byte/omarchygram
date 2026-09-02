//! Telegram-style message markdown ⇄ (plain text, spans). Orchestrator-owned.
//!
//! Markers (same as Telegram Desktop): `**bold**`, `__italic__`,
//! `~~strike~~`, `` `code` ``, ```` ```lang⏎pre``` ````, `||spoiler||`,
//! `[text](url)`. Unmatched markers stay literal. Offsets in `Span` are char
//! indices into the returned text (half-open).

use super::{Span, SpanKind};

/// Parse markers out of `input`; returns the plain text and its spans.
pub fn parse_markdown(input: &str) -> (String, Vec<Span>) {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut spans = Vec::new();
    parse_into(&chars, &mut out, &mut spans, 0);
    (out, spans)
}

fn starts_with(chars: &[char], at: usize, marker: &str) -> bool {
    let m: Vec<char> = marker.chars().collect();
    at + m.len() <= chars.len() && chars[at..at + m.len()] == m[..]
}

fn find(chars: &[char], from: usize, marker: &str) -> Option<usize> {
    let m: Vec<char> = marker.chars().collect();
    if m.is_empty() || from + m.len() > chars.len() {
        return None;
    }
    (from..=chars.len() - m.len()).find(|&i| chars[i..i + m.len()] == m[..])
}

/// Appends the parsed form of `chars` to `out` (whose current char length
/// is `base`), pushing spans with offsets relative to the whole output.
fn parse_into(chars: &[char], out: &mut String, spans: &mut Vec<Span>, base: usize) {
    let mut i = 0;
    let mut len = base; // char length of `out`
    while i < chars.len() {
        // Code block: ```lang\n...``` — no nesting inside.
        if starts_with(chars, i, "```") {
            if let Some(close) = find(chars, i + 3, "```") {
                let body: String = chars[i + 3..close].iter().collect();
                let (lang, code) = split_lang(&body);
                if !code.is_empty() {
                    let start = len;
                    out.push_str(code);
                    len += code.chars().count();
                    spans.push(Span { start, end: len, kind: SpanKind::Pre(lang.to_string()) });
                    i = close + 3;
                    continue;
                }
            }
        }
        // Inline code — no nesting inside.
        if chars[i] == '`' {
            if let Some(close) = find(chars, i + 1, "`") {
                if close > i + 1 {
                    let start = len;
                    let code: String = chars[i + 1..close].iter().collect();
                    out.push_str(&code);
                    len += close - i - 1;
                    spans.push(Span { start, end: len, kind: SpanKind::Code });
                    i = close + 1;
                    continue;
                }
            }
        }
        // Link: [text](url)
        if chars[i] == '[' {
            if let Some(mid) = find(chars, i + 1, "](") {
                if let Some(close) = find(chars, mid + 2, ")") {
                    let url: String = chars[mid + 2..close].iter().collect();
                    if mid > i + 1 && !url.is_empty() && !url.contains(char::is_whitespace) {
                        let start = len;
                        parse_into(&chars[i + 1..mid], out, spans, len);
                        len = out.chars().count();
                        spans.push(Span { start, end: len, kind: SpanKind::Link(url) });
                        i = close + 1;
                        continue;
                    }
                }
            }
        }
        // Paired markers with nesting.
        let mut matched = false;
        for (marker, kind) in [
            ("**", SpanKind::Bold),
            ("__", SpanKind::Italic),
            ("~~", SpanKind::Strike),
            ("||", SpanKind::Spoiler),
        ] {
            if starts_with(chars, i, marker) {
                if let Some(close) = find(chars, i + 2, marker) {
                    if close > i + 2 {
                        let start = len;
                        parse_into(&chars[i + 2..close], out, spans, len);
                        len = out.chars().count();
                        spans.push(Span { start, end: len, kind });
                        i = close + 2;
                        matched = true;
                    }
                }
                break;
            }
        }
        if matched {
            continue;
        }
        out.push(chars[i]);
        len += 1;
        i += 1;
    }
}

/// "rust\nfn x" → ("rust", "fn x"); "fn x" → ("", "fn x").
fn split_lang(body: &str) -> (&str, &str) {
    let body = body.strip_prefix('\n').unwrap_or(body);
    if let Some((first, rest)) = body.split_once('\n') {
        let first = first.trim();
        if !first.is_empty()
            && first.len() <= 20
            && first.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '#' || c == '-')
        {
            return (first, rest.strip_suffix('\n').unwrap_or(rest));
        }
    }
    ("", body.strip_suffix('\n').unwrap_or(body))
}

/// Re-insert markers so the text can be edited in the composer.
/// Underline, mentions and blockquotes have no marker; they become plain.
pub fn to_markdown(text: &str, spans: &[Span]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut opens: Vec<Vec<&Span>> = vec![Vec::new(); n + 1];
    let mut closes: Vec<Vec<&Span>> = vec![Vec::new(); n + 1];
    for s in spans.iter().filter(|s| s.start < s.end && s.end <= n) {
        if marker_for(&s.kind).is_some() {
            opens[s.start].push(s);
            closes[s.end].push(s);
        }
    }
    // Longer spans open first and close last, so nesting stays well-formed.
    for v in opens.iter_mut() {
        v.sort_by_key(|s| std::cmp::Reverse(s.end));
    }
    for v in closes.iter_mut() {
        v.sort_by_key(|s| std::cmp::Reverse(s.start));
    }
    let mut out = String::new();
    for i in 0..=n {
        for s in &closes[i] {
            match &s.kind {
                SpanKind::Pre(_) => out.push_str("```"),
                SpanKind::Link(url) => {
                    out.push_str("](");
                    out.push_str(url);
                    out.push(')');
                }
                k => out.push_str(marker_for(k).unwrap_or("")),
            }
        }
        for s in &opens[i] {
            match &s.kind {
                SpanKind::Pre(lang) => {
                    out.push_str("```");
                    out.push_str(lang);
                    out.push('\n');
                }
                SpanKind::Link(_) => out.push('['),
                k => out.push_str(marker_for(k).unwrap_or("")),
            }
        }
        if i < n {
            out.push(chars[i]);
        }
    }
    out
}

fn marker_for(kind: &SpanKind) -> Option<&'static str> {
    Some(match kind {
        SpanKind::Bold => "**",
        SpanKind::Italic => "__",
        SpanKind::Strike => "~~",
        SpanKind::Spoiler => "||",
        SpanKind::Code => "`",
        SpanKind::Pre(_) => "```",
        SpanKind::Link(_) => "[",
        SpanKind::Underline | SpanKind::Mention(_) | SpanKind::Blockquote => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_untouched() {
        let (t, s) = parse_markdown("2 * 3 = 6, a_b, ~x");
        assert_eq!(t, "2 * 3 = 6, a_b, ~x");
        assert!(s.is_empty());
    }

    #[test]
    fn bold_code_link() {
        let (t, s) = parse_markdown("**bold** and `code` and [site](https://x.y)");
        assert_eq!(t, "bold and code and site");
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], Span { start: 0, end: 4, kind: SpanKind::Bold });
        assert_eq!(s[1], Span { start: 9, end: 13, kind: SpanKind::Code });
        assert_eq!(s[2], Span { start: 18, end: 22, kind: SpanKind::Link("https://x.y".into()) });
    }

    #[test]
    fn nesting_and_unicode() {
        let (t, s) = parse_markdown("__итал **жир** ик__");
        assert_eq!(t, "итал жир ик");
        assert!(s.iter().any(|x| x.kind == SpanKind::Bold && x.start == 5 && x.end == 8));
        assert!(s.iter().any(|x| x.kind == SpanKind::Italic && x.start == 0 && x.end == 11));
    }

    #[test]
    fn pre_with_lang_and_unmatched() {
        let (t, s) = parse_markdown("```rust\nfn x() {}\n```\n**open");
        assert_eq!(t, "fn x() {}\n**open");
        assert_eq!(s, vec![Span { start: 0, end: 9, kind: SpanKind::Pre("rust".into()) }]);
    }

    #[test]
    fn roundtrip() {
        let src = "a **b __c__** `d` [e](https://f) ||g||";
        let (t, s) = parse_markdown(src);
        let back = to_markdown(&t, &s);
        let (t2, s2) = parse_markdown(&back);
        assert_eq!(t, t2);
        assert_eq!(s.len(), s2.len());
    }
}
