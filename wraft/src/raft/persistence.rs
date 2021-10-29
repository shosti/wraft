use crate::raft::errors::Error;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::{future, select, SinkExt, StreamExt};
use js_sys::{Function, Promise};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::IdbDatabase;

type CmdSender = Sender<(Cmd, oneshot::Sender<Result<(), Error>>)>;
type CmdReceiver = Receiver<(Cmd, oneshot::Sender<Result<(), Error>>)>;

#[derive(Serialize, Deserialize, Debug)]
pub enum Cmd {}

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
    tx: CmdSender,
}

impl DBClient {
    pub async fn initialize(session_id: &str) -> Result<Self, Error> {
        let tx = run_db(session_id).await?;

        Ok(Self { tx })
    }
}

async fn run_db(session_id: &str) -> Result<CmdSender, Error> {
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

        let req = factory.open(&db_name).unwrap();
        let r = req.clone();
        let mut p = move |resolve: Function, reject: Function| {
            r.set_onsuccess(Some(&resolve));
            r.set_onerror(Some(&reject));
        };

        let db: IdbDatabase;
        match JsFuture::from(Promise::new(&mut p)).await {
            Ok(_) => {
                db = req
                    .result()
                    .unwrap()
                    .dyn_ref::<IdbDatabase>()
                    .expect("should have gotten a datbase from database request")
                    .clone();
                ready_tx.send(()).await.unwrap();
            }
            Err(err) => {
                err_tx.send(err).await.unwrap();
                return;
            }
        }

        handle_db_requests(db, client_rx).await;
    });

    select! {
        _ = ready_rx.next() => Ok(client_tx),
        res = err_rx.next() => {
            let err = Error::DatabaseError(res.unwrap().as_string().unwrap());
            Err(err)
        }
    }
}

async fn handle_db_requests(_db: IdbDatabase, _rx: CmdReceiver) {
    future::pending::<()>().await;
}
