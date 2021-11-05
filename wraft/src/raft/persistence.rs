use crate::raft::errors::Error;
use crate::raft::{LogEntry, LogIndex, NodeId, TermIndex};
use web_sys::Storage;

#[derive(Debug)]
pub struct PersistentState {
    session_key: String,
    last_log_index: LogIndex,
    current_term: TermIndex,
    voted_for: Option<NodeId>,
    storage: Storage,
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
        };

        if let Some(term) = state.get(state.current_term_key().as_str()) {
            state.set_current_term(term.parse().unwrap());
        }

        if let Some(vote) = state.get(state.voted_for_key().as_str()) {
            state.set_voted_for(Some(&vote));
        }

        state
    }

    pub fn last_log_index(&self) -> LogIndex {
        self.last_log_index
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

    pub fn _append_log(&mut self, entry: LogEntry) -> Result<(), Error> {
        self.last_log_index += 1;
        let key = self.log_key(self.last_log_index);
        let data = serde_json::to_string(&entry)?;
        self.storage.set_item(&key, &data).unwrap();
        Ok(())
    }

    fn get_log(&self, idx: LogIndex) -> Option<LogEntry> {
        let key = self.log_key(idx);
        match self.storage.get_item(&key).unwrap() {
            Some(data) => {
                let entry: LogEntry = serde_json::from_str(&data).unwrap();
                Some(entry)
            }
            None => None,
        }
    }

    pub fn current_term(&self) -> TermIndex {
        self.current_term
    }

    fn set_current_term(&mut self, term: TermIndex) {
        self.current_term = term;
        let key = self.current_term_key();
        let val = term.to_string();
        self.set(&key, &val);
    }

    pub fn voted_for(&self) -> &Option<NodeId> {
        &self.voted_for
    }

    pub fn set_voted_for(&mut self, val: Option<&str>) {
        let key = self.voted_for_key();
        match &val {
            Some(val) => {
                self.voted_for = Some(val.to_string());
                self.set(&key, val);
            }
            None => {
                self.storage.remove_item(&key).unwrap();
                self.voted_for = None;
            }
        }
    }

    fn get(&self, key: &str) -> Option<String> {
        self.storage.get_item(key).unwrap()
    }

    fn set(&self, key: &str, val: &str) {
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
}
