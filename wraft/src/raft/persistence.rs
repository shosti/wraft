use crate::console_log;
use crate::raft::errors::Error;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::{select, SinkExt, StreamExt};
use js_sys::{Function, Promise, Uint8Array};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{DomException, Event, IdbDatabase, IdbTransactionMode};

type RequestSender = Sender<(DBRequest, oneshot::Sender<Result<DBResponse, DBError>>)>;
type RequestReceiver = Receiver<(DBRequest, oneshot::Sender<Result<DBResponse, DBError>>)>;

// Increment when there are schema changes
const DB_VERSION: u32 = 1;
const LOG_OBJECT_STORE: &str = "log_entries";

type LogPosition = u64;

#[derive(Debug)]
enum DBRequest {
    Add(LogEntry),
    Get(LogPosition),
}

#[derive(Debug)]
enum DBResponse {
    Ack,
    Entry(LogEntry),
}

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
    tx: RequestSender,
}

impl PersistentState {
    pub async fn initialize(session_id: &str) -> Result<Self, Error> {
        let tx = run_db(session_id).await?;

        Ok(Self { tx })
    }

    pub async fn append(&self, entry: LogEntry) -> Result<(), Error> {
        let mut tx = self.tx.clone();
        let (resp_tx, resp_rx) = oneshot::channel();
        let req = DBRequest::Add(entry);
        tx.send((req, resp_tx)).await.unwrap();

        match resp_rx.await.unwrap()? {
            DBResponse::Ack => Ok(()),
            _ => unreachable!(),
        }
    }

    pub async fn get(&self, pos: LogPosition) -> Result<LogEntry, Error> {
        let mut tx = self.tx.clone();
        let (resp_tx, resp_rx) = oneshot::channel();
        let req = DBRequest::Get(pos);
        tx.send((req, resp_tx)).await.unwrap();

        match resp_rx.await.unwrap()? {
            DBResponse::Entry(entry) => Ok(entry),
            _ => unreachable!(),
        }
    }
}

async fn run_db(session_id: &str) -> Result<RequestSender, DBError> {
    let (mut ready_tx, mut ready_rx) = channel(1);
    let (mut err_tx, mut err_rx) = channel(1);
    let (client_tx, client_rx) = channel(1000);

    let db_name = format!("wraft-log-{}", session_id);

    spawn_local(async move {
        let window = web_sys::window().expect("no global window");
        let factory = window
            .indexed_db()
            .unwrap()
            .expect("indexed DB not available");

        let req = factory.open_with_u32(&db_name, DB_VERSION).unwrap();
        let r = req.clone();
        let mut p = move |resolve: Function, reject: Function| {
            let r2 = r.clone();
            let rej = reject.clone();
            let upgrade_db = Closure::wrap(Box::new(move || {
                console_log!("UPGRADING DB");
                let res = r2.result().unwrap();
                let db = res
                    .dyn_ref::<IdbDatabase>()
                    .expect("should have gotten a database from database request");

                match db.create_object_store(LOG_OBJECT_STORE) {
                    Ok(_) => (),
                    Err(err) => {
                        rej.call1(&JsValue::UNDEFINED, &err).unwrap();
                    }
                }
            }) as Box<dyn FnMut()>);
            r.set_onupgradeneeded(Some(upgrade_db.as_ref().unchecked_ref()));
            // Memory leak, but there should really only be one DB client per Raft so probably not a big deal.
            upgrade_db.forget();

            r.set_onsuccess(Some(&resolve));
            r.set_onerror(Some(&reject));
        };

        let db: IdbDatabase;
        match JsFuture::from(Promise::new(&mut p)).await {
            Ok(_) => {
                console_log!("WE GOT A DB!!!");
                db = req
                    .result()
                    .unwrap()
                    .dyn_ref::<IdbDatabase>()
                    .expect("should have gotten a database from database request")
                    .clone();
                ready_tx.send(()).await.unwrap();
            }
            Err(err) => {
                console_log!("DB ERR: {:#?}", err);
                err_tx.send(err).await.unwrap();
                return;
            }
        }

        handle_db_requests(db, client_rx).await;
    });

    select! {
        _ = ready_rx.next() => Ok(client_tx),
        err = err_rx.next() => Err(err.unwrap().into()),
    }
}

async fn handle_db_requests(db: IdbDatabase, mut rx: RequestReceiver) {
    while let Some((req, tx)) = rx.next().await {
        match req {
            DBRequest::Add(entry) => {
                let res = add_log_entry(db.clone(), entry).await;
                tx.send(res).unwrap();
            }
            DBRequest::Get(pos) => {
                let res = get_log_entry(db.clone(), pos).await;
                tx.send(res).unwrap();
            }
        }
    }
}

async fn add_log_entry(db: IdbDatabase, entry: LogEntry) -> Result<DBResponse, DBError> {
    let tx = db.transaction_with_str_and_mode(LOG_OBJECT_STORE, IdbTransactionMode::Readwrite)?;
    let obj_store = tx.object_store(LOG_OBJECT_STORE)?;

    let key: JsValue = entry.position.to_string().into();
    let data = bincode::serialize(&entry)?;
    let buf = Uint8Array::new_with_length(data.len() as u32);
    buf.copy_from(&data);
    let req = obj_store.put_with_key(&buf, &key)?;

    let mut p = move |resolve: Function, reject: Function| {
        tx.set_oncomplete(Some(&resolve));
        tx.set_onerror(Some(&reject));
    };
    if let Err(_) = JsFuture::from(Promise::new(&mut p)).await {
        return Err(req.error().unwrap().unwrap().into());
    }

    Ok(DBResponse::Ack)
}

// This is mostly for testing, probably will want to go with cursors eventually
async fn get_log_entry(db: IdbDatabase, pos: LogPosition) -> Result<DBResponse, DBError> {
    let tx = db.transaction_with_str_and_mode(LOG_OBJECT_STORE, IdbTransactionMode::Readonly)?;
    let obj_store = tx.object_store(LOG_OBJECT_STORE)?;

    let key: JsValue = pos.to_string().into();
    let req = obj_store.get(&key)?;

    let r = req.clone();
    let mut p = move |resolve: Function, reject: Function| {
        r.set_onsuccess(Some(&resolve));
        r.set_onerror(Some(&reject));
    };

    if let Err(_) = JsFuture::from(Promise::new(&mut p)).await {
        return Err(req.error().unwrap().unwrap().into());
    }
    let res = req.result()?;
    let data = res
        .dyn_ref::<Uint8Array>()
        .expect("result should be a uint8array")
        .to_vec();
    let entry: LogEntry = bincode::deserialize(&data)?;

    Ok(DBResponse::Entry(entry))
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
