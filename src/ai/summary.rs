//! Range validation and bounded prompt construction; never silently truncate.
use crate::tg::{MediaKind, Msg, msg_in_chat};
use std::collections::BTreeMap;

pub const MAX_MESSAGES: usize = 2_000;
const MAX_CHARS: usize = 1_000_000;
pub const CHUNK_CHARS: usize = 12_000;

pub struct Range {
    pub chat_id: i64,
    pub first: i32,
    pub last: i32,
    messages: BTreeMap<i32, Msg>,
    chars: usize,
}

impl Range {
    pub fn new(chat_id: i64, a: i32, b: i32) -> Result<Self, String> {
        if a <= 0 || b <= 0 {
            return Err("Choose two delivered messages.".into());
        }
        Ok(Self {
            chat_id,
            first: a.min(b),
            last: a.max(b),
            messages: BTreeMap::new(),
            chars: 0,
        })
    }

    pub fn add(&mut self, messages: Vec<Msg>) -> Result<(), String> {
        for message in messages {
            if !msg_in_chat(&message, self.chat_id)
                || message.id < self.first
                || message.id > self.last
            {
                continue;
            }
            let chars = message.text.chars().count();
            let old = self.messages.insert(message.id, message);
            self.chars = self
                .chars
                .saturating_sub(old.map(|m| m.text.chars().count()).unwrap_or(0))
                + chars;
            if self.messages.len() > MAX_MESSAGES || self.chars > MAX_CHARS {
                return Err(format!(
                    "This range is too large. Choose a shorter range (up to {MAX_MESSAGES} messages and one million characters). Nothing was summarized."
                ));
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<Msg>, String> {
        if !self.messages.contains_key(&self.first) || !self.messages.contains_key(&self.last) {
            return Err(
                "A selected endpoint is no longer available. Choose the range again.".into(),
            );
        }
        Ok(self.messages.into_values().collect())
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}

pub fn entry(message: &Msg, transcript: Option<&str>) -> Result<String, String> {
    let content = if message.deleted {
        "[deleted message; content unavailable]".to_string()
    } else if message.media == Some(MediaKind::Voice) {
        let text = transcript
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "Voice message #{} has no transcript. Retry to include it.",
                    message.id
                )
            })?;
        format!("{}\n[voice transcript] {text}", message.text)
    } else if message.text.is_empty() {
        format!(
            "[{}; content not transcribed]",
            message
                .media
                .map(|m| format!("{m:?}").to_lowercase())
                .unwrap_or_else(|| "empty message".into())
        )
    } else {
        message.text.clone()
    };
    Ok(format!(
        "#{} · {} · {}{}\n{}\n\n",
        message.id,
        message.ts.format("%Y-%m-%d %H:%M"),
        message.sender,
        if message.outgoing { " (You)" } else { "" },
        content
    ))
}

pub fn chunks(text: &str) -> Result<Vec<String>, String> {
    if text.chars().count() > MAX_CHARS {
        return Err("The messages and transcripts are too long. Choose a shorter range. Nothing was summarized.".into());
    }
    let mut result = Vec::new();
    let mut chunk = String::new();
    for (index, c) in text.chars().enumerate() {
        if index > 0 && index % CHUNK_CHARS == 0 {
            result.push(std::mem::take(&mut chunk));
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        result.push(chunk);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn msg(id: i32) -> Msg {
        Msg {
            id,
            chat_id: 7,
            text: format!("message {id}"),
            ..Msg::default()
        }
    }

    #[test]
    fn inclusive_range_orders_deduplicates_and_excludes_other_chats() {
        let mut range = Range::new(7, 30, 10).unwrap();
        range
            .add(vec![
                msg(30),
                msg(20),
                msg(10),
                msg(9),
                msg(31),
                Msg {
                    chat_id: 8,
                    ..msg(15)
                },
            ])
            .unwrap();
        range.add(vec![msg(20)]).unwrap();
        assert_eq!(
            range
                .finish()
                .unwrap()
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
    }

    #[test]
    fn missing_or_pending_endpoint_cannot_be_summarized() {
        assert!(Range::new(7, -1, 2).is_err());
        let mut range = Range::new(7, 10, 30).unwrap();
        range.add(vec![msg(10), msg(20)]).unwrap();
        assert!(range.finish().is_err());
    }

    #[test]
    fn topic_range_excludes_other_topics_in_the_same_forum() {
        let forum = -1_000_000_000_007;
        let topic = crate::tg::topic_chat_id(forum, 42);
        let mut range = Range::new(topic, 10, 30).unwrap();
        let messages = [10, 20, 30]
            .into_iter()
            .map(|id| Msg {
                chat_id: forum,
                topic_id: Some(if id == 20 { 43 } else { 42 }),
                ..msg(id)
            })
            .collect();
        range.add(messages).unwrap();
        assert_eq!(
            range
                .finish()
                .unwrap()
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            vec![10, 30]
        );
    }

    #[test]
    fn voice_content_is_required_and_deleted_content_is_not_revealed() {
        let voice = Msg {
            media: Some(MediaKind::Voice),
            ..msg(10)
        };
        assert!(entry(&voice, None).is_err());
        assert!(entry(&voice, Some(" ")).is_err());
        let text = entry(&voice, Some("meet at eight")).unwrap();
        assert!(text.contains("[voice transcript] meet at eight"));
        assert!(text.contains("message 10"));
        let deleted = entry(
            &Msg {
                deleted: true,
                ..voice
            },
            None,
        )
        .unwrap();
        assert!(!deleted.contains("message 10"));
    }

    #[test]
    fn chunking_keeps_every_unicode_character() {
        let text = "שלום🙂\n".repeat(5_000);
        let chunks = chunks(&text).unwrap();
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.chars().count() <= CHUNK_CHARS));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn oversize_input_fails_instead_of_silently_truncating() {
        let mut range = Range::new(7, 1, 2001).unwrap();
        assert!(range.add((1..=2001).map(msg).collect()).is_err());
        assert!(chunks(&"a".repeat(MAX_CHARS + 1)).is_err());
    }
}
