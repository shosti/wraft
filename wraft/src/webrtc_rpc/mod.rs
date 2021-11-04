mod introduction;
pub mod transport;
use futures::channel::mpsc::Sender;
use transport::PeerTransport;

pub async fn introduce(id: String, session_id: String, peers_tx: Sender<PeerTransport>) {
    introduction::initiate(&id, &session_id, peers_tx)
        .await
        .unwrap();
}

pub mod error {
    use wasm_bindgen::JsValue;

    #[derive(Debug)]
    pub enum Error {
        Js(JsValue),
        Rust(Box<dyn std::error::Error>),
        AlreadyInitialized(String),
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
