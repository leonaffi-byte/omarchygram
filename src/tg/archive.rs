//! Local message archive (sqlite via libsql — the same SQLite grammers links,
//! so the binary carries one copy). Orchestrator-owned: user data.
//!
//! Single-writer design: one task owns the connection; `record()` is a
//! fire-and-forget upsert (ordered, never races), reads are awaited.
//! Every message the backend sees is recorded. On an edit the previous text
//! is kept as a version; on a delete update the row is flagged, never
//! removed. Powers anti-delete and edit history (UI-side opt-ins); the archive
//! itself is always on — the user's own data, on their own disk, 0600.

use std::path::PathBuf;

use chrono::{Local, TimeZone};
use libsql::{params, Connection, Value};
use tokio::sync::{mpsc, oneshot};

use super::{MediaKind, Msg, MsgVersion};

#[derive(Clone)]
pub struct Archive {
    tx: mpsc::UnboundedSender<Op>,
}

enum Op {
    ImportLegacy { path: PathBuf, respond: oneshot::Sender<Result<u64, String>> },
    Upsert(Box<Msg>),
    MarkDeleted { chat_id: Option<i64>, ids: Vec<i32>, respond: oneshot::Sender<Vec<(i64, i32)>> },
    DeletedBetween { chat_id: i64, min_id: i32, max_id: i32, respond: oneshot::Sender<Vec<Msg>> },
    Versions { chat_id: i64, msg_id: i32, respond: oneshot::Sender<Vec<MsgVersion>> },
}

fn path(account_id: i64) -> PathBuf {
    dirs::data_dir()
        .expect("cannot determine XDG data dir — is HOME set?")
        .join(format!("omarchygram/accounts/{account_id}/archive.sqlite"))
}

fn media_str(m: Option<MediaKind>) -> Option<&'static str> {
    match m {
        Some(MediaKind::Photo) => Some("photo"),
        Some(MediaKind::Sticker) => Some("sticker"),
        Some(MediaKind::Voice) => Some("voice"),
        Some(MediaKind::Document) => Some("document"),
        Some(MediaKind::Video) => Some("video"),
        Some(MediaKind::Gif) => Some("gif"),
        Some(MediaKind::Audio) => Some("audio"),
        Some(MediaKind::VideoNote) => Some("video_note"),
        Some(MediaKind::Location) => Some("location"),
        Some(MediaKind::Venue) => Some("venue"),
        Some(MediaKind::Contact) => Some("contact"),
        Some(MediaKind::Dice) => Some("dice"),
        Some(MediaKind::Poll) => Some("poll"),
        Some(MediaKind::Unsupported) => Some("unsupported"),
        None => None,
    }
}

fn media_kind(s: &Option<String>) -> Option<MediaKind> {
    match s.as_deref() {
        Some("photo") => Some(MediaKind::Photo),
        Some("sticker") => Some(MediaKind::Sticker),
        Some("video") => Some(MediaKind::Video),
        Some("gif") => Some(MediaKind::Gif),
        Some("audio") => Some(MediaKind::Audio),
        Some("video_note") => Some(MediaKind::VideoNote),
        Some("unsupported") => Some(MediaKind::Unsupported),
        Some("voice") => Some(MediaKind::Voice),
        Some("document") => Some(MediaKind::Document),
        Some("location") => Some(MediaKind::Location),
        Some("venue") => Some(MediaKind::Venue),
        Some("contact") => Some(MediaKind::Contact),
        Some("dice") => Some(MediaKind::Dice),
        Some("poll") => Some(MediaKind::Poll),
        _ => None,
    }
}

/// 0600 on the db and any -wal/-shm/-journal sidecar that exists.
fn restrict(db: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut targets = vec![db.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut os = db.as_os_str().to_owned();
        os.push(suffix);
        targets.push(PathBuf::from(os));
    }
    for t in targets {
        if t.exists() {
            let _ = std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o600));
        }
    }
}

fn opt_text(v: Value) -> Option<String> {
    match v {
        Value::Text(s) => Some(s),
        _ => None,
    }
}

