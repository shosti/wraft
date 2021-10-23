use crate::console_log;

// #[wasm_bindgen]
// extern "C" {
//     #[wasm_bindgen(js_namespace = console)]
//     fn log(s: &str);
// }

// macro_rules! console_log {
//     ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
// }

pub async fn introduce(id: String, session_id: String, initiate: bool) {
    console_log!("INTRODUCE");
    console_log!("ID: {:#?}", id);
    console_log!("SESSION_ID: {:#?}", session_id);
    console_log!("INITIATE: {:#?}", initiate);
}
