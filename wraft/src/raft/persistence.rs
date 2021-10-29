use crate::raft::errors::Error;
use serde::{Deserialize, Serialize};

pub type LogPosition = u64;

#[derive(Serialize, Deserialize, Debug)]
pub struct LogEntry {
    pub cmd: LogCmd,
    pub term: u64,
    pub position: LogPosition,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum LogCmd {
    Set { key: String, data: Vec<u8> },
    Delete { key: String },
}

#[derive(Debug, Clone)]
pub struct PersistentState {
    session_id: String,
}

impl PersistentState {
    pub async fn initialize(session_id: &str) -> Result<Self, Error> {
        Ok(Self {
            session_id: session_id.to_string(),
        })
    }

    pub async fn append(&self, entry: LogEntry) -> Result<(), Error> {
        let key = format!("log-{}-{}", self.session_id, entry.position);
        let window = web_sys::window().expect("no global window");
        let storage = window.local_storage().unwrap().unwrap();
        let data = serde_json::to_string(&entry)?;
        storage.set_item(&key, &data).unwrap();
        Ok(())
    }

    pub async fn get(&self, pos: LogPosition) -> Result<LogEntry, Error> {
        let key = format!("log-{}-{}", self.session_id, pos);
        let window = web_sys::window().expect("no global window");
        let storage = window.local_storage().unwrap().unwrap();
        let data = storage.get_item(&key).unwrap().unwrap();
        let entry: LogEntry = serde_json::from_str(&data)?;
        Ok(entry)
    }
}
