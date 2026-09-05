//! Disposable, account-scoped recent history. All disk/JSON work runs off the
//! UI and backend executor threads. Telegram remains authoritative.
use std::{fs, path::{Path, PathBuf}, time::SystemTime, sync::{Arc, Mutex}, collections::HashMap};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use super::{Msg, Poll, msg_in_chat, split_topic_chat_id};

const MAX_PAGES: usize = 24;
const MAX_MESSAGES: usize = 50;
const MAX_PAGE_BYTES: u64 = 512 * 1024;

#[derive(Serialize, Deserialize)]
struct Page { version: u8, account: i64, chat: i64, messages: Vec<Msg> }

enum Op {
    Get(i64, oneshot::Sender<Vec<Msg>>),
    Put(i64, Vec<Msg>, u64),
    Update(Box<Msg>),
    Poll(i64, Box<Poll>),
    Delete(i64, Vec<i32>),
    Clear(i64),
    Flush(oneshot::Sender<()>),
}

#[derive(Default)]
struct Revisions { next: u64, chats: HashMap<i64, u64> }

fn parent(chat: i64) -> i64 { split_topic_chat_id(chat).map(|t| t.0).unwrap_or(chat) }

#[derive(Clone)]
pub(super) struct HistoryCache { tx: mpsc::UnboundedSender<Op>, revisions: Arc<Mutex<Revisions>> }

impl HistoryCache {
    pub fn new(account: i64) -> Self {
        let dir = dirs::cache_dir().expect("XDG cache directory unavailable")
            .join(format!("omarchygram/accounts/{account}/history"));
        Self::at(account, dir)
    }

    fn at(account: i64, dir: PathBuf) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let revisions = Arc::new(Mutex::new(Revisions::default()));
        let current = revisions.clone();
        tokio::spawn(async move {
            while let Some(op) = rx.recv().await {
                let dir = dir.clone();
                let current = current.clone();
                // One ordered worker prevents old writes overtaking edits,
                // deletions or a subsequent read. No message text is logged.
                let _ = tokio::task::spawn_blocking(move || match op {
                    Op::Get(chat, reply) => {
                        let path = page_path(&dir, chat);
                        let messages = read(&path, account, chat).map(|p| p.messages).unwrap_or_default();
                        if let Ok(file) = fs::File::open(path) { let _ = file.set_modified(SystemTime::now()); }
                        let _ = reply.send(messages);
                    }
                    Op::Put(chat, messages, revision) => {
                        if current.lock().unwrap().chats.get(&parent(chat)) == Some(&revision) { let _ = write(&dir, account, chat, messages); }
                    }
                    Op::Update(message) => {
                        for (chat, path) in pages_for(&dir, message.chat_id) {
                            if !msg_in_chat(&message, chat) { continue; }
                            let Some(mut page) = read(&path, account, chat) else { continue; };
                            if let Some(old) = page.messages.iter_mut().find(|m| m.id == message.id) {
                                *old = (*message).clone();
                            } else if page.messages.last().is_none_or(|m| message.id > m.id) {
                                page.messages.push((*message).clone());
                            }
                            let _ = write(&dir, account, chat, page.messages);
                        }
                    }
                    Op::Poll(id, poll) => {
                        for (chat, path, _) in files(&dir) {
                            let Some(mut page) = read(&path, account, chat) else { continue; };
                            let mut changed = false;
                            for message in &mut page.messages {
                                if message.poll.as_ref().is_some_and(|p| p.id == id) {
                                    message.poll = Some((*poll).clone());
                                    changed = true;
                                }
                            }
                            if changed { let _ = write(&dir, account, chat, page.messages); }
                        }
                    }
                    Op::Delete(parent, ids) => {
                        for (chat, path) in pages_for(&dir, parent) {
                            let Some(mut page) = read(&path, account, chat) else { continue; };
                            page.messages.retain(|m| !ids.contains(&m.id));
                            let _ = write(&dir, account, chat, page.messages);
                        }
                    }
                    Op::Clear(chat) => {
                        if split_topic_chat_id(chat).is_some() { let _ = fs::remove_file(page_path(&dir, chat)); }
                        else { for (_, path) in pages_for(&dir, chat) { let _ = fs::remove_file(path); } }
                    }
                    Op::Flush(reply) => { let _ = reply.send(()); }
                }).await;
            }
        });
        Self { tx, revisions }
    }

    pub async fn get(&self, chat: i64) -> Vec<Msg> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Op::Get(chat, tx));
        rx.await.unwrap_or_default()
    }
    /// Only newer loads or changes in this dialog invalidate its snapshot.
    /// A busy unrelated chat must not prevent recently opened chats caching.
    pub fn revision(&self, chat: i64) -> u64 {
        let mut state = self.revisions.lock().unwrap();
        state.next = state.next.wrapping_add(1);
        let next = state.next;
        let parent = parent(chat);
        if state.chats.len() >= 128 && !state.chats.contains_key(&parent)
            && let Some(old) = state.chats.iter().min_by_key(|(_, version)| **version).map(|(id, _)| *id) {
            state.chats.remove(&old);
        }
        state.chats.insert(parent, next);
        next
    }
    fn invalidate(&self, chat: Option<i64>) {
        let mut state = self.revisions.lock().unwrap();
        state.next = state.next.wrapping_add(1);
        let next = state.next;
        if let Some(chat) = chat {
            if let Some(version) = state.chats.get_mut(&parent(chat)) { *version = next; }
        } else { for version in state.chats.values_mut() { *version = next; } }
    }
    pub fn put(&self, chat: i64, messages: Vec<Msg>, revision: u64) { let _ = self.tx.send(Op::Put(chat, messages, revision)); }
    pub fn update(&self, message: &Msg) {
        self.invalidate(Some(message.chat_id));
        let _ = self.tx.send(Op::Update(Box::new(message.clone())));
    }
    pub fn poll(&self, id: i64, poll: &Poll) {
        self.invalidate(None);
        let _ = self.tx.send(Op::Poll(id, Box::new(poll.clone())));
    }
    pub fn delete(&self, chat: i64, ids: &[i32]) {
        self.invalidate(Some(chat));
        let _ = self.tx.send(Op::Delete(chat, ids.to_vec()));
    }
    pub fn clear(&self, chat: i64) {
        self.invalidate(Some(chat));
        let _ = self.tx.send(Op::Clear(chat));
    }
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Op::Flush(tx));
        let _ = rx.await;
    }
}

