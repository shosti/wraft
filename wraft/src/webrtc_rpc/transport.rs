use crate::console_log;
use crate::util::sleep;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::{select, SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, RtcDataChannel, RtcPeerConnection};

const CHANNEL_SIZE: usize = 1024;
const MAX_IN_FLIGHT_REQUESTS: usize = 1024;
const REQUEST_TIMEOUT_MILLIS: u64 = 1000;

#[derive(Serialize, Deserialize)]
enum Message<Req, Resp> {
    Request { idx: usize, req: Req },
    Response { idx: usize, resp: Resp },
}

#[derive(Debug)]
pub struct PeerTransport<Req, Resp> {
    node_id: String,
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    req_tx: Sender<(Req, oneshot::Sender<Result<Resp, Error>>)>,
    // _message_cb: Closure<dyn FnMut(MessageEvent)>,
    _resp: PhantomData<Resp>,
}

#[derive(Debug)]
pub struct Client<Req, Resp> {
    req_tx: Sender<(Req, oneshot::Sender<Result<Resp, Error>>)>,
    _resp: PhantomData<Resp>,
}

impl<Req, Resp> PeerTransport<Req, Resp>
where
    Req: Serialize + DeserializeOwned + Debug + 'static,
    Resp: Serialize + DeserializeOwned + Debug + 'static,
{
    pub fn new(
        node_id: String,
        connection: RtcPeerConnection,
        data_channel: RtcDataChannel,
    ) -> Self {
        data_channel.set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);

        let (client_req_tx, client_req_rx) = channel(CHANNEL_SIZE);
        let (client_resp_tx, client_resp_rx) = channel(CHANNEL_SIZE);

        let req_dc = data_channel.clone();
        spawn_local(async move {
            handle_client_requests(client_req_rx, client_resp_rx, req_dc).await;
        });
        let cb = Closure::wrap(Box::new(move |ev: MessageEvent| {
            let mut resp_tx = client_resp_tx.clone();
            spawn_local(async move {
                let abuf = ev
                    .data()
                    .dyn_into::<js_sys::ArrayBuffer>()
                    .expect("Expected message in binary format");
                let data = js_sys::Uint8Array::new(&abuf).to_vec();

                let msg = bincode::deserialize::<Message<Req, Resp>>(&data).unwrap();
                match msg {
                    Message::Request { idx: _, req: _ } => {
                        // Got a request from the other side's client
                        unimplemented!()
                    },
                    Message::Response { idx: _, resp: _ } => {
                        // Got a response to one of our requests
                        resp_tx.send(msg).await.unwrap();
                    }
                }
            });
        }) as Box<dyn FnMut(MessageEvent)>);
        data_channel.set_onmessage(Some(cb.as_ref().unchecked_ref()));

        // let send_dc = data_channel.clone();
        // let (responses, mut resp_rx) = channel::<Resp>(CHANNEL_SIZE);
        // spawn_local(async move {
        //     while let Some(r) = resp_rx.next().await {
        //         let data = bincode::serialize(&r).unwrap();
        //         // TODO: error handling
        //         send_dc.send_with_u8_array(&data).unwrap();
        //     }
        // });

        Self {
            node_id,
            connection,
            data_channel,
            req_tx: client_req_tx,
            // _message_cb: cb,
            _resp: PhantomData,
        }
    }

    pub fn client(&self) -> Client<Req, Resp> {
        Client {
            req_tx: self.req_tx.clone(),
            _resp: PhantomData,
        }
    }
}

async fn handle_client_requests<Req, Resp>(
    mut req_rx: Receiver<(Req, oneshot::Sender<Result<Resp, Error>>)>,
    mut resp_rx: Receiver<Message<Req, Resp>>,
    dc: RtcDataChannel,
) where
    Req: Serialize + Debug + 'static,
    Resp: Serialize + Debug + 'static,
{
    // Add extra element at the end for swap_remove dance
    let mut in_flight: Vec<Option<oneshot::Sender<Result<Resp, Error>>>> =
        (0..(MAX_IN_FLIGHT_REQUESTS + 1)).map(|_| None).collect();
    let mut min_req_idx: usize = 0;
    let mut next_req_idx: usize = 0;

    let (timeout_tx, mut timeout_rx) = channel::<usize>(MAX_IN_FLIGHT_REQUESTS);

    loop {
        select! {
            res = req_rx.next() => {
                let (req, tx) = res.expect("client request channel is closed");
                let idx = next_req_idx;
                if idx - min_req_idx >= MAX_IN_FLIGHT_REQUESTS {
                    tx.send(Err(Error::TooManyInFlightRequests)).unwrap();
                    continue;
                }

                let msg: Message<Req, Resp> = Message::Request {
                    idx,
                    req,
                };

                let data = bincode::serialize(&msg).unwrap();
                if let Err(err) = dc.send_with_u8_array(&data) {
                    tx.send(Err(Error::Js(err))).unwrap();
                    continue;
                }

                next_req_idx += 1;
                in_flight[idx % MAX_IN_FLIGHT_REQUESTS] = Some(tx);

                let mut tx = timeout_tx.clone();
                spawn_local(async move {
                    sleep(Duration::from_millis(REQUEST_TIMEOUT_MILLIS)).await.unwrap();
                    tx.send(idx).await.unwrap();
                });
            }
            res = resp_rx.next() => {
                let msg = res.expect("client response channel is closed");
                if let Message::Response { idx, resp } = msg {
                    let tx_opt = in_flight.swap_remove(idx % MAX_IN_FLIGHT_REQUESTS);
                    in_flight.push(None);
                    assert_eq!(in_flight.len(), MAX_IN_FLIGHT_REQUESTS + 1);
                    match tx_opt {
                        Some(tx) => tx.send(Ok(resp)).unwrap(),
                        None => {
                            console_log!("request {} came in after request canceled", idx)
                        }
                    }
                }
            }
            res = timeout_rx.next() => {
                let idx = res.expect("timeout channel closed");
                let tx_opt = in_flight.swap_remove(idx % MAX_IN_FLIGHT_REQUESTS);
                in_flight.push(None);
                assert_eq!(in_flight.len(), MAX_IN_FLIGHT_REQUESTS + 1);

                if let Some(tx) = tx_opt {
                    console_log!("request {} timed out", idx);
                    tx.send(Err(Error::RequestTimeout)).unwrap();
                }
            }
        }
        while min_req_idx < next_req_idx && in_flight[min_req_idx].is_none() {
            min_req_idx += 1;
        }
    }
}

impl<Req, Resp> Client<Req, Resp> {
    pub async fn rpc(&mut self, req: Req) -> Result<Resp, Error> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.req_tx.send((req, resp_tx)).await.unwrap();

        resp_rx.await.unwrap()
    }
}

impl<Req, Resp> Drop for PeerTransport<Req, Resp> {
    fn drop(&mut self) {
        self.data_channel.set_onmessage(None);
    }
}

#[derive(Debug)]
pub enum Error {
    String(String),
    Js(JsValue),
    RequestTimeout,
    TooManyInFlightRequests,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            Error::String(msg) => write!(f, "{}", msg),
            Error::Js(err) => {
                if err.is_string() {
                    write!(f, "JavaScript error: {}", err.as_string().unwrap())
                } else {
                    write!(f, "JavaScript error: {:?}", err)
                }
            }
            Error::RequestTimeout => write!(f, "request timeout"),
            Error::TooManyInFlightRequests => write!(f, "too many in-flight requests"),
        }
    }
}
