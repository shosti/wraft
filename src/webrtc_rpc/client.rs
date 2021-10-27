use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::task::{Context, Poll};
use futures::{Sink, Stream};
use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, RtcDataChannel, RtcPeerConnection};

static CHANNEL_SIZE: usize = 1_000_000;

#[derive(Debug)]
pub struct Peer<Req, Resp> {
    pub node_id: String,
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    requests: Receiver<Req>,
    responses: Sender<Resp>,
    _message_cb: Closure<dyn FnMut(MessageEvent)>,
}

#[derive(Debug)]
pub struct Client<Req, Resp> {
    pub peers: HashMap<String, Peer<Req, Resp>>,
}

impl<Req, Resp> Peer<Req, Resp>
where
    Req: DeserializeOwned + 'static,
    Resp: Serialize + 'static,
{
    pub fn new(
        node_id: String,
        connection: RtcPeerConnection,
        data_channel: RtcDataChannel,
    ) -> Self {
        data_channel.set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);

        let (req_tx, requests) = channel(CHANNEL_SIZE);
        let cb = Closure::wrap(Box::new(move |ev: MessageEvent| {
            let tx = req_tx.clone();
            spawn_local(async move {
                let mut req_tx = tx.clone();
                let abuf = ev
                    .data()
                    .dyn_into::<js_sys::ArrayBuffer>()
                    .expect("Expected message in binary format");
                let data = js_sys::Uint8Array::new(&abuf).to_vec();

                let req = bincode::deserialize::<Req>(&data).unwrap();
                req_tx.send(req).await.unwrap();
            });
        }) as Box<dyn FnMut(MessageEvent)>);
        data_channel.set_onmessage(Some(cb.as_ref().unchecked_ref()));

        let send_dc = data_channel.clone();
        let (responses, mut resp_rx) = channel::<Resp>(CHANNEL_SIZE);
        spawn_local(async move {
            while let Some(r) = resp_rx.next().await {
                let data = bincode::serialize(&r).unwrap();
                // TODO: error handling
                send_dc.send_with_u8_array(&data).unwrap();
            }
        });

        Self {
            node_id,
            connection,
            data_channel,
            requests,
            responses,
            _message_cb: cb,
        }
    }
}

impl<Req, Resp> Stream for Peer<Req, Resp> {
    type Item = Result<Req, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.requests.poll_next_unpin(cx).map(|r| r.map(Ok))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.requests.size_hint()
    }
}

impl<Req, Resp> Sink<Resp> for Peer<Req, Resp> {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.responses
            .poll_ready_unpin(cx)
            .map_err(|e| Error::String(format!("{:?}", e)))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Resp) -> Result<(), Self::Error> {
        self.responses
            .start_send_unpin(item)
            .map_err(|e| Error::String(format!("{:?}", e)))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.responses
            .poll_flush_unpin(cx)
            .map_err(|e| Error::String(format!("{:?}", e)))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.responses
            .poll_close_unpin(cx)
            .map_err(|e| Error::String(format!("{:?}", e)))
    }
}

impl<Req, Resp> Drop for Peer<Req, Resp> {
    fn drop(&mut self) {
        self.data_channel.set_onmessage(None);
    }
}

impl<Req, Resp> Client<Req, Resp>
where
    Req: DeserializeOwned + 'static,
    Resp: Serialize + 'static,
{
    pub fn new(peers: HashMap<String, Peer<Req, Resp>>) -> Self {
        Self { peers }
    }
}

impl<Req, Resp> Unpin for Peer<Req, Resp> {
}

#[derive(Debug, Deserialize)]
pub enum Error {
    String(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            Error::String(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for Error {}
