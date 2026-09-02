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
    Upsert(Msg),
    MarkDeleted { chat_id: Option<i64>, ids: Vec<i32>, respond: oneshot::Sender<Vec<(i64, i32)>> },
    DeletedBetween { chat_id: i64, min_id: i32, max_id: i32, respond: oneshot::Sender<Vec<Msg>> },
    Versions { chat_id: i64, msg_id: i32, respond: oneshot::Sender<Vec<MsgVersion>> },
}

fn path() -> PathBuf {
    dirs::data_dir()
        .expect("cannot determine XDG data dir — is HOME set?")
        .join("omarchygram/archive.sqlite")
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
    pub async fn open() -> Result<Archive, String> {
        let p = path();
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
            "CREATE TABLE IF NOT EXISTS messages(
               chat_id INTEGER NOT NULL, msg_id INTEGER NOT NULL,
               sender TEXT NOT NULL DEFAULT '', sender_id INTEGER,
               text TEXT NOT NULL DEFAULT '', ts INTEGER NOT NULL,
               outgoing INTEGER NOT NULL DEFAULT 0, media TEXT, doc_name TEXT,
               reply_to INTEGER, edited INTEGER NOT NULL DEFAULT 0,
               deleted INTEGER NOT NULL DEFAULT 0, deleted_at INTEGER,
               PRIMARY KEY(chat_id, msg_id));
             CREATE INDEX IF NOT EXISTS messages_by_msg ON messages(msg_id);
             CREATE TABLE IF NOT EXISTS versions(
               chat_id INTEGER NOT NULL, msg_id INTEGER NOT NULL, seq INTEGER NOT NULL,
               text TEXT NOT NULL, replaced_at INTEGER NOT NULL,
               PRIMARY KEY(chat_id, msg_id, seq));",
        )
        .await
        .map_err(|e| format!("archive schema: {e}"))?;
        restrict(&p); // sidecars may have appeared during schema setup
        let (tx, rx) = mpsc::unbounded_channel();
        let p2 = p.clone();
        tokio::spawn(async move {
            writer(conn, rx).await;
            restrict(&p2);
        });
        Ok(Archive { tx })
    }

    /// Fire-and-forget upsert (keeps message order; never blocks the caller).
    pub fn record(&self, m: Msg) {
        let _ = self.tx.send(Op::Upsert(m));
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

async fn writer(conn: Connection, mut rx: mpsc::UnboundedReceiver<Op>) {
    let mut writes: u64 = 0;
    while let Some(op) = rx.recv().await {
        match op {
            Op::Upsert(m) => {
                // Version + upsert in ONE transaction: a crash between them
                // can't leave a stale current text or duplicate a version.
                let _ = conn.execute("BEGIN", ()).await;
                match upsert(&conn, &m).await {
                    Ok(()) => {
                        let _ = conn.execute("COMMIT", ()).await;
                    }
                    Err(e) => {
                        let _ = conn.execute("ROLLBACK", ()).await;
                        eprintln!("omarchygram: archive write failed: {e}");
                    }
                }
                writes += 1;
                if writes == 1 || writes % 200 == 0 {
                    restrict(&path()); // sidecars appear lazily
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
    let mut rows = conn
        .query(
            "SELECT text FROM messages WHERE chat_id=?1 AND msg_id=?2",
            params![m.chat_id, m.id],
        )
        .await?;
    let existing: Option<String> = match rows.next().await? {
        Some(row) => opt_text(row.get_value(0)?),
        None => None,
    };
    if let Some(old) = existing {
        if old != m.text && !old.is_empty() {
            let mut r = conn
                .query(
                    "SELECT COALESCE(MAX(seq),0)+1 FROM versions WHERE chat_id=?1 AND msg_id=?2",
                    params![m.chat_id, m.id],
                )
                .await?;
            let seq: i64 = match r.next().await? {
                Some(row) => row.get::<i64>(0).unwrap_or(1),
                None => 1,
            };
            conn.execute(
                "INSERT OR IGNORE INTO versions(chat_id,msg_id,seq,text,replaced_at) VALUES(?1,?2,?3,?4,?5)",
                params![m.chat_id, m.id, seq, old, Local::now().timestamp()],
            )
            .await?;
        }
    }
    conn.execute(
        "INSERT INTO messages(chat_id,msg_id,sender,sender_id,text,ts,outgoing,media,doc_name,reply_to,edited)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(chat_id,msg_id) DO UPDATE SET
           sender=excluded.sender, sender_id=excluded.sender_id, text=excluded.text,
           outgoing=excluded.outgoing, media=excluded.media, doc_name=excluded.doc_name,
           reply_to=excluded.reply_to, edited=excluded.edited",
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
            m.edited as i64
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
    let mut out = Vec::new();
    let Ok(mut rows) = conn
        .query(
            "SELECT msg_id,sender,sender_id,text,ts,outgoing,media,doc_name,reply_to,edited
             FROM messages WHERE chat_id=?1 AND deleted=1 AND msg_id BETWEEN ?2 AND ?3 ORDER BY msg_id",
            params![chat_id, min_id, max_id],
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
            ..Msg::default()
        });
    }
    out
}

async fn versions(conn: &Connection, chat_id: i64, msg_id: i32) -> Vec<MsgVersion> {
    let mut out = Vec::new();
    let Ok(mut rows) = conn
        .query(
            "SELECT text, replaced_at FROM versions WHERE chat_id=?1 AND msg_id=?2 ORDER BY seq",
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
