use futures::channel::mpsc::{channel, Receiver};
use futures::SinkExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tarpc::Request;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, RtcDataChannel, RtcPeerConnection};

#[derive(Debug, Clone)]
pub struct Peer<T> {
    pub node_id: String,
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    recv: Arc<Receiver<Request<T>>>,
    _message_cb: Arc<Closure<dyn FnMut(MessageEvent)>>,
}

#[derive(Debug)]
pub struct Client<T> {
    peers: HashMap<String, Peer<T>>,
}

impl<T> Peer<T>
where
    T: Clone + Serialize + DeserializeOwned + 'static,
{
    pub fn new(
        node_id: String,
        connection: RtcPeerConnection,
        data_channel: RtcDataChannel,
    ) -> Self {
        let (req_tx, req_rx) = channel(1000);

        let tx_clone = req_tx.clone();
        let cb = Arc::new(Closure::wrap(Box::new(move |ev: MessageEvent| {
            let tx = tx_clone.clone();
            spawn_local(async move {
                let mut req_tx = tx.clone();
                let abuf = ev
                    .data()
                    .dyn_into::<js_sys::ArrayBuffer>()
                    .expect("Expected message in binary format");
                let data = js_sys::Uint8Array::new(&abuf).to_vec();

                // TODO: error handling
                let message = bincode::deserialize(&data).unwrap();
                let req = Request::from(message);
                req_tx.send(req.clone()).await.unwrap();
            });
        }) as Box<dyn FnMut(MessageEvent)>));
        data_channel.set_onmessage(Some(cb.as_ref().as_ref().unchecked_ref()));

        Self {
            node_id,
            connection,
            data_channel,
            recv: Arc::new(req_rx),
            _message_cb: cb,
        }
    }
}

impl<T> Drop for Peer<T> {
    fn drop(&mut self) {
        self.data_channel.set_onmessage(None);
    }
}

impl<T> Client<T>
where
    T: Clone + Serialize + DeserializeOwned,
{
    pub fn new(peers: HashMap<String, Peer<T>>) -> Self {
        Self { peers }
    }
}