fn opt_int(v: Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(i),
        _ => None,
    }
}

impl Archive {
    /// Opens (creating) the archive and spawns its writer task on the
    /// current tokio runtime.
    pub async fn open(account_id: i64) -> Result<Archive, String> {
        if account_id <= 0 { return Err("archive requires an authenticated account".into()); }
        Self::open_at(path(account_id)).await
    }

    async fn open_at(p: PathBuf) -> Result<Archive, String> {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            use std::os::unix::fs::PermissionsExt;
            // The 0700 dir is the primary barrier around message text.
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("archive dir permissions: {e}"))?;
        }
        let db = libsql::Builder::new_local(&p)
            .build()
            .await
            .map_err(|e| format!("archive open: {e}"))?;
        let conn = db.connect().map_err(|e| format!("archive connect: {e}"))?;
        restrict(&p);
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS archive_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS messages(
               chat_id INTEGER NOT NULL, msg_id INTEGER NOT NULL,
               sender TEXT NOT NULL DEFAULT '', sender_id INTEGER,
               text TEXT NOT NULL DEFAULT '', ts INTEGER NOT NULL,
               outgoing INTEGER NOT NULL DEFAULT 0, media TEXT, doc_name TEXT,
               reply_to INTEGER, edited INTEGER NOT NULL DEFAULT 0,
               deleted INTEGER NOT NULL DEFAULT 0, deleted_at INTEGER, topic_id INTEGER,
               PRIMARY KEY(chat_id, msg_id));
             CREATE INDEX IF NOT EXISTS messages_by_msg ON messages(msg_id);
             CREATE TABLE IF NOT EXISTS versions(
               chat_id INTEGER NOT NULL, msg_id INTEGER NOT NULL, seq INTEGER NOT NULL,
               text TEXT NOT NULL, replaced_at INTEGER NOT NULL,
               PRIMARY KEY(chat_id, msg_id, seq));",
        )
        .await
        .map_err(|e| format!("archive schema: {e}"))?;
        // Older account archives remain readable if this column is absent.
        let mut columns = conn.query("PRAGMA table_info(messages)", ()).await.map_err(|e| e.to_string())?;
        let mut has_topic = false;
        while let Some(row) = columns.next().await.map_err(|e| e.to_string())? {
            has_topic |= row.get::<String>(1).ok().as_deref() == Some("topic_id");
        }
        drop(columns);
        if !has_topic {
            conn.execute("ALTER TABLE messages ADD COLUMN topic_id INTEGER", ()).await.map_err(|e| e.to_string())?;
        }
        restrict(&p); // sidecars may have appeared during schema setup
        let (tx, rx) = mpsc::unbounded_channel();
        let p2 = p.clone();
        tokio::spawn(async move {
            writer(conn, rx, &p2).await;
            restrict(&p2);
        });
        Ok(Archive { tx })
    }

    pub async fn import_legacy(&self) -> Result<u64, String> {
        let path = dirs::data_dir().ok_or("Data folder unavailable")?.join("omarchygram/archive.sqlite");
        let (respond, rx) = oneshot::channel();
        self.tx.send(Op::ImportLegacy { path, respond }).map_err(|_| "Archive is closed")?;
        rx.await.map_err(|_| "Archive is closed")?
    }

    /// Fire-and-forget upsert (keeps message order; never blocks the caller).
    pub fn record(&self, m: Msg) {
        let _ = self.tx.send(Op::Upsert(Box::new(m)));
    }

    /// Flag messages deleted. Channel deletions carry the chat id; for
    /// non-channel deletions Telegram gives only message ids (globally
    /// unique across users/basic groups), so pass None and the archive
    /// resolves the chat. Returns the (chat_id, msg_id) pairs flagged.
    pub async fn mark_deleted(&self, chat_id: Option<i64>, ids: Vec<i32>) -> Vec<(i64, i32)> {
        let (respond, rx) = oneshot::channel();
        let _ = self.tx.send(Op::MarkDeleted { chat_id, ids, respond });
        rx.await.unwrap_or_default()
    }

    /// Archived messages flagged deleted with ids in [min_id, max_id].
    pub async fn deleted_between(&self, chat_id: i64, min_id: i32, max_id: i32) -> Vec<Msg> {
        let (respond, rx) = oneshot::channel();
        let _ = self.tx.send(Op::DeletedBetween { chat_id, min_id, max_id, respond });
        rx.await.unwrap_or_default()
    }

    /// Previous texts, oldest first.
    pub async fn versions(&self, chat_id: i64, msg_id: i32) -> Vec<MsgVersion> {
        let (respond, rx) = oneshot::channel();
        let _ = self.tx.send(Op::Versions { chat_id, msg_id, respond });
        rx.await.unwrap_or_default()
    }
}

