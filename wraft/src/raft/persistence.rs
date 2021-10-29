use crate::raft::errors::Error;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{DomException, Event};

type LogPosition = u64;

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
        Ok(Self { session_id: session_id.to_string() })
    }

    pub async fn append(&self, entry: LogEntry) -> Result<(), Error> {
        let key = format!("log-{}-{}", self.session_id, entry.position);
        let window = web_sys::window().expect("no global window");
        let storage = window.local_storage().unwrap().unwrap();
        let data = serde_json::to_string(&entry).unwrap();
        storage.set_item(&key, &data).unwrap();
        Ok(())
    }

    pub async fn get(&self, pos: LogPosition) -> Result<LogEntry, Error> {
        let key = format!("log-{}-{}", self.session_id, pos);
        let window = web_sys::window().expect("no global window");
        let storage = window.local_storage().unwrap().unwrap();
        let data = storage.get_item(&key).unwrap().unwrap();
        let entry: LogEntry = serde_json::from_str(&data).unwrap();
        Ok(entry)
    }
}

#[derive(Debug)]
pub enum DBError {
    Js(String),
    Serialization(Box<bincode::ErrorKind>),
}

impl From<JsValue> for DBError {
    fn from(err: JsValue) -> Self {
        let msg = match err.as_string() {
            Some(e) => e,
            None => {
                if let Some(ev) = err.dyn_ref::<Event>() {
                    format!(
                        "error on event with type {} and target {:?}",
                        ev.type_(),
                        ev.target()
                    )
                } else {
                    format!("database error: {:?}", err)
                }
            }
        };
        DBError::Js(msg)
    }
}

impl From<DomException> for DBError {
    fn from(err: DomException) -> Self {
        let msg = format!("{}: {}", err.name(), err.message());
        DBError::Js(msg)
    }
}

impl From<Box<bincode::ErrorKind>> for DBError {
    fn from(err: Box<bincode::ErrorKind>) -> Self {
        DBError::Serialization(err)
    }
}
