use crate::raft::{LogEntry, LogIndex, NodeId, TermIndex};
use std::cmp::min;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use web_sys::Storage;

#[derive(Debug)]
pub struct PersistentState {
    session_key: String,
    last_log_index: LogIndex,
    current_term: TermIndex,
    voted_for: Option<NodeId>,
    storage: Storage,
    snapshot: HashMap<String, String>,
}

impl PersistentState {
    pub fn new(session_key: &str) -> Self {
        let window = web_sys::window().expect("no global window");
        let storage = window.local_storage().expect("no local storage").unwrap();

        let mut state = Self {
            storage,
            session_key: session_key.to_string(),
            last_log_index: 0,
            current_term: 0,
            voted_for: None,
            snapshot: HashMap::new(),
        };

        if let Some(term) = state.get_persistent(state.current_term_key().as_str()) {
            state.current_term = term.parse().unwrap();
        }

        if let Some(vote) = state.get_persistent(state.voted_for_key().as_str()) {
            state.voted_for = Some(vote);
        }

        if let Some(idx) = state.get_persistent(state.last_log_index_key().as_str()) {
            state.last_log_index = idx.parse().unwrap();
        }

        state
    }

    pub fn last_log_index(&self) -> LogIndex {
        self.last_log_index
    }

    fn increment_last_log_index(&mut self) -> LogIndex {
        let idx = self.last_log_index() + 1;
        self.set_last_log_index(idx);
        idx
    }

    fn set_last_log_index(&mut self, idx: LogIndex) {
        self.last_log_index = idx;
        let key = self.last_log_index_key();
        let val = idx.to_string();
        self.set_persistent(&key, &val);
    }

    pub fn last_log_term(&self) -> TermIndex {
        match self.get_log(self.last_log_index()) {
            Some(entry) => entry.term,
            None => 0,
        }
    }

    // Returns true if the term needed to be updated
    pub fn update_term(&mut self, term: TermIndex) -> bool {
        if self.current_term() < term {
            self.set_voted_for(None);
            self.set_current_term(term);
            true
        } else {
            false
        }
    }

    pub fn increment_term(&mut self) {
        self.set_voted_for(None);
        self.set_current_term(self.current_term() + 1);
    }

    // This explodes if you use it wrong!
    pub fn append_log(&mut self, entry: LogEntry) {
        let idx = self.increment_last_log_index();
        assert_eq!(idx, entry.idx);

        let key = self.log_key(idx);
        let data = serde_json::to_string(&entry).unwrap();
        self.storage.set_item(&key, &data).unwrap();
    }

    pub fn get_log(&self, idx: LogIndex) -> Option<LogEntry> {
        // log indices start at 1, as per the paper
        if idx == 0 || idx > self.last_log_index() {
            return None;
        }
        let key = self.log_key(idx);
        let data = self.storage.get_item(&key).unwrap().unwrap(); // Christmas!
        let entry: LogEntry = serde_json::from_str(&data).unwrap();
        Some(entry)
    }

    pub fn truncate_from(&mut self, idx: LogIndex) {
        let new_index = min(idx - 1, self.last_log_index());
        self.set_last_log_index(new_index);
    }

    pub fn sublog(&self, indices: RangeInclusive<LogIndex>) -> Vec<LogEntry> {
        indices
            .map(|i| self.get_log(i))
            .filter(|e| e.is_some())
            .flatten()
            .collect()
    }

    pub fn current_term(&self) -> TermIndex {
        self.current_term
    }

    fn set_current_term(&mut self, term: TermIndex) {
        self.current_term = term;
        let key = self.current_term_key();
        let val = term.to_string();
        self.set_persistent(&key, &val);
    }

    pub fn voted_for(&self) -> &Option<NodeId> {
        &self.voted_for
    }

    pub fn set_voted_for(&mut self, val: Option<&str>) {
        let key = self.voted_for_key();
        match &val {
            Some(val) => {
                self.voted_for = Some(val.to_string());
                self.set_persistent(&key, val);
            }
            None => {
                self.storage.remove_item(&key).unwrap();
                self.voted_for = None;
            }
        }
    }

    fn get_persistent(&self, key: &str) -> Option<String> {
        self.storage.get_item(key).unwrap()
    }

    fn set_persistent(&self, key: &str, val: &str) {
        self.storage.set_item(key, val).unwrap();
    }

    fn log_key(&self, idx: LogIndex) -> String {
        format!("log-{}-{}", self.session_key, idx)
    }

    fn current_term_key(&self) -> String {
        format!("current-term-{}", self.session_key)
    }

    fn voted_for_key(&self) -> String {
        format!("voted-for-{}", self.session_key)
    }

    fn last_log_index_key(&self) -> String {
        format!("last-log-index-{}", self.session_key)
    }
}
