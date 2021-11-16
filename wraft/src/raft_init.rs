use crate::raft::{self, Raft};
use rand::{thread_rng, Rng};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Storage, Window};
use yew::prelude::*;
use yew::Properties;

const CLUSTER_SIZE: usize = 3;

pub struct RaftWrapper<S: raft::State>(Arc<Mutex<Option<Raft<S>>>>);

impl<S: raft::State> RaftWrapper<S> {
    pub fn new(raft: Raft<S>) -> Self {
        Self(Arc::new(Mutex::new(Some(raft))))
    }

    pub fn take(&self) -> Option<Raft<S>> {
        let mut r = self.0.lock().unwrap();
        r.take()
    }
}

impl<S: raft::State> Clone for RaftWrapper<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[derive(Properties, Clone)]
pub struct Props {
    pub session_key: Option<u128>,
}

#[derive(Properties)]
pub struct RaftProps<S: raft::State> {
    pub raft: RaftWrapper<S>,
}

impl<S: raft::State> Clone for RaftProps<S> {
    fn clone(&self) -> Self {
        Self {
            raft: self.raft.clone(),
        }
    }
}

pub enum Msg<S: raft::State> {
    UpdateSessionKey(String),
    StartCluster,
    JoinCluster,
    ClearStorage,
    ClusterStarted(RaftWrapper<S>),
}

enum State<S: raft::State> {
    Setup,
    Waiting(u128),
    Running(RaftWrapper<S>),
}

pub struct Model<C: Component<Properties = RaftProps<S>>, S: raft::State>
where
    S: raft::State + Clone,
{
    link: ComponentLink<Self>,
    session_key: String,
    state: State<S>,
    _component: PhantomData<C>,
    _message: PhantomData<S>,
}

impl<C: Component<Properties = RaftProps<S>>, S> Component for Model<C, S>
where
    S: raft::State + Clone,
{
    type Message = Msg<S>;
    type Properties = Props;

    fn create(props: Self::Properties, link: ComponentLink<Self>) -> Self {
        if let Some(session_key) = props.session_key {
            Self::start_raft(session_key, link.clone());
            Self {
                link,
                state: State::Waiting(session_key),
                session_key: "".into(),
                _component: PhantomData,
                _message: PhantomData,
            }
        } else {
            Self {
                link,
                state: State::Setup,
                session_key: "".into(),
                _component: PhantomData,
                _message: PhantomData,
            }
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            Msg::UpdateSessionKey(key) => {
                self.session_key = key;
                true
            }
            Msg::StartCluster => {
                let session_key = generate_session_key();
                Self::start_raft(session_key, self.link.clone());
                self.state = State::Waiting(session_key);
                true
            }
            Msg::JoinCluster => match u128::from_str_radix(&self.session_key, 16) {
                Ok(session_key) => {
                    Self::start_raft(session_key, self.link.clone());
                    self.state = State::Waiting(session_key);
                    true
                }
                Err(_) => {
                    web_sys::window()
                        .unwrap()
                        .alert_with_message("Invalid session key!")
                        .unwrap();
                    false
                }
            },
            Msg::ClearStorage => {
                local_storage().clear().unwrap();
                window()
                    .alert_with_message("Storage cleared successfully!")
                    .unwrap();
                true
            }
            Msg::ClusterStarted(raft) => {
                self.state = State::Running(raft);
                true
            }
        }
    }

    fn change(&mut self, _props: Self::Properties) -> ShouldRender {
        false
    }

    fn view(&self) -> Html {
        match &self.state {
            State::Setup => self.render_setup(),
            State::Waiting(session_key) => self.render_waiting(*session_key),
            State::Running(raft) => self.render_running(raft.clone()),
        }
    }
}

impl<C: Component<Properties = RaftProps<S>>, S> Model<C, S>
where
    S: raft::State + Clone,
{
    fn render_setup(&self) -> Html {
        let start = self.link.callback(|_| Msg::StartCluster);
        let session_key = self.session_key.clone();
        let local_storage_items = local_storage().length().unwrap();
        html! {
            <>
                <h1>{ "Startup" }</h1>
                <p>
                <button type="button" onclick=start>{ "Start new cluster" }</button>
                </p>
                <p>
                <label for="join-session-key">{ "Join existing cluster: " }</label>
                <input
                type="text"
                value=session_key
                name="join-session-key"
                placeholder="Cluster ID"
                oninput=self.link.callback(|e: InputData| Msg::UpdateSessionKey(e.value))
                onkeypress=self.link.batch_callback(move |e: KeyboardEvent| {
                    if e.key() == "Enter" { Some(Msg::JoinCluster) } else { None }
                })
                />
                </p>
                <p>
                <div>
                <button type="button" onclick=self.link.callback(|_| Msg::ClearStorage)>{
                    format!("Clear local storage (currently {} item(s))", local_storage_items)
                }</button>
                </div>
                </p>
                </>
        }
    }

    fn render_waiting(&self, session_key: u128) -> Html {
        html! {
            <>
            <h1>{ "Waiting for cluster to start..." }</h1>
                <h3>{ format!("Cluster ID is {:032x}", session_key) }</h3>
                <p>
            { other_cluster_members(session_key) }
                </p>
                </>
        }
    }

    fn render_running(&self, raft: RaftWrapper<S>) -> Html {
        html! { <C raft=raft /> }
    }

    fn start_raft(session_key: u128, link: ComponentLink<Self>) {
        spawn_local(async move {
            let hostname = hostname();
            let raft = Raft::start(&hostname, session_key, CLUSTER_SIZE)
                .await
                .unwrap();
            link.send_message(Msg::ClusterStarted(RaftWrapper::new(raft)));
        });
    }
}

fn generate_session_key() -> u128 {
    thread_rng().gen()
}

fn hostname() -> String {
    window().location().hostname().unwrap()
}

fn local_storage() -> Storage {
    window().local_storage().unwrap().unwrap()
}

fn window() -> Window {
    web_sys::window().unwrap()
}

fn other_cluster_members(session_key: u128) -> Vec<Html> {
    let all_targets = vec!["wraft0", "wraft1", "wraft2"];
    let hostname = hostname();
    let (me, domain) = hostname.split_once('.').unwrap_or((&hostname, ""));
    let path = window().location().pathname().unwrap();
    all_targets.iter().filter(|&t| t != &me).map(|t| {
        let url = format!("https://{}.{}{}#{:032x}", t, domain, path, session_key);
        html! {
            <a href=url target="_blank">
                <button type="button">{ format!("Open {}", t) }</button>
                </a>
        }
    }).collect()
}