fn page_path(dir: &Path, chat: i64) -> PathBuf {
    let (parent, topic) = split_topic_chat_id(chat).unwrap_or((chat, 0));
    dir.join(format!("p{parent}_t{topic}.json"))
}

fn files(dir: &Path) -> Vec<(i64, PathBuf, fs::Metadata)> {
    fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|entry| {
        let path = entry.path();
        let name = path.file_name()?.to_str()?;
        let (parent, topic) = name.strip_prefix('p')?.strip_suffix(".json")?.split_once("_t")?;
        let parent: i64 = parent.parse().ok()?;
        let topic: i32 = topic.parse().ok()?;
        if parent <= super::TOPIC_CHAT_ID_BASE || !(0..(1 << 28)).contains(&topic)
            || (topic > 0 && !(-1_000_000_000_000 - (1_i64 << 34) + 1..-1_000_000_000_000).contains(&parent)) { return None; }
        let chat = if topic == 0 { parent } else { super::topic_chat_id(parent, topic) };
        if page_path(dir, chat) != path { return None; }
        let metadata = fs::symlink_metadata(&path).ok()?;
        metadata.is_file().then_some((chat, path, metadata))
    }).collect()
}

fn pages_for(dir: &Path, parent: i64) -> Vec<(i64, PathBuf)> {
    files(dir).into_iter().filter(|(chat, _, _)| {
        split_topic_chat_id(*chat).map(|t| t.0).unwrap_or(*chat) == parent
    }).map(|(chat, path, _)| (chat, path)).collect()
}

fn read(path: &Path, account: i64, chat: i64) -> Option<Page> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PAGE_BYTES { return None; }
    let page: Page = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (page.version == 1 && page.account == account && page.chat == chat
        && page.messages.len() <= MAX_MESSAGES
        && page.messages.iter().all(|m| m.id > 0 && msg_in_chat(m, chat))
        && page.messages.windows(2).all(|pair| pair[0].id < pair[1].id)).then_some(page)
}