async fn writer(conn: Connection, mut rx: mpsc::UnboundedReceiver<Op>, path: &std::path::Path) {
    let mut writes: u64 = 0;
    let mut pending = None;
    loop {
        let Some(op) = (match pending.take() { Some(op) => Some(op), None => rx.recv().await }) else { break };
        match op {
            Op::ImportLegacy { path: source, respond } => {
                let result = import_legacy(&conn, &source).await.map_err(|_| "Could not import the older archive; the original is unchanged".to_string());
                restrict(path);
                let _ = respond.send(result);
            }
            Op::Upsert(m) => {
                // Amortize fsync over a history page without reordering edits
                // or reads/deletions. Never coalesce distinct message versions.
                let mut batch = vec![m];
                while batch.len() < 128 {
                    match rx.try_recv() {
                        Ok(Op::Upsert(m)) => batch.push(m),
                        Ok(other) => { pending = Some(other); break; }
                        Err(_) => break,
                    }
                }
                let result = async {
                    conn.execute("BEGIN IMMEDIATE", ()).await?;
                    for message in &batch { upsert(&conn, message).await?; }
                    conn.execute("COMMIT", ()).await?;
                    Ok::<(), libsql::Error>(())
                }.await;
                match result {
                    Ok(()) => {
                        writes += batch.len() as u64;
                    }
                    Err(e) => {
                        let _ = conn.execute("ROLLBACK", ()).await;
                        eprintln!("omarchygram: archive write failed: {e}");
                    }
                }
                if writes <= 128 || writes % 200 < batch.len() as u64 {
                    restrict(path); // sidecars appear lazily
                }
            }
            Op::MarkDeleted { chat_id, ids, respond } => {
                let _ = respond.send(mark_deleted(&conn, chat_id, &ids).await);
            }
            Op::DeletedBetween { chat_id, min_id, max_id, respond } => {
                let _ = respond.send(deleted_between(&conn, chat_id, min_id, max_id).await);
            }
            Op::Versions { chat_id, msg_id, respond } => {
                let _ = respond.send(versions(&conn, chat_id, msg_id).await);
            }
        }
    }
}

async fn upsert(conn: &Connection, m: &Msg) -> libsql::Result<()> {
    conn.execute(
        "INSERT INTO versions(chat_id,msg_id,seq,text,replaced_at)
         SELECT chat_id,msg_id,(SELECT COALESCE(MAX(seq),0)+1 FROM versions WHERE chat_id=?1 AND msg_id=?2),text,?4
         FROM messages WHERE chat_id=?1 AND msg_id=?2 AND text<>?3 AND text<>''",
        params![m.chat_id, m.id, m.text.clone(), Local::now().timestamp()],
    ).await?;
    conn.execute(
        "INSERT INTO messages(chat_id,msg_id,sender,sender_id,text,ts,outgoing,media,doc_name,reply_to,edited,topic_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(chat_id,msg_id) DO UPDATE SET
           sender=excluded.sender, sender_id=excluded.sender_id, text=excluded.text,
           outgoing=excluded.outgoing, media=excluded.media, doc_name=excluded.doc_name,
           reply_to=excluded.reply_to, edited=excluded.edited, topic_id=COALESCE(excluded.topic_id,messages.topic_id)",
        params![
            m.chat_id,
            m.id,
            m.sender.clone(),
            m.sender_id,
            m.text.clone(),
            m.ts.timestamp(),
            m.outgoing as i64,
            media_str(m.media).map(str::to_string),
            m.doc_name.clone(),
            m.reply_to.map(|r| r as i64),
            m.edited as i64,
            m.topic_id
        ],
    )
    .await?;
    Ok(())
}

