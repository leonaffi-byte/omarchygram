use std::collections::{HashMap, HashSet};

use chrono::Local;

use crate::tg::Msg;

pub const ASSISTANT_CHAT: i64 = i64::MIN + 1;
pub const OMARCHY_CHAT: i64 = i64::MIN + 2;

pub struct VirtualStore {
    pub msgs: Vec<Msg>,
    pub next_id: i32,
    pub in_flight: u32,
    pub mono_ids: HashSet<i32>,
}

impl Default for VirtualStore {
    fn default() -> Self {
        Self {
            msgs: Vec::new(),
            next_id: i32::MIN + 1,
            in_flight: 0,
            mono_ids: HashSet::new(),
        }
    }
}

impl VirtualStore {
    pub fn append(&mut self, chat_id: i64, text: String, outgoing: bool, monospace: bool) -> Msg {
        let id = self.next_id;
        self.next_id += 1;
        let title = virtual_title(chat_id);
        let message = Msg {
            id,
            chat_id,
            chat_title: title.to_string(),
            sender: if outgoing { "You" } else { title }.to_string(),
            sender_id: None,
            text,
            ts: Local::now(),
            outgoing,
            media: None,
            doc_name: None,
            reply_to: None,
            reactions: Vec::new(),
            edited: false,
            deleted: false,
            ..Msg::default()
        };
        if monospace {
            self.mono_ids.insert(id);
        }
        self.msgs.push(message.clone());
        message
    }
}

pub struct AuxState {
    pub transcripts: HashMap<(i64, i32), ReqState<String>>,
    pub translations: HashMap<(i64, i32), ReqState<String>>,
    pub summaries: HashMap<(i64, i32), ReqState<String>>,
    pub draft_token: u64,
}

impl Default for AuxState {
    fn default() -> Self {
        Self {
            transcripts: HashMap::new(),
            translations: HashMap::new(),
            summaries: HashMap::new(),
            draft_token: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReqState<T> {
    InFlight,
    Done(T),
    Failed(String),
}

pub fn is_virtual(chat_id: i64) -> bool {
    matches!(chat_id, ASSISTANT_CHAT | OMARCHY_CHAT)
}

pub fn virtual_title(chat_id: i64) -> &'static str {
    match chat_id {
        ASSISTANT_CHAT => "Assistant",
        OMARCHY_CHAT => "Omarchy",
        _ => "Unknown",
    }
}
