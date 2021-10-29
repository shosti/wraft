use crate::console_log;
use crate::raft::errors::Error;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::{future, select, SinkExt, StreamExt};
use js_sys::{Function, Promise};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{IdbDatabase, IdbTransactionMode};

type RequestSender = Sender<(DBRequest, oneshot::Sender<Result<DBResponse, DBError>>)>;
type RequestReceiver = Receiver<(DBRequest, oneshot::Sender<Result<DBResponse, DBError>>)>;

// Increment when there are schema changes
const DB_VERSION: u32 = 1;
const LOG_OBJECT_STORE: &str = "log_entries";

#[derive(Debug)]
enum DBRequest {
    Apply(LogEntry),
}

#[derive(Debug)]
enum DBResponse {
    Ack,
}

#[derive(Serialize, Deserialize, Debug)]
struct LogEntry {
    cmd: Cmd,
    term: u64,
    position: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Cmd {
    Set { key: String, data: Vec<u8> },
    Delete { key: String },
}

#[derive(Debug)]
pub struct PersistentState {
    db: DBClient,
}

impl PersistentState {
    pub async fn initialize(session_id: &str) -> Result<Self, Error> {
        let db = DBClient::initialize(session_id).await?;

        Ok(Self { db })
    }
}

#[derive(Debug)]
struct DBClient {
    tx: RequestSender,
}

impl DBClient {
    pub async fn initialize(session_id: &str) -> Result<Self, Error> {
        let tx = run_db(session_id).await?;

        Ok(Self { tx })
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
            DBRequest::Apply(entry) => {
                let res = apply_log_entry(db.clone(), entry).await;
                tx.send(res).unwrap();
            }
        }
    }
}

async fn apply_log_entry(db: IdbDatabase, entry: LogEntry) -> Result<DBResponse, DBError> {
    let tx = db.transaction_with_str_and_mode(LOG_OBJECT_STORE, IdbTransactionMode::Readwrite)?;
    let obj_store = tx.object_store(LOG_OBJECT_STORE).unwrap();
    Ok(DBResponse::Ack)
}

#[derive(Debug)]
pub struct DBError {
    msg: String,
}

impl From<JsValue> for DBError {
    fn from(err: JsValue) -> DBError {
        let msg = match err.as_string() {
            Some(e) => e,
            None => format!("database error: {:?}", err),
        };
        DBError { msg }
    }
}
