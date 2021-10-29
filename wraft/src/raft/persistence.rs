use crate::raft::errors::Error;
use serde::{Deserialize, Serialize};
use web_sys::Storage;

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
    session_key: String,
}

impl PersistentState {
    pub fn new(session_key: &str) -> Self {
        Self {
            session_key: session_key.to_string(),
        }
    }

    pub fn append_log(&self, entry: LogEntry) -> Result<(), Error> {
        let key = self.log_key(entry.position);
        let data = serde_json::to_string(&entry)?;
        self.storage().set_item(&key, &data).unwrap();
        Ok(())
    }

    pub fn get_log(&self, pos: LogPosition) -> Result<LogEntry, Error> {
        let key = self.log_key(pos);
        let data = self.storage().get_item(&key).unwrap().unwrap();
        let entry: LogEntry = serde_json::from_str(&data)?;
        Ok(entry)
    }

    pub fn current_term(&self) -> u64 {
        let key = self.current_term_key();
        self.get(&key).parse::<u64>().unwrap()
    }

    pub fn set_current_term(&self, term: u64) {
        let key = self.current_term_key();
        let val = term.to_string();
        self.set(&key, &val);
    }

    pub fn voted_for(&self) -> String {
        let key = self.voted_for_key();
        self.get(&key)
    }

    pub fn set_voted_for(&self, val: &str) {
        let key = self.voted_for_key();
        self.set(&key, val)
    }

    fn storage(&self) -> Storage {
        let window = web_sys::window().expect("no global window");
        window.local_storage().expect("no local storage").unwrap()
    }

    fn get(&self, key: &str) -> String {
        self.storage()
            .get_item(key)
            .unwrap()
            .expect(format!("no local storage entry for {}", key).as_str())
    }

    fn set(&self, key: &str, val: &str) {
        self.storage().set_item(key, val).unwrap();
    }

    fn log_key(&self, pos: LogPosition) -> String {
        format!("log-{}-{}", self.session_key, pos)
    }

    fn current_term_key(&self) -> String {
        format!("current-term-{}", self.session_key)
    }

    fn voted_for_key(&self) -> String {
        format!("voted-for-{}", self.session_key)
    }
}