async fn mark_deleted(conn: &Connection, chat_id: Option<i64>, ids: &[i32]) -> Vec<(i64, i32)> {
    let now = Local::now().timestamp();
    let mut out = Vec::new();
    for &id in ids {
        let chats: Vec<i64> = match chat_id {
            Some(c) => vec![c],
            None => {
                let mut v = Vec::new();
                if let Ok(mut rows) = conn
                    .query(
                        "SELECT chat_id FROM messages WHERE msg_id=?1 AND chat_id > -1000000000000",
                        params![id],
                    )
                    .await
                {
                    while let Ok(Some(row)) = rows.next().await {
                        if let Ok(c) = row.get::<i64>(0) {
                            v.push(c);
                        }
                    }
                }
                v
            }
        };
        for c in chats {
            let n = conn
                .execute(
                    "UPDATE messages SET deleted=1, deleted_at=?3 WHERE chat_id=?1 AND msg_id=?2 AND deleted=0",
                    params![c, id, now],
                )
                .await
                .unwrap_or(0);
            if n > 0 || chat_id.is_some() {
                out.push((c, id));
            }
        }
    }
    out
}

async fn deleted_between(conn: &Connection, chat_id: i64, min_id: i32, max_id: i32) -> Vec<Msg> {
    let (chat_id, topic) = super::split_topic_chat_id(chat_id).map(|(c,t)| (c,Some(t))).unwrap_or((chat_id,None));
    let mut out = Vec::new();
    let Ok(mut rows) = conn
        .query(
            "SELECT msg_id,sender,sender_id,text,ts,outgoing,media,doc_name,reply_to,edited,topic_id
             FROM messages WHERE chat_id=?1 AND deleted=1 AND msg_id BETWEEN ?2 AND ?3
             AND (?4 IS NULL OR COALESCE(topic_id,1)=?4) ORDER BY msg_id DESC LIMIT 200",
            params![chat_id, min_id, max_id, topic],
        )
        .await
    else {
        return out;
    };
    while let Ok(Some(row)) = rows.next().await {
        let get_i = |i| row.get::<i64>(i).unwrap_or(0);
        let media = opt_text(row.get_value(6).unwrap_or(Value::Null));
        out.push(Msg {
            id: get_i(0) as i32,
            chat_id,
            chat_title: String::new(),
            sender: row.get::<String>(1).unwrap_or_default(),
            sender_id: opt_int(row.get_value(2).unwrap_or(Value::Null)),
            text: row.get::<String>(3).unwrap_or_default(),
            ts: Local.timestamp_opt(get_i(4), 0).single().unwrap_or_else(Local::now),
            outgoing: get_i(5) != 0,
            media: media_kind(&media),
            doc_name: opt_text(row.get_value(7).unwrap_or(Value::Null)),
            reply_to: opt_int(row.get_value(8).unwrap_or(Value::Null)).map(|r| r as i32),
            reactions: vec![],
            edited: get_i(9) != 0,
            deleted: true,
            topic_id: opt_int(row.get_value(10).unwrap_or(Value::Null)).map(|v| v as i32),
            ..Msg::default()
        });
    }
    out.reverse();
    out
}

async fn versions(conn: &Connection, chat_id: i64, msg_id: i32) -> Vec<MsgVersion> {
    let chat_id = super::split_topic_chat_id(chat_id).map(|(c,_)| c).unwrap_or(chat_id);
    let mut out = Vec::new();
    let Ok(mut rows) = conn
        .query(
            "SELECT text, replaced_at FROM versions WHERE chat_id=?1 AND msg_id=?2 ORDER BY replaced_at, seq",
            params![chat_id, msg_id],
        )
        .await
    else {
        return out;
    };
    while let Ok(Some(row)) = rows.next().await {
        out.push(MsgVersion {
            text: row.get::<String>(0).unwrap_or_default(),
            replaced_at: Local
                .timestamp_opt(row.get::<i64>(1).unwrap_or(0), 0)
                .single()
                .unwrap_or_else(Local::now),
        });
    }
    out
}

