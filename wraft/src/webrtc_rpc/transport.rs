use crate::console_log;
use crate::ringbuf::{self, RingBuf};
use crate::util::sleep;
use async_trait::async_trait;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::{select, SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, RtcDataChannel, RtcPeerConnection};

const CHANNEL_SIZE: usize = 1024;
const MAX_IN_FLIGHT_REQUESTS: usize = 1024;
const REQUEST_TIMEOUT_MILLIS: u64 = 500;

type RequestMessage<Req, Resp> = (Req, oneshot::Sender<Result<Resp, Error>>);

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

pub struct PeerTransport {
    node_id: u64,
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    done: oneshot::Sender<()>,
}

#[derive(Debug)]
pub struct Server<Req, Resp> {
    node_id: u64,
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    server_req_rx: Receiver<RequestMessage<Req, Resp>>,
    done: Option<oneshot::Sender<()>>,

    // Keep references to JS closures for memory-management purposes
    _onmessage_cb: Option<Closure<dyn FnMut(MessageEvent)>>,
    _onclose_cb: Option<Closure<dyn FnMut()>>,
}

#[derive(Debug)]
pub struct Client<Req, Resp> {
    node_id: u64,
    connected: Arc<AtomicBool>,
    req_tx: Sender<RequestMessage<Req, Resp>>,
}

impl<Req, Resp> Clone for Client<Req, Resp> {
    fn clone(&self) -> Self {
        Self {
            node_id: self.node_id,
            connected: self.connected.clone(),
            req_tx: self.req_tx.clone(),
        }
    }
}

struct RequestManager<Req, Resp> {
    dc: RtcDataChannel,
    in_flight: RingBuf<oneshot::Sender<Result<Resp, Error>>>,
    timeout_tx: Sender<usize>,
    _request: PhantomData<Req>,
}

#[async_trait]
pub trait RequestHandler<Req, Resp> {
    async fn handle(&self, req: Req) -> Result<Resp, Error>;
}

impl PeerTransport {
    pub fn new(
        node_id: u64,
        connection: RtcPeerConnection,
        data_channel: RtcDataChannel,
        done: oneshot::Sender<()>,
    ) -> Self {
        Self {
            node_id,
            done,
            connection,
            data_channel,
        }
    }

    pub fn start<Req, Resp>(self) -> (Client<Req, Resp>, Server<Req, Resp>)
    where
        Req: Serialize + DeserializeOwned + Debug + 'static,
        Resp: Serialize + DeserializeOwned + Debug + 'static,
    {
        self.data_channel
            .set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);

        let (client_req_tx, client_req_rx) = channel(CHANNEL_SIZE);
        let (client_resp_tx, client_resp_rx) = channel(CHANNEL_SIZE);
        let (server_req_tx, server_req_rx) = channel(CHANNEL_SIZE);

        spawn_local(handle_client_requests(
            client_req_rx,
            client_resp_rx,
            self.data_channel.clone(),
        ));

        let mut server = Server {
            server_req_rx,
            node_id: self.node_id,
            data_channel: self.data_channel,
            connection: self.connection,
            done: Some(self.done),
            _onmessage_cb: None,
            _onclose_cb: None,
        };

        server.set_onclose_callback(
            client_req_tx.clone(),
            client_resp_tx.clone(),
            server_req_tx.clone(),
        );
        server.set_onmessage_callback(client_resp_tx, server_req_tx);

        let client = Client {
            connected: Arc::new(true.into()),
            node_id: self.node_id,
            req_tx: client_req_tx,
        };

        (client, server)
    }

    pub fn node_id(&self) -> u64 {
        self.node_id
    }
}

impl<Req, Resp> Server<Req, Resp>
where
    Req: Serialize + DeserializeOwned + Debug + 'static,
    Resp: Serialize + DeserializeOwned + Debug + 'static,
{
    pub async fn serve(&mut self, handler: impl RequestHandler<Req, Resp> + 'static) {
        while let Some((req, tx)) = self.server_req_rx.next().await {
            let resp = handler.handle(req).await;
            if tx.send(resp).is_err() {
                // Server is down, so we're done serving requests
                break;
            }
        }
    }

    fn set_onmessage_callback(
        &mut self,
        client_resp_tx: Sender<Message<Req, Resp>>,
        server_req_tx: Sender<RequestMessage<Req, Resp>>,
    ) {
        let data_channel = self.data_channel.clone();

        let cb = Closure::wrap(Box::new(move |ev: MessageEvent| {
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
                        if server_tx.send((req, tx)).await.is_err() {
                            console_log!("server request channel closed");
                            return;
                        }
                        if let Ok(resp) = rx.await {
                            let msg: Message<Req, Resp> = Message::Response { idx, resp };
                            let data = bincode::serialize(&msg).unwrap();
                            if let Err(err) = dc.send_with_u8_array(&data) {
                                console_log!("error sending data: {:?}", err);
                            }
                        }
                    }
                    msg @ Message::Response { idx: _, resp: _ } => {
                        // Got a response to one of our requests, try to process
                        // it on our end
                        let _ = client_tx.send(msg).await;
                    }
                }
            });
        }) as Box<dyn FnMut(MessageEvent)>);

        self.data_channel
            .set_onmessage(Some(cb.as_ref().unchecked_ref()));

        self._onmessage_cb = Some(cb);
    }

    fn set_onclose_callback(
        &mut self,
        mut client_req_tx: Sender<RequestMessage<Req, Resp>>,
        mut client_resp_tx: Sender<Message<Req, Resp>>,
        mut server_req_tx: Sender<RequestMessage<Req, Resp>>,
    ) {
        let node_id = self.node_id;

        let cb = Closure::once(move || {
            console_log!("lost data channel for {}", node_id);

            // Close channels and hope all the listeners clean up nicely after
            // themselves :)
            client_req_tx.close_channel();
            client_resp_tx.close_channel();
            server_req_tx.close_channel();
        });

        self.data_channel
            .set_onclose(Some(cb.as_ref().unchecked_ref()));

        self._onclose_cb = Some(cb);
    }
}

