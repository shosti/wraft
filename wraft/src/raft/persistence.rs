use crate::raft::errors::Error;
use crate::raft::{LogPosition, TermIndex};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use web_sys::Storage;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LogEntry {
    pub cmd: LogCmd,
    pub term: TermIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
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

    pub fn last_log_index(&self) -> LogPosition {
        self.last_log_pos.load(Ordering::SeqCst)
    }

    pub fn last_log_term(&self) -> TermIndex {
        match self.get_log(self.last_log_index()) {
            Some(entry) => entry.term,
            None => 0,
        }
    }

    pub fn _append_log(&self, entry: LogEntry) -> Result<(), Error> {
        let last_log = self.last_log_pos.fetch_add(1, Ordering::SeqCst);
        let key = self.log_key(last_log + 1);
        let data = serde_json::to_string(&entry)?;
        self.storage().set_item(&key, &data).unwrap();
        Ok(())
    }

    fn get_log(&self, pos: LogPosition) -> Option<LogEntry> {
        let key = self.log_key(pos);
        match self.storage().get_item(&key).unwrap() {
            Some(data) => {
                let entry: LogEntry = serde_json::from_str(&data).unwrap();
                Some(entry)
            }
            None => None,
        }
    }

    pub fn current_term(&self) -> TermIndex {
        let key = self.current_term_key();
        self.get(&key)
            .expect("current term not set")
            .parse::<TermIndex>()
            .unwrap()
    }

    pub fn set_current_term(&self, term: TermIndex) {
        let key = self.current_term_key();
        let val = term.to_string();
        self.set(&key, &val);
    }

    pub fn _voted_for(&self) -> Option<String> {
        let key = self.voted_for_key();
        self.get(&key)
    }

    pub fn set_voted_for(&self, val: Option<&str>) {
        let key = self.voted_for_key();

        match val {
            Some(val) => self.set(&key, val),
            None => self.storage().remove_item(&key).unwrap(),
        }
    }

    fn storage(&self) -> Storage {
        let window = web_sys::window().expect("no global window");
        window.local_storage().expect("no local storage").unwrap()
    }

    fn get(&self, key: &str) -> Option<String> {
        self.storage().get_item(key).unwrap()
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
