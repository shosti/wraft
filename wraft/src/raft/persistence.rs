use crate::raft::errors::Error;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use web_sys::Storage;

pub type LogPosition = u64;

#[derive(Serialize, Deserialize, Debug)]
pub struct LogEntry {
    pub cmd: LogCmd,
    pub term: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum LogCmd {
    Set { key: String, data: Vec<u8> },
    Delete { key: String },
}

#[derive(Debug)]
pub struct PersistentState {
    session_key: String,
    last_log_pos: AtomicU64,
}

impl PersistentState {
    pub fn new(session_key: &str) -> Self {
        let state = Self {
            session_key: session_key.to_string(),
            last_log_pos: AtomicU64::new(0),
        };

        if state.get(state.current_term_key().as_str()).is_none() {
            state.set_current_term(0);
        }

        state
    }

    pub fn append_log(&self, entry: LogEntry) -> Result<(), Error> {
        let last_log = self.last_log_pos.fetch_add(1, Ordering::SeqCst);
        let key = self.log_key(last_log + 1);
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
        self.get(&key)
            .expect("current term not set")
            .parse::<u64>()
            .unwrap()
    }

    pub fn set_current_term(&self, term: u64) {
        let key = self.current_term_key();
        let val = term.to_string();
        self.set(&key, &val);
    }

    pub fn voted_for(&self) -> Option<String> {
        let key = self.voted_for_key();
        self.get(&key)
    }

    pub fn set_voted_for(&self, val: Option<&str>) {
        let key = self.voted_for_key();

        match val {
            Some(ref val) => self.set(&key, val),
            None => self.storage().remove_item(&key).unwrap(),
        }
    }

    fn storage(&self) -> Storage {
        let window = web_sys::window().expect("no global window");
        window.local_storage().expect("no local storage").unwrap()
    }

    fn get(&self, key: &str) -> Option<String> {
        self.storage()
            .get_item(key)
            .unwrap()
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
