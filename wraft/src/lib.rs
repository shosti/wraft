pub mod raft;
use yew::prelude::*;
use yew_router::prelude::*;
pub mod ringbuf;
pub mod util;
mod webrtc_rpc;
use wasm_bindgen::prelude::*;
pub mod init;
mod todo;

// Use `wee_alloc` as the global allocator.
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[derive(Switch, Debug, Clone)]
pub enum Route {
    #[to = "/todo"]
    Todo,
    #[to = "/"]
    Home,
}

pub struct Model {}

impl Component for Model {
    type Message = ();
    type Properties = ();

    fn create(_props: Self::Properties, _link: ComponentLink<Self>) -> Self {
        Self {}
    }

    fn update(&mut self, _msg: Self::Message) -> ShouldRender {
        false
    }

    fn change(&mut self, _props: Self::Properties) -> ShouldRender {
        false
    }

    fn view(&self) -> Html {
        html! {
            <Router<Route>
                render=Router::render(|switch| {
                    console_log!("SWITCH: {:#?}", switch);
                    match switch {
                        Route::Home => html! {
                            <>
                                <h1>{ "Try out WRaft!" }</h1>
                                <ul>
                                <li><RouterAnchor<Route> route=Route::Todo>{ "Todos" }</RouterAnchor<Route>></li>
                                </ul>
                                </>
                        },
                        Route::Todo => html! {
                            <todo::Model />
                        },
                    }
                })
                />
        }
    }
}

#[wasm_bindgen(start)]
pub async fn start() {
    util::set_panic_hook();

    yew::start_app::<Model>();
}
