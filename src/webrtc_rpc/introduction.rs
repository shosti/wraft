use crate::console_log;
use futures::channel::mpsc::channel;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, WebSocket};
#[allow(dead_code, unused_imports)]
#[path = "../../target/flatbuffers/introduction_generated.rs"]
pub mod flatbuf;
use flatbuf::wraft::introduction::{Join, JoinArgs, Message, Command, MessageArgs};

static INTRODUCER: &str = "ws://localhost:9999";

pub async fn initiate(id: &str, session_id: &str) -> Result<(), JsValue> {
    let ws = WebSocket::new(INTRODUCER)?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    let onmessage_cb = Closure::wrap(Box::new(move |e: MessageEvent| {
        console_log!("E: {:#?}", e);
    }) as Box<dyn FnMut(MessageEvent)>);
    ws.set_onmessage(Some(onmessage_cb.as_ref().unchecked_ref()));

    let (opened_tx, mut opened_rx) = channel::<()>(1);
    let onopen_cb = Closure::wrap(Box::new(move || {
        let mut tx = opened_tx.clone();
        spawn_local(async move {
            tx.send(()).await.unwrap();
        });
    }) as Box<dyn FnMut()>);
    ws.set_onopen(Some(onopen_cb.as_ref().unchecked_ref()));

    match opened_rx.next().await {
        Some(()) => (),
        None => panic!("Thought this couldn't happen?"),
    }

    let mut builder = flatbuffers::FlatBufferBuilder::with_capacity(1024);
    let join_id = Some(builder.create_string(id));
    let join_session_id = Some(builder.create_string(session_id));
    let join = Join::create(&mut builder, &JoinArgs{
        id: join_id,
        session_id: join_session_id,
    });
    let message = Message::create(&mut builder, &MessageArgs{
        command_type: Command::Join,
        command: Some(join.as_union_value()),
    });
    builder.finish(message, None);
    let data = &builder.finished_data();
    ws.send_with_u8_array(data)?;
    Ok(())
}

pub async fn join(_id: &str, _session_id: &str) -> Result<(), JsValue> {
    console_log!("JOIN!");
    Ok(())
}