impl<Req, Resp> Drop for Server<Req, Resp> {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
        self.connection.close();
    }
}

async fn handle_client_requests<Req, Resp>(
    req_rx: Receiver<(Req, oneshot::Sender<Result<Resp, Error>>)>,
    resp_rx: Receiver<Message<Req, Resp>>,
    dc: RtcDataChannel,
) where
    Req: Serialize + 'static,
    Resp: Serialize + 'static,
{
    let (timeout_tx, timeout_rx) = channel::<usize>(MAX_IN_FLIGHT_REQUESTS);
    let m = RequestManager {
        in_flight: RingBuf::with_capacity(MAX_IN_FLIGHT_REQUESTS),
        dc,
        timeout_tx,
        _request: PhantomData,
    };

    m.run(req_rx, resp_rx, timeout_rx).await;
}

impl<Req, Resp> RequestManager<Req, Resp>
where
    Req: Serialize + 'static,
    Resp: Serialize + 'static,
{
    pub async fn run(
        mut self,
        mut req_rx: Receiver<RequestMessage<Req, Resp>>,
        mut resp_rx: Receiver<Message<Req, Resp>>,
        mut timeout_rx: Receiver<usize>,
    ) where
        Req: Serialize + 'static,
        Resp: Serialize + 'static,
    {
        loop {
            select! {
                res = req_rx.next() => {
                    match res {
                        Some((req, tx)) => self.handle_request(req, tx),
                        None => {
                            console_log!("request channel closed, stopping request manager");
                            return;
                        }
                    }
                }
                res = resp_rx.next() => {
                    match res {
                        Some(msg) => self.handle_response(msg),
                        None => {
                            console_log!("response channel closed, stopping request manager");
                            return;
                        }
                    }
                }
                res = timeout_rx.next() => {
                    let idx = res.unwrap();
                    self.handle_timeout(idx);
                }
            }
        }
    }

    fn handle_request(&mut self, req: Req, tx: oneshot::Sender<Result<Resp, Error>>) {
        match self.in_flight.add(tx) {
            Ok(idx) => {
                let msg: Message<Req, Resp> = Message::Request { idx, req };
                let data = bincode::serialize(&msg).unwrap();
                if let Err(err) = self.dc.send_with_u8_array(&data) {
                    let tx = self.in_flight.remove(idx).unwrap();
                    let _ = tx.send(Err(Error::from(err)));
                    return;
                }
                let mut timeout_tx = self.timeout_tx.clone();
                spawn_local(async move {
                    sleep(Duration::from_millis(REQUEST_TIMEOUT_MILLIS)).await;
                    let _ = timeout_tx.send(idx).await;
                });
            }
            Err(ringbuf::Error::Overflow(tx)) => {
                let _ = tx.send(Err(Error::TooManyInFlightRequests));
            }
        }
    }

    fn handle_response(&mut self, msg: Message<Req, Resp>) {
        if let Message::Response { idx, resp } = msg {
            match self.in_flight.remove(idx) {
                Some(tx) => {
                    // Best-effort reply (if the caller is gone then one
                    // can assume that the request has been canceled or
                    // something).
                    let _ = tx.send(resp);
                }
                None => {
                    console_log!("request {} came in after request canceled", idx)
                }
            }
        }
    }

    fn handle_timeout(&mut self, idx: usize) {
        if let Some(tx) = self.in_flight.remove(idx) {
            console_log!("request {} timed out", idx);
            let _ = tx.send(Err(Error::RequestTimeout));
        }
    }
}

impl<Req, Resp> Client<Req, Resp> {
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub async fn call(&mut self, req: Req) -> Result<Resp, Error> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        match self.req_tx.send((req, resp_tx)).await {
            Ok(_) => resp_rx.await.map_err(|_| Error::Disconnected)?,
            Err(_) => {
                self.connected.store(false, Ordering::SeqCst);
                Err(Error::Disconnected)
            }
        }
    }
}

impl Debug for PeerTransport {
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
