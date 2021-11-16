use crate::console_log;
use crate::raft::client::Client;
use crate::raft::Raft;
use crate::raft_init::{self, RaftProps};
use crate::todo_state::{self, Entry, Filter, State};
use crate::util::sleep;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use strum::IntoEnumIterator;
use wasm_bindgen_futures::spawn_local;
use yew::web_sys::HtmlInputElement as InputElement;
use yew::{
    classes, html, Callback, Component, ComponentLink, Html, InputData, NodeRef, ShouldRender,
};
use yew::{events::KeyboardEvent, Classes};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Msg {
    StateUpdate,
    GotState(State),
    Focus,
}

pub struct Model {
    link: ComponentLink<Self>,
    focus_ref: NodeRef,
    raft_client: Client<State>,
    state: State,
}

pub type Todo = raft_init::Model<Model, State>;

impl Component for Model {
    type Message = Msg;
    type Properties = RaftProps<State>;

    fn create(props: Self::Properties, link: ComponentLink<Self>) -> Self {
        let focus_ref = NodeRef::default();
        let raft = props.raft.take().unwrap();
        let raft_client = raft.client();

        Self::run_raft(raft, link.clone());
        Self {
            link,
            focus_ref,
            raft_client,
            state: State::default(),
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            Msg::Focus => {
                if let Some(input) = self.focus_ref.cast::<InputElement>() {
                    input.focus().unwrap();
                    true
                } else {
                    false
                }
            }
            Msg::StateUpdate => {
                let link = self.link.clone();
                let raft_client = self.raft_client.clone();
                spawn_local(async move {
                    if let Ok(Some(new_state)) = raft_client.get(()).await {
                        link.send_message(Msg::GotState(new_state))
                    }
                });
                false
            }
            Msg::GotState(state) => {
                self.state = state;
                true
            }
        }
    }

    fn change(&mut self, _: Self::Properties) -> ShouldRender {
        false
    }

    fn view(&self) -> Html {
        let hidden_class = if self.state.entries.is_empty() {
            "hidden"
        } else {
            ""
        };
        html! {
            <>
                <link
                rel="stylesheet"
                href="https://cdn.jsdelivr.net/npm/todomvc-common@1.0.5/base.css"
                />
                <link
                rel="stylesheet"
                href="https://cdn.jsdelivr.net/npm/todomvc-app-css@2.3.0/index.css"
                />

            <div class="todomvc-wrapper">
                <section class="todoapp">
                    <header class="header">
                        <h1>{ "todos" }</h1>
                        { self.view_input() }
                    </header>
                    <section class=classes!("main", hidden_class)>
                        <input
                            type="checkbox"
                            class="toggle-all"
                            id="toggle-all"
                            checked=self.state.is_all_completed()
                            onclick=self.raft_callback(|_| todo_state::Msg::ToggleAll)
                        />
                        <label for="toggle-all" />
                        <ul class="todo-list">
                            { for self.state.entries.iter().filter(|e| self.state.filter.fits(e)).enumerate().map(|e| self.view_entry(e)) }
                        </ul>
                    </section>
                    <footer class=classes!("footer", hidden_class)>
                        <span class="todo-count">
                            <strong>{ self.state.total() }</strong>
                            { " item(s) left" }
                        </span>
                        <ul class="filters">
                            { for Filter::iter().map(|flt| self.view_filter(flt)) }
                        </ul>
                        <button class="clear-completed" onclick=self.raft_callback(|_| todo_state::Msg::ClearCompleted)>
                            { format!("Clear completed ({})", self.state.total_completed()) }
                        </button>
                    </footer>
                </section>
                <footer class="info">
                    <p>{ "Double-click to edit a todo" }</p>
                    <p>{ "Written by " }<a href="https://github.com/shosti/" target="_blank">{ "Emanuel Evans" }</a></p>
                    <p>{ "Adapted from TodoMVC by " }<a href="https://github.com/DenisKolodin/" target="_blank">{ "Denis Kolodin" }</a></p>
                </footer>
            </div>
                </>
        }
    }
}

