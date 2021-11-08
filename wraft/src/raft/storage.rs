use crate::raft::{LogEntry, LogIndex, NodeId, TermIndex};
use base64::write::EncoderStringWriter;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;
use std::io::Cursor;
use std::marker::PhantomData;

#[derive(Debug)]
pub struct Storage<T> {
    session_key: u128,
    last_log_index: LogIndex,
    last_log_index_key: String,
    current_term: TermIndex,
    current_term_key: String,
    voted_for: Option<NodeId>,
    voted_for_key: String,
    storage: web_sys::Storage,
    _record_type: PhantomData<T>,
}

impl<T> Storage<T>
where
    T: Serialize + DeserializeOwned + Clone + Debug + 'static,
{
    pub fn new(session_key: u128) -> Self {
        let window = web_sys::window().expect("no global window");
        let storage = window.local_storage().expect("no local storage").unwrap();

        let mut state = Self {
            storage,
            session_key,
            last_log_index: 0,
            current_term: 0,
            voted_for: None,
            current_term_key: format!("current-term-{}", session_key),
            voted_for_key: format!("voted-for-{}", session_key),
            last_log_index_key: format!("last-log-index-{}", session_key),
            _record_type: PhantomData,
        };

        if let Some(term) = state.get_persistent(&state.current_term_key) {
            state.current_term = term.parse().unwrap();
        }

        if let Some(vote) = state.get_persistent(&state.voted_for_key) {
            state.voted_for = Some(vote.parse().unwrap());
        }

        if let Some(idx) = state.get_persistent(&state.last_log_index_key) {
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
        let val = idx.to_string();
        self.set_persistent(&self.last_log_index_key, &val);
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
    pub fn append_log(&mut self, entry: &LogEntry<T>) {
        let idx = self.increment_last_log_index();
        assert_eq!(idx, entry.idx);

        let key = self.log_key(idx);
        let mut buf = EncoderStringWriter::new(base64::STANDARD);
        bincode::serialize_into(&mut buf, entry).unwrap();
        let data = buf.into_inner();
        self.storage.set_item(&key, &data).unwrap();
    }

    // Sets log entry at entry.idx and truncates the log from then on out
    // (assuming all following logs are invalid)
    pub fn overwrite_log(&mut self, entry: &LogEntry<T>) {
        assert!(entry.idx >= self.last_log_index());
        assert!(entry.idx != 0);
        self.set_last_log_index(entry.idx - 1);
        self.append_log(entry);
    }

    pub fn get_log(&self, idx: LogIndex) -> Option<LogEntry<T>> {
        // log indices start at 1, as per the paper
        if idx == 0 || idx > self.last_log_index() {
            return None;
        }
        let key = self.log_key(idx);
        let mut data = Cursor::new(self.storage.get_item(&key).unwrap().unwrap()); // Christmas!
        let buf = base64::read::DecoderReader::new(&mut data, base64::STANDARD);
        let entry: LogEntry<T> = bincode::deserialize_from(buf).unwrap();
        Some(entry)
    }

    pub fn sublog(&self, indices: impl Iterator<Item = LogIndex>) -> Vec<LogEntry<T>> {
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
        let val = term.to_string();
        self.set_persistent(&self.current_term_key, &val);
    }

    pub fn voted_for(&self) -> &Option<NodeId> {
        &self.voted_for
    }

    pub fn set_voted_for(&mut self, val: Option<NodeId>) {
        match val {
            Some(val) => {
                self.voted_for = Some(val);
                let sval = val.to_string();
                self.set_persistent(&self.voted_for_key, &sval);
            }
            None => {
                self.storage.remove_item(&self.voted_for_key).unwrap();
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
}
