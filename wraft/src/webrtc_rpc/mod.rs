pub mod client;
mod introduction;
use client::Peer;
use futures::channel::mpsc::Sender;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;

pub async fn introduce<Req, Resp>(
    id: &str,
    session_id: &str,
    peers: Sender<Peer<Req, Resp>>,
) -> Result<(), error::Error>
where
    Req: DeserializeOwned + Debug + 'static,
    Resp: Serialize + Debug + 'static,
{
    let client = introduction::initiate::<Req, Resp>(id, session_id, peers).await?;
    Ok(client)
}

pub mod error {
    use wasm_bindgen::JsValue;

    #[derive(Debug)]
    pub enum Error {
        Js(JsValue),
        Rust(Box<dyn std::error::Error>),
        String(String),
    }

    impl From<JsValue> for Error {
        fn from(js_val: JsValue) -> Self {
            Error::Js(js_val)
        }
    }

    impl From<Box<bincode::ErrorKind>> for Error {
        fn from(err: Box<bincode::ErrorKind>) -> Self {
            Error::Rust(err)
        }
    }

    impl From<futures::channel::mpsc::SendError> for Error {
        fn from(err: futures::channel::mpsc::SendError) -> Self {
            Error::Rust(Box::new(err))
        }
    }
}
