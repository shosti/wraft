mod introduction;
use wasm_bindgen::prelude::*;

pub async fn introduce(
    id: String,
    session_id: String,
    initiate: bool,
) -> Result<(), JsValue> {
    if initiate {
        introduction::initiate(&id, &session_id).await
    } else {
        introduction::join(&id, &session_id).await
    }
}
