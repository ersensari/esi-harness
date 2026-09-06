use crate::conversation::message::Message;
use anyhow::{Context, Result};
use heed::{
    types::{Bytes, Str},
    Database, Env, EnvOpenOptions,
};
use std::path::Path;

const MAP_SIZE: usize = 1024 * 1024 * 1024;
const RECORDS_DB: &str = "archived_messages";
const IDS_DB: &str = "message_ids";

/// A local LMDB-backed index of messages archived by ESI Context Management.
///
/// The session transcript remains the recovery source. This index is rebuilt
/// idempotently from its compaction provenance before every tool query, so a
/// missing or stale index can never make unrelated hidden messages recallable.
pub struct ContextArchiveStore {
    env: Env,
    records: Database<Str, Bytes>,
    ids: Database<Str, Str>,
}

impl ContextArchiveStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)
            .with_context(|| format!("create context archive directory {}", path.display()))?;

        // SAFETY: ESI owns this directory exclusively, uses a fixed map size,
        // and never resizes an open environment. Those are heed's required
        // caller invariants for EnvOpenOptions::open.
        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(MAP_SIZE)
                .max_dbs(2)
                .open(path)
        }
        .with_context(|| format!("open context archive at {}", path.display()))?;

        let mut txn = env.write_txn()?;
        let records = env.create_database(&mut txn, Some(RECORDS_DB))?;
        let ids = env.create_database(&mut txn, Some(IDS_DB))?;
        txn.commit()?;
        Ok(Self { env, records, ids })
    }

    fn prefix(session_id: &str) -> String {
        format!("{session_id}\0")
    }

    fn record_key(session_id: &str, message: &Message, ordinal: usize) -> String {
        let id = message.id.as_deref().unwrap_or("no-id");
        format!("{session_id}\0{:020}\0{ordinal:08}\0{id}", message.created)
    }

    fn id_key(session_id: &str, message_id: &str) -> String {
        format!("{session_id}\0{message_id}")
    }

    pub fn replace_session<'a>(
        &self,
        session_id: &str,
        messages: impl IntoIterator<Item = &'a Message>,
    ) -> Result<()> {
        let prefix = Self::prefix(session_id);
        let mut txn = self.env.write_txn()?;

        let record_keys = self
            .records
            .prefix_iter(&txn, prefix.as_str())?
            .map(|entry| entry.map(|(key, _)| key.to_owned()))
            .collect::<heed::Result<Vec<_>>>()?;
        for key in record_keys {
            self.records.delete(&mut txn, key.as_str())?;
        }

        let id_keys = self
            .ids
            .prefix_iter(&txn, prefix.as_str())?
            .map(|entry| entry.map(|(key, _)| key.to_owned()))
            .collect::<heed::Result<Vec<_>>>()?;
        for key in id_keys {
            self.ids.delete(&mut txn, key.as_str())?;
        }

        for (ordinal, message) in messages.into_iter().enumerate() {
            let record_key = Self::record_key(session_id, message, ordinal);
            let encoded = serde_json::to_vec(message)?;
            self.records
                .put(&mut txn, record_key.as_str(), encoded.as_slice())?;
            if let Some(message_id) = message.id.as_deref() {
                let id_key = Self::id_key(session_id, message_id);
                self.ids
                    .put(&mut txn, id_key.as_str(), record_key.as_str())?;
            }
        }

        txn.commit()?;
        Ok(())
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let txn = self.env.read_txn()?;
        let mut messages = Vec::new();
        for entry in self
            .records
            .prefix_iter(&txn, Self::prefix(session_id).as_str())?
        {
            let (_, value) = entry?;
            messages.push(serde_json::from_slice(value)?);
        }
        Ok(messages)
    }

    pub fn get(&self, session_id: &str, message_id: &str) -> Result<Option<Message>> {
        let txn = self.env.read_txn()?;
        let Some(record_key) = self
            .ids
            .get(&txn, Self::id_key(session_id, message_id).as_str())?
        else {
            return Ok(None);
        };
        let Some(value) = self.records.get(&txn, record_key)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(value)?))
    }
}

#[cfg(test)]
mod tests {
    use super::ContextArchiveStore;
    use crate::conversation::message::Message;

    #[test]
    fn replaces_only_the_selected_session_and_supports_exact_get() {
        let root = tempfile::tempdir().unwrap();
        let store = ContextArchiveStore::open(root.path()).unwrap();
        let first = Message::user()
            .with_text("first archived decision")
            .with_id("first");
        let second = Message::assistant()
            .with_text("second archived result")
            .with_id("second");

        store
            .replace_session("session-a", [&first, &second])
            .unwrap();
        store.replace_session("session-b", [&first]).unwrap();
        assert_eq!(store.messages("session-a").unwrap().len(), 2);
        assert_eq!(store.messages("session-b").unwrap().len(), 1);
        assert_eq!(
            store.get("session-a", "second").unwrap().unwrap().id,
            Some("second".to_string())
        );

        store.replace_session("session-a", [&second]).unwrap();
        assert_eq!(store.messages("session-a").unwrap().len(), 1);
        assert!(store.get("session-a", "first").unwrap().is_none());
        assert_eq!(store.messages("session-b").unwrap().len(), 1);
    }
}
