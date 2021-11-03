use crate::console_log;
use crate::util::sleep;
use async_trait::async_trait;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::{select, SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, RtcDataChannel, RtcPeerConnection};

const CHANNEL_SIZE: usize = 1024;
const MAX_IN_FLIGHT_REQUESTS: usize = 1024;
const REQUEST_TIMEOUT_MILLIS: u64 = 1000;

type RequestSender<Req, Resp> = Sender<(Req, oneshot::Sender<Result<Resp, Error>>)>;
type RequestReceiver<Req, Resp> = Receiver<(Req, oneshot::Sender<Result<Resp, Error>>)>;

#[derive(Serialize, Deserialize)]
enum Message<Req, Resp> {
    Request {
        idx: usize,
        req: Req,
    },
    Response {
        idx: usize,
        resp: Result<Resp, Error>,
    },
}

#[derive(Clone, Debug)]
pub enum Status {
    Connected,
    Closed,
}

pub struct PeerTransport<Req, Resp> {
    node_id: String,
    data_channel: RtcDataChannel,
    status: Arc<RwLock<Status>>,
    client_req_tx: RequestSender<Req, Resp>,
    server_req_rx: RequestReceiver<Req, Resp>,
    _connection: RtcPeerConnection,
    _onmessage_cb: Closure<dyn FnMut(MessageEvent)>,
    _onclose_cb: Closure<dyn FnMut()>,
    _onerror_cb: Closure<dyn FnMut()>,
}

#[derive(Debug, Clone)]
pub struct Client<Req, Resp> {
    status: Arc<RwLock<Status>>,
    req_tx: RequestSender<Req, Resp>,
}

#[async_trait]
pub trait RequestHandler<Req, Resp> {
    async fn handle(&self, req: Req) -> Result<Resp, Error>;
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

        spawn_local(handle_client_requests(
            client_req_rx,
            client_resp_rx,
            data_channel.clone(),
        ));
        let onmessage_cb = Self::onmessage_callback(
            client_resp_tx,
            server_req_tx,
            data_channel.clone(),
        );
        data_channel.set_onmessage(Some(onmessage_cb.as_ref().unchecked_ref()));

        let status = Arc::new(RwLock::new(Status::Connected));
        let s = status.clone();
        let nid = node_id.clone();
        let onclose_cb = Closure::wrap(Box::new(move || {
            console_log!("lost data channel for {}", nid);
            let mut status = s.write().unwrap();
            *status = Status::Closed;
        }) as Box<dyn FnMut()>);
        data_channel.set_onclose(Some(onclose_cb.as_ref().unchecked_ref()));

        let onerror_cb = Closure::wrap(Box::new(move || {
            console_log!("GOT ONERROR!!!");
        }) as Box<dyn FnMut()>);
        data_channel.set_onerror(Some(onerror_cb.as_ref().unchecked_ref()));

