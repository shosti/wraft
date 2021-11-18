pub mod raft;
pub mod raft_init;
pub mod todo_state;

use todo::Todo;
use yew::prelude::*;
use yew_router::prelude::*;
pub mod ringbuf;
pub mod util;
mod webrtc_rpc;
use wasm_bindgen::prelude::*;
mod benchmark;
mod todo;
use benchmark::Benchmark;

// Use `wee_alloc` as the global allocator.
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[derive(Switch, Debug, Clone)]
pub enum Route {
    #[to = "/todo"]
    Todo,
    #[to = "/bench"]
    Benchmark,
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
                    match switch {
                        Route::Home => html! {
                            <>
                                <h1>{ "Try out WRaft!" }</h1>
                                <ul>
                                <li><RouterAnchor<Route> route=Route::Todo>{ "Todos" }</RouterAnchor<Route>></li>
                                <li><RouterAnchor<Route> route=Route::Benchmark>{ "Benchmark" }</RouterAnchor<Route>></li>
                                </ul>
                                </>
                        },
                        Route::Todo => html! {
                            <Todo session_key=get_session_key() />
                        },
                        Route::Benchmark => html! {
                            <Benchmark session_key=get_session_key() />
                        },
                    }
                })
                />
        }
    }
}

fn get_session_key() -> Option<u128> {
    let hash = web_sys::window()
        .expect("no global window")
        .location()
        .hash()
        .ok()?;
    u128::from_str_radix(hash.strip_prefix('#')?, 16).ok()
}

#[wasm_bindgen(start)]
pub fn start() {
    util::set_panic_hook();

    yew::start_app::<Model>();
}