impl Model {
    fn run_raft(mut raft: Raft<State>, link: ComponentLink<Self>) {
        spawn_local(async move {
            while let Some(()) = raft.next().await {
                link.send_message(Msg::StateUpdate);
            }
        });
    }

    pub fn raft_callback<F, IN>(&self, function: F) -> Callback<IN>
    where
        F: Fn(IN) -> todo_state::Msg + 'static,
    {
        let raft_client = self.raft_client.clone();
        let closure = move |input| {
            let output = function(input);
            let r = raft_client.clone();
            spawn_local(async move {
                loop {
                    match r.send(output.clone()).await {
                        Ok(()) => return,
                        Err(err) => {
                            console_log!("err: {:?}, retrying...", err);
                            sleep(Duration::from_millis(500)).await;
                        }
                    }
                }
            })
        };
        closure.into()
    }

    pub fn raft_batch_callback<F, IN>(&self, function: F) -> Callback<IN>
    where
        F: Fn(IN) -> Option<todo_state::Msg> + 'static,
    {
        let raft_client = self.raft_client.clone();
        let closure = move |input| {
            if let Some(output) = function(input) {
                let r = raft_client.clone();
                spawn_local(async move {
                    loop {
                        match r.send(output.clone()).await {
                            Ok(()) => return,
                            Err(err) => {
                                console_log!("err: {:?}, retrying...", err);
                                sleep(Duration::from_millis(500)).await;
                            }
                        }
                    }
                });
            }
        };
        closure.into()
    }

    fn view_filter(&self, filter: Filter) -> Html {
        let cls = if self.state.filter == filter {
            "selected"
        } else {
            "not-selected"
        };
        html! {
            <li>
                <a class=cls
                   href=filter.as_href()
                   onclick=self.raft_callback(move |_| todo_state::Msg::SetFilter(filter))
                >
                    { filter }
                </a>
            </li>
        }
    }

    fn view_input(&self) -> Html {
        html! {
            // You can use standard Rust comments. One line:
            // <li></li>
            <input
                class="new-todo"
                placeholder="What needs to be done?"
                value=self.state.value.clone()
                oninput=self.raft_callback(|e: InputData| todo_state::Msg::Update(e.value))
                onkeypress=self.raft_batch_callback(|e: KeyboardEvent| {
                    if e.key() == "Enter" { Some(todo_state::Msg::Add) } else { None }
                })
            />
            /* Or multiline:
            <ul>
                <li></li>
            </ul>
            */
        }
    }

    fn view_entry(&self, (idx, entry): (usize, &Entry)) -> Html {
        let mut class = Classes::from("todo");
        if entry.editing {
            class.push(" editing");
        }
        if entry.completed {
            class.push(" completed");
        }
        html! {
            <li class=class>
                <div class="view">
                    <input
                        type="checkbox"
                        class="toggle"
                        checked=entry.completed
                        onclick=self.raft_callback(move |_| todo_state::Msg::Toggle(idx))
                    />
                    <label ondblclick=self.raft_callback(move |_| todo_state::Msg::ToggleEdit(idx))>{ &entry.description }</label>
                    <button class="destroy" onclick=self.raft_callback(move |_| todo_state::Msg::Remove(idx)) />
                </div>
                { self.view_entry_edit_input((idx, entry)) }
            </li>
        }
    }

    fn view_entry_edit_input(&self, (idx, entry): (usize, &Entry)) -> Html {
        if entry.editing {
            html! {
                <input
                    class="edit"
                    type="text"
                    ref=self.focus_ref.clone()
                    value=self.state.edit_value.clone()
                    onmouseover=self.link.callback(|_| Msg::Focus)
                    oninput=self.raft_callback(|e: InputData| todo_state::Msg::UpdateEdit(e.value))
                    onblur=self.raft_callback(move |_| todo_state::Msg::Edit(idx))
                    onkeypress=self.raft_batch_callback(move |e: KeyboardEvent| {
                        if e.key() == "Enter" { Some(todo_state::Msg::Edit(idx)) } else { None }
                    })
                />
            }
        } else {
            html! { <input type="hidden" /> }
        }
    }
}
