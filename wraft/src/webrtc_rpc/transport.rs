use crate::console_log;
use crate::util::sleep;
use async_trait::async_trait;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::{select, SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
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

pub struct PeerTransport<Req, Resp> {
    node_id: String,
    data_channel: RtcDataChannel,
    client_req_tx: Sender<(Req, oneshot::Sender<Result<Resp, Error>>)>,
    server_req_rx: Receiver<(Req, oneshot::Sender<Resp>)>,
    _connection: RtcPeerConnection,
    _message_cb: Closure<dyn FnMut(MessageEvent)>,
}

#[derive(Debug, Clone)]
pub struct Client<Req, Resp> {
    pub node_id: String,
    req_tx: Sender<(Req, oneshot::Sender<Result<Resp, Error>>)>,
}

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub source_node_id: String,
}

#[async_trait]
pub trait RequestHandler<Req, Resp> {
    async fn handle(&self, req: Req, cx: RequestContext) -> Resp;
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
        let (server_req_tx, server_req_rx) = channel(CHANNEL_SIZE);

        let req_dc = data_channel.clone();
        spawn_local(async move {
            handle_client_requests(client_req_rx, client_resp_rx, req_dc).await;
        });
        let msg_dc = data_channel.clone();
        let cb = Closure::wrap(Box::new(move |ev: MessageEvent| {
            let mut client_tx = client_resp_tx.clone();
            let mut server_tx = server_req_tx.clone();
            let dc = msg_dc.clone();
            spawn_local(async move {
                let abuf = ev
                    .data()
                    .dyn_into::<js_sys::ArrayBuffer>()
                    .expect("Expected message in binary format");
                let data = js_sys::Uint8Array::new(&abuf).to_vec();

                let msg = bincode::deserialize::<Message<Req, Resp>>(&data).unwrap();
                match msg {
                    Message::Request { idx, req } => {
                        // Got a request from the other side's client
                        let (tx, rx) = oneshot::channel();
                        server_tx.send((req, tx)).await.unwrap();
                        let resp = rx.await.unwrap();

                        let msg: Message<Req, Resp> = Message::Response { idx, resp };
                        let data = bincode::serialize(&msg).unwrap();
                        if let Err(err) = dc.send_with_u8_array(&data) {
                            console_log!("error sending data: {:?}", err);
                        }
                    }
                    Message::Response { idx: _, resp: _ } => {
                        // Got a response to one of our requests
                        client_tx.send(msg).await.unwrap();
                    }
                }
            });
        }) as Box<dyn FnMut(MessageEvent)>);
        data_channel.set_onmessage(Some(cb.as_ref().unchecked_ref()));

        Self {
            node_id,
            data_channel,
            client_req_tx,
            server_req_rx,
            _connection: connection,
            _message_cb: cb,
        }
    }

    pub fn node_id(&self) -> String {
        self.node_id.clone()
    }

    pub async fn serve(&mut self, handler: impl RequestHandler<Req, Resp> + 'static) {
        let cx = RequestContext {
            source_node_id: self.node_id.clone(),
        };
        while let Some((req, tx)) = self.server_req_rx.next().await {
            let resp = handler.handle(req, cx.clone()).await;
            tx.send(resp).unwrap();
        }
    }

    pub fn client(&self) -> Client<Req, Resp> {
        Client {
            node_id: self.node_id.clone(),
            req_tx: self.client_req_tx.clone(),
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
                    tx.send(Err(Error::from(err))).unwrap();
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
        while min_req_idx < next_req_idx && in_flight[min_req_idx % MAX_IN_FLIGHT_REQUESTS].is_none() {
            min_req_idx += 1;
        }
    }
}

impl<Req, Resp> Client<Req, Resp> {
    pub async fn call(&mut self, req: Req) -> Result<Resp, Error> {
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

impl<Req, Resp> Debug for PeerTransport<Req, Resp> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "PeerTransport for {} ({:?})",
            self.node_id,
            self.data_channel.ready_state()
        )
    }
}

#[derive(Debug)]
pub enum Error {
    Js(String),
    RequestTimeout,
    TooManyInFlightRequests,
}

impl From<JsValue> for Error {
    fn from(err: JsValue) -> Self {
        match err.as_string() {
            Some(err) => Error::Js(format!("JavaScript error: {}", err)),
            None => Error::Js(format!("JavaScript error: {:?}", err)),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            Error::Js(err) => write!(f, "{}", err),
            Error::RequestTimeout => write!(f, "request timeout"),
            Error::TooManyInFlightRequests => write!(f, "too many in-flight requests"),
        }
    }
}