fn write(dir: &Path, account: i64, chat: i64, mut messages: Vec<Msg>) -> std::io::Result<()> {
    messages.retain(|m| m.id > 0 && !m.deleted && !m.scheduled && msg_in_chat(m, chat));
    messages.sort_by_key(|m| m.id);
    messages.dedup_by_key(|m| m.id);
    if messages.len() > MAX_MESSAGES { messages.drain(..messages.len() - MAX_MESSAGES); }
    let bytes = serde_json::to_vec(&Page { version: 1, account, chat, messages }).map_err(std::io::Error::other)?;
    let path = page_path(dir, chat);
    if bytes.len() as u64 > MAX_PAGE_BYTES {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    crate::storage::atomic_write(&path, &bytes)?;
    let mut entries = files(dir);
    entries.sort_by_key(|(_, _, meta)| meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
    let excess = entries.len().saturating_sub(MAX_PAGES);
    for (_, path, _) in entries.into_iter().take(excess) { fs::remove_file(path)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn temporary() -> PathBuf {
        std::env::temp_dir().join(format!("omg-history-{}-{}", std::process::id(), SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()))
    }
    fn message(chat: i64, id: i32) -> Msg { Msg { chat_id: chat, id, text: format!("message {id}"), ..Msg::default() } }

    #[tokio::test]
    async fn cached_history_preserves_rich_messages_and_account_scope() {
        let dir = temporary();
        let cache = HistoryCache::at(10, dir.clone());
        let mut m = message(20, 1);
        m.spans = vec![super::super::Span { start: 0, end: 7, kind: super::super::SpanKind::Bold }];
        m.keyboard = Some(super::super::Keyboard { rows: vec![vec![super::super::KeyButton { text: "button".into(), kind: super::super::ButtonKind::Callback(vec![0, 1, 255]) }]] });
        cache.put(20, vec![m.clone()], cache.revision(20));
        assert_eq!(cache.get(20).await, vec![m.clone()]);
        assert_eq!(fs::metadata(page_path(&dir, 20)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(HistoryCache::at(11, dir.clone()).get(20).await.is_empty());
        assert_eq!(HistoryCache::at(10, dir.clone()).get(20).await, vec![m]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn updates_deletes_and_clear_keep_topics_separate() {
        let dir = temporary();
        let cache = HistoryCache::at(10, dir.clone());
        let forum = -1_000_000_000_123;
        let a = super::super::topic_chat_id(forum, 2);
        let b = super::super::topic_chat_id(forum, 3);
        let mut first = message(forum, 10); first.topic_id = Some(2);
        let mut second = message(forum, 11); second.topic_id = Some(3);
        cache.put(a, vec![first.clone()], cache.revision(a)); cache.flush().await; cache.put(b, vec![second.clone()], cache.revision(b));
        cache.flush().await;
        first.text = "edited".into(); cache.update(&first);
        assert_eq!(cache.get(a).await, vec![first]);
        assert_eq!(cache.get(b).await, vec![second.clone()]);
        cache.clear(a);
        assert!(cache.get(a).await.is_empty());
        assert_eq!(cache.get(b).await, vec![second]);
        cache.delete(forum, &[11]); assert!(cache.get(b).await.is_empty());
        cache.flush().await;
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_history_response_started_before_a_mutation_cannot_undo_it() {
        let dir = temporary();
        let cache = HistoryCache::at(10, dir.clone());
        let original = vec![message(20, 1), message(20, 2)];
        cache.put(20, original.clone(), cache.revision(20));
        cache.flush().await;
        let stale = cache.revision(20);
        cache.delete(20, &[1]);
        let mut edited = message(20, 2); edited.text = "edited while loading".into();
        cache.update(&edited);
        cache.put(20, original, stale);
        assert_eq!(cache.get(20).await, vec![edited]);
        cache.clear(20);
        cache.put(20, vec![message(20, 2)], stale);
        assert!(cache.get(20).await.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn unrelated_updates_do_not_prevent_caching_and_newer_loads_win() {
        let dir = temporary();
        let cache = HistoryCache::at(10, dir.clone());
        let earlier = cache.revision(20);
        cache.update(&message(30, 1));
        cache.put(20, vec![message(20, 1)], earlier);
        assert_eq!(cache.get(20).await.len(), 1);
        let newer = cache.revision(20);
        cache.put(20, vec![message(20, 2)], newer);
        cache.put(20, vec![message(20, 1)], earlier);
        assert_eq!(cache.get(20).await[0].id, 2);
        for chat in 100..400 { cache.revision(chat); }
        assert_eq!(cache.revisions.lock().unwrap().chats.len(), 128);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn cache_bounds_pages_messages_and_rejects_malformed_data() {
        let dir = temporary();
        let cache = HistoryCache::at(10, dir.clone());
        for chat in 1..=30 { cache.put(chat, (1..=70).map(|id| message(chat, id)).collect(), cache.revision(chat)); }
        cache.flush().await;
        assert_eq!(files(&dir).len(), MAX_PAGES);
        let page = cache.get(30).await;
        assert_eq!(page.len(), MAX_MESSAGES);
        assert_eq!(page[0].id, 21);
        fs::write(page_path(&dir, 30), b"incomplete JSON").unwrap();
        assert!(cache.get(30).await.is_empty());
        let mut huge = message(31, 1); huge.text = "x".repeat(MAX_PAGE_BYTES as usize);
        cache.put(31, vec![huge], cache.revision(31));
        assert!(cache.get(31).await.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