/// Import only after the account owner explicitly confirms ownership. The
/// pre-account archive is opened read-only and never removed or rewritten.
async fn import_legacy(target: &Connection, source: &std::path::Path) -> libsql::Result<u64> {
    let db = libsql::Builder::new_local(source).flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY).build().await?;
    let legacy = db.connect()?;
    let mut imported = target.query("SELECT 1 FROM archive_meta WHERE key='legacy_imported'", ()).await?;
    if imported.next().await?.is_some() { return Ok(0); }
    drop(imported);
    let mut columns = legacy.query("PRAGMA table_info(messages)", ()).await?;
    let mut has_topic = false;
    while let Some(row) = columns.next().await? { has_topic |= row.get::<String>(1)?.as_str() == "topic_id"; }
    drop(columns);
    // The transaction prevents a interrupted import leaving a partial result;
    // the marker makes retries idempotent, including overlapping UI requests.
    target.execute("BEGIN IMMEDIATE", ()).await?;
    let result = async {
        let mut rows = legacy.query(&format!("SELECT chat_id,msg_id,sender,sender_id,text,ts,outgoing,media,doc_name,reply_to,edited,deleted,deleted_at,{} FROM messages", if has_topic { "topic_id" } else { "NULL" }), ()).await?;
        let mut count = 0;
        while let Some(row) = rows.next().await? {
            let values = (0..14).map(|index| row.get_value(index)).collect::<libsql::Result<Vec<_>>>()?;
            // Existing current-account rows win; retain the older text as an
            // edit-history entry when the current version differs.
            target.execute("INSERT INTO versions(chat_id,msg_id,seq,text,replaced_at)
                SELECT ?1,?2,COALESCE((SELECT MAX(seq) FROM versions WHERE chat_id=?1 AND msg_id=?2),0)+1,?3,?4
                WHERE EXISTS(SELECT 1 FROM messages WHERE chat_id=?1 AND msg_id=?2 AND text<>?3)
                AND NOT EXISTS(SELECT 1 FROM versions WHERE chat_id=?1 AND msg_id=?2 AND text=?3 AND replaced_at=?4)",
                params![values[0].clone(),values[1].clone(),values[4].clone(),values[5].clone()]).await?;
            count += target.execute("INSERT OR IGNORE INTO messages(chat_id,msg_id,sender,sender_id,text,ts,outgoing,media,doc_name,reply_to,edited,deleted,deleted_at,topic_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", libsql::params_from_iter(values)).await?;
        }
        let mut rows = legacy.query("SELECT chat_id,msg_id,text,replaced_at FROM versions ORDER BY seq", ()).await?;
        while let Some(row) = rows.next().await? {
            target.execute("INSERT INTO versions(chat_id,msg_id,seq,text,replaced_at)
                SELECT ?1,?2,COALESCE((SELECT MAX(seq) FROM versions WHERE chat_id=?1 AND msg_id=?2),0)+1,?3,?4
                WHERE NOT EXISTS(SELECT 1 FROM versions WHERE chat_id=?1 AND msg_id=?2 AND text=?3 AND replaced_at=?4)",
                params![row.get_value(0)?,row.get_value(1)?,row.get_value(2)?,row.get_value(3)?]).await?;
        }
        target.execute("INSERT INTO archive_meta(key,value) VALUES('legacy_imported','1')", ()).await?;
        target.execute("COMMIT", ()).await?;
        Ok(count)
    }.await;
    if result.is_err() { let _ = target.execute("ROLLBACK", ()).await; }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn legacy_import_preserves_original_current_messages_and_edit_history() {
        let dir = std::env::temp_dir().join(format!("omg-import-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("legacy.sqlite");
        let legacy_db = libsql::Builder::new_local(&source).build().await.unwrap();
        let old = legacy_db.connect().unwrap();
        old.execute_batch("CREATE TABLE messages(chat_id,msg_id,sender,sender_id,text,ts,outgoing,media,doc_name,reply_to,edited,deleted,deleted_at); CREATE TABLE versions(chat_id,msg_id,seq,text,replaced_at);
            INSERT INTO messages VALUES(1,10,'A',1,'old current',100,0,NULL,NULL,NULL,1,0,NULL),(1,11,'A',1,'deleted',101,0,NULL,NULL,NULL,0,1,102);
            INSERT INTO versions VALUES(1,10,1,'first text',99);").await.unwrap();
        drop(old); drop(legacy_db);
        let before = std::fs::read(&source).unwrap();
        let db = libsql::Builder::new_local(dir.join("new.sqlite")).build().await.unwrap();
        let target = db.connect().unwrap();
        target.execute_batch("CREATE TABLE messages(chat_id,msg_id,sender,sender_id,text,ts,outgoing,media,doc_name,reply_to,edited,deleted,deleted_at,topic_id,PRIMARY KEY(chat_id,msg_id)); CREATE TABLE versions(chat_id,msg_id,seq,text,replaced_at,PRIMARY KEY(chat_id,msg_id,seq)); CREATE TABLE archive_meta(key TEXT PRIMARY KEY,value TEXT);
            INSERT INTO messages VALUES(1,10,'A',1,'new current',100,0,NULL,NULL,NULL,1,0,NULL,NULL);").await.unwrap();
        assert_eq!(import_legacy(&target, &source).await.unwrap(), 1);
        assert_eq!(import_legacy(&target, &source).await.unwrap(), 0);
        let mut row = target.query("SELECT text FROM messages WHERE msg_id=10", ()).await.unwrap();
        assert_eq!(row.next().await.unwrap().unwrap().get::<String>(0).unwrap(), "new current");
        let texts = versions(&target, 1, 10).await.into_iter().map(|v| v.text).collect::<Vec<_>>();
        assert_eq!(texts, ["first text", "old current"]);
        assert_eq!(deleted_between(&target, 1, 1, 100).await.len(), 1);
        assert_eq!(before, std::fs::read(&source).unwrap());
        drop(row); drop(target); drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn batches_preserve_edits_topics_and_account_isolation() {
        let dir = std::env::temp_dir().join(format!("omg-archive-test-{}", std::process::id()));
        let a = Archive::open_at(dir.join("account-a/archive.sqlite")).await.unwrap();
        let b = Archive::open_at(dir.join("account-b/archive.sqlite")).await.unwrap();
        let forum = -1_000_000_000_123;
        for text in ["first", "second", "third"] {
            a.record(Msg { id: 50, chat_id: forum, text: text.into(), topic_id: Some(10), ..Msg::default() });
        }
        a.record(Msg { id: 51, chat_id: forum, text: "different topic".into(), topic_id: Some(20), ..Msg::default() });
        b.record(Msg { id: 50, chat_id: forum, text: "other account".into(), topic_id: Some(10), ..Msg::default() });
        let edits = a.versions(super::super::topic_chat_id(forum, 10), 50).await;
        assert_eq!(edits.iter().map(|v| v.text.as_str()).collect::<Vec<_>>(), ["first", "second"]);
        a.mark_deleted(Some(forum), vec![50,51]).await;
        let topic = a.deleted_between(super::super::topic_chat_id(forum, 10), 1, 100).await;
        assert_eq!(topic.len(), 1);
        assert_eq!((topic[0].id, topic[0].text.as_str(), topic[0].topic_id), (50, "third", Some(10)));
        assert!(b.deleted_between(forum, 1, 100).await.is_empty());
        assert!(b.versions(forum, 50).await.is_empty());
        drop(a);
        drop(b);
        // Writer exits after its channel closes; avoid deleting a live database.
        tokio::task::yield_now().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