        Self {
            status,
            node_id,
            data_channel,
            client_req_tx,
            server_req_rx,
            _onmessage_cb: onmessage_cb,
            _connection: connection,
            _onclose_cb: onclose_cb,
            _onerror_cb: onerror_cb,
        }
    }

    fn onmessage_callback(
        client_resp_tx: Sender<Message<Req, Resp>>,
        server_req_tx: RequestSender<Req, Resp>,
        data_channel: RtcDataChannel,
    ) -> Closure<dyn FnMut(MessageEvent)> {
        Closure::wrap(Box::new(move |ev: MessageEvent| {
            let mut client_tx = client_resp_tx.clone();
            let mut server_tx = server_req_tx.clone();
            let dc = data_channel.clone();
            spawn_local(async move {
                let abuf = ev
                    .data()
                    .dyn_into::<js_sys::ArrayBuffer>()
                    .expect("Expected message in binary format");
                let data = js_sys::Uint8Array::new(&abuf).to_vec();

                match bincode::deserialize::<Message<Req, Resp>>(&data).unwrap() {
                    Message::Request { idx, req } => {
                        // Got a request from the other side's client
                        let (tx, rx) = oneshot::channel();
                        server_tx
                            .send((req, tx))
                            .await
                            .expect("server request channel closed");
                        let resp = rx.await.expect("channel closed");

                        let msg: Message<Req, Resp> = Message::Response { idx, resp };
                        let data = bincode::serialize(&msg).unwrap();
                        if let Err(err) = dc.send_with_u8_array(&data) {
                            console_log!("error sending data: {:?}", err);
                        }
                    }
                    msg@Message::Response { idx: _, resp: _ } => {
                        // Got a response to one of our requests
                        client_tx
                            .send(msg)
                            .await
                            .expect("client response channel closed");
                    }
                }
            });
        }) as Box<dyn FnMut(MessageEvent)>)
    }

    pub fn node_id(&self) -> String {
        self.node_id.clone()
    }

    pub async fn serve(&mut self, handler: impl RequestHandler<Req, Resp> + 'static) {
        while let Some((req, tx)) = self.server_req_rx.next().await {
            let resp = handler.handle(req).await;
            tx.send(resp).unwrap();
        }
    }

    pub fn client(&self) -> Client<Req, Resp> {
        Client {
            status: self.status.clone(),
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
    type InFlightRequest<Resp> = (usize, oneshot::Sender<Result<Resp, Error>>);
    let mut in_flight: Vec<Option<InFlightRequest<Resp>>> =
        (0..MAX_IN_FLIGHT_REQUESTS).map(|_| None).collect();
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
                in_flight[idx % MAX_IN_FLIGHT_REQUESTS] = Some((idx, tx));

                let mut tx = timeout_tx.clone();
                spawn_local(async move {
                    sleep(Duration::from_millis(REQUEST_TIMEOUT_MILLIS)).await;
                    tx.send(idx).await.unwrap();
                });
            }
            res = resp_rx.next() => {
                let msg = res.expect("client response channel is closed");
                if let Message::Response { idx, resp } = msg {
                    let tx_opt = in_flight
                        .get_mut(idx % MAX_IN_FLIGHT_REQUESTS)
                        .expect("out of bounds for in-flight requests")
                        .take();
                    match tx_opt {
                        Some((i, tx)) if i == idx => {
                            // Best-effort reply (if the caller is gone then one
                            // can assume that the request has been canceled or
                            // something).
                            let _ = tx.send(resp);
                        }
                        Some((i, tx)) => {
                            console_log!(
                                "got unexpected response for leftover timed-out request (in-flight ID is {}, response ID is {})",
                                i,
                                idx,
                            );
                            in_flight[idx % MAX_IN_FLIGHT_REQUESTS] = Some((i, tx))

                        }
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

                if let Some((i, tx)) = tx_opt {
                    assert_eq!(i, idx, "unexpected response sender in timeout");
                    console_log!("request {} timed out", idx);
                    tx.send(Err(Error::RequestTimeout)).unwrap();
                }
            }
        }
        while min_req_idx < next_req_idx
            && in_flight[min_req_idx % MAX_IN_FLIGHT_REQUESTS].is_none()
        {
            min_req_idx += 1;
        }
    }
}

impl<Req, Resp> Client<Req, Resp> {
    pub async fn call(&self, req: Req) -> Result<Resp, Error> {
        if let Status::Closed = self.get_status() {
            return Err(Error::Disconnected);
        }
        let (resp_tx, resp_rx) = oneshot::channel();
        let mut tx = self.req_tx.clone();
        tx.send((req, resp_tx)).await.unwrap();

        resp_rx.await.unwrap()
    }

    pub fn get_status(&self) -> Status {
        let s = self.status.read().unwrap();
        s.clone()
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

#[derive(Debug, Serialize, Deserialize)]
pub enum Error {
    Js(String),
    RequestTimeout,
    TooManyInFlightRequests,
    Disconnected,
    Unavailable,
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
            Error::Disconnected => write!(f, "data channel has been disconnected"),
            Error::Unavailable => write!(f, "rpc server is unavailable"),
        }
    }
}
