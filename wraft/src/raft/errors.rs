use crate::webrtc_rpc::transport;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::Event;

#[derive(Debug)]
pub enum Error {
    Js(String),
    Transport(transport::Error),
    NotEnoughPeers,
    Persistence(serde_json::Error),
    NotLeader,
    CommandTimeout,
}

#[derive(Debug)]
pub enum ClientError {}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Persistence(err)
    }
}

impl From<JsValue> for Error {
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
                    format!("JS error: {:?}", err)
                }
            }
        };
        Error::Js(msg)
    }
}
