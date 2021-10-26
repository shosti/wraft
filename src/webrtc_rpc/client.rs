use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::task::{Context, Poll};
use futures::{Sink, Stream};
use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::pin::Pin;
use tarpc::{Request, Response};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, RtcDataChannel, RtcPeerConnection};

static CHANNEL_SIZE: usize = 1_000_000;

#[derive(Debug)]
pub struct Peer<T> {
    pub node_id: String,
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    requests: Receiver<Request<T>>,
    responses: Sender<Response<T>>,
    _message_cb: Closure<dyn FnMut(MessageEvent)>,
}

#[derive(Debug)]
pub struct Client<T> {
    peers: HashMap<String, Peer<T>>,
}

impl<T> Peer<T>
where
    T: Serialize + DeserializeOwned + 'static,
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

                let req = bincode::deserialize::<Request<T>>(&data).unwrap();
                req_tx.send(req).await.unwrap();
            });
        }) as Box<dyn FnMut(MessageEvent)>);
        data_channel.set_onmessage(Some(cb.as_ref().unchecked_ref()));

        let send_dc = data_channel.clone();
        let (responses, mut resp_rx) = channel::<Response<T>>(CHANNEL_SIZE);
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

impl<T> Stream for Peer<T> {
    type Item = Request<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.requests.poll_next_unpin(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.requests.size_hint()
    }
}

impl<T> Sink<Response<T>> for Peer<T> {
    type Error = <Sender<Response<T>> as Sink<Response<T>>>::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.responses.poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Response<T>) -> Result<(), Self::Error> {
        self.responses.start_send_unpin(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.responses.poll_flush_unpin(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.responses.poll_close_unpin(cx)
    }
}

impl<T> Drop for Peer<T> {
    fn drop(&mut self) {
        self.data_channel.set_onmessage(None);
    }
}

impl<T> Client<T>
where
    T: Serialize + DeserializeOwned,
{
    pub fn new(peers: HashMap<String, Peer<T>>) -> Self {
        Self { peers }
    }
}
