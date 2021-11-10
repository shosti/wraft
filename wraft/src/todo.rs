use crate::init::{ClusterInit, ClusterWaiting};
use crate::raft::{LogCmd, Raft};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

const CLUSTER_SIZE: usize = 3;

pub struct Model {
    state: State,
    link: ComponentLink<Self>,
}

enum State {
    Setup,
    Waiting { session_key: u128 },
    Running { todos: TodoList, new_todo: String },
}

pub enum Msg {
    UpdateNewTodo(String),
    StartCluster(u128),
    ClusterStarted(TodoList),
    NewTodo,
    TodosUpdate(LogCmd<TodoItem>),
    ToggleTodo(String),
    DeleteTodo(String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum TodoState {
    Todo,
    Done,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TodoItem {
    text: String,
    state: TodoState,
}

pub struct TodoList {
    raft: Raft<TodoItem>,
    todos: BTreeMap<String, TodoItem>,
    link: ComponentLink<Model>,
}

impl Component for Model {
    type Message = Msg;
    type Properties = ();

    fn create(_props: Self::Properties, link: ComponentLink<Self>) -> Self {
        Self {
            link,
            state: State::Setup,
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            Msg::StartCluster(session_key) => {
                self.start_raft(session_key);
                self.state = State::Waiting { session_key };
                true
            }
            Msg::ClusterStarted(todos) => {
                self.state = State::Running {
                    todos,
                    new_todo: "".into(),
                };
                true
            }
            Msg::TodosUpdate(cmd) => {
                if let State::Running { todos, .. } = &mut self.state {
                    todos.update(cmd);
                    return true;
                }
                unreachable!();
            }
            Msg::ToggleTodo(todo_text) => {
                if let State::Running { todos, .. } = &mut self.state {
                    todos.toggle(todo_text);
                    return true;
                }
                unreachable!();
            }
            Msg::UpdateNewTodo(todo_text) => match &mut self.state {
                State::Running { new_todo, .. } => {
                    *new_todo = todo_text;
                    true
                }
                _ => unreachable!(),
            },
            Msg::NewTodo => match &mut self.state {
                State::Running { todos, new_todo } => {
                    todos.add(new_todo.to_string());
                    *new_todo = "".into();
                    true
                }
                _ => unreachable!(),
            },
            Msg::DeleteTodo(todo_text) => {
                if let State::Running { todos, .. } = &mut self.state {
                    todos.delete(todo_text);
                    return true;
                }
                unreachable!();
            }
        }
    }

    fn change(&mut self, _props: Self::Properties) -> ShouldRender {
        false
    }

    fn view(&self) -> Html {
        match &self.state {
            State::Setup => self.render_setup(),
            State::Waiting { session_key } => self.render_waiting(*session_key),
            State::Running { todos, new_todo } => todos.render(new_todo.clone()),
        }
    }
}

impl Model {
    fn render_setup(&self) -> Html {
        let onstart = self.link.callback(Msg::StartCluster);
        html! {
            <ClusterInit onstart=onstart />
        }
    }

    fn render_waiting(&self, session_key: u128) -> Html {
        html! {
            <ClusterWaiting session_key=session_key />
        }
    }

    fn hostname() -> String {
        web_sys::window()
            .expect("no global window")
            .location()
            .hostname()
            .unwrap()
    }

    fn start_raft(&self, session_id: u128) {
        let link = self.link.clone();
        spawn_local(async move {
            let hostname = Self::hostname();
            let raft = Raft::start(&hostname, session_id, CLUSTER_SIZE)
                .await
                .unwrap();
            let todos = TodoList::new(raft, link.clone()).await;
            link.send_message(Msg::ClusterStarted(todos));
        });
    }
}

impl TodoList {
    pub async fn new(raft: Raft<TodoItem>, link: ComponentLink<Model>) -> Self {
        let todos = raft
            .get_current_state()
            .await
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut rx = raft.subscribe();
        let l = link.clone();
        spawn_local(async move {
            while let Some(cmd) = rx.next().await {
                l.send_message(Msg::TodosUpdate(cmd));
            }
        });
        Self { raft, todos, link }
    }

    pub fn render(&self, new_todo: String) -> Html {
        html! {
            <>
            <h1>{ "TODO" }</h1>
            <div>
            <ul>
            {
                self.todos
                    .iter()
                    .map(|(_, todo)| todo.render(&self.link))
                    .collect::<Html>()
            }
            </ul>
                <input
                type="text"
                value=new_todo
                oninput=self.link.callback(|e: InputData| Msg::UpdateNewTodo(e.value))
                onkeypress=self.link.batch_callback(move |e: KeyboardEvent| {
                    if e.key() == "Enter" { Some(Msg::NewTodo) } else { None }
                })
                />
                </div>
                </>
        }
    }

    pub fn update(&mut self, cmd: LogCmd<TodoItem>) {
        match cmd {
            LogCmd::Set { key, data } => {
                self.todos.insert(key, data);
            }
            LogCmd::Delete { ref key } => {
                self.todos.remove(key);
            }
        }
    }

    pub fn toggle(&self, todo_text: String) {
        let old = self.todos.get(&todo_text).unwrap();
        let todo = TodoItem {
            text: todo_text.clone(),
            state: if let TodoState::Done = old.state {
                TodoState::Todo
            } else {
                TodoState::Done
            },
        };
        let raft = self.raft.clone();
        spawn_local(async move {
            raft.set(todo_text, todo).await.unwrap();
        });
    }

    pub fn add(&self, todo_text: String) {
        let todo = TodoItem {
            text: todo_text.clone(),
            state: TodoState::Todo,
        };
        let raft = self.raft.clone();
        spawn_local(async move {
            raft.set(todo_text, todo).await.unwrap();
        });
    }

    pub fn delete(&self, todo_text: String) {
        let raft = self.raft.clone();
        spawn_local(async move {
            raft.delete(todo_text).await.unwrap();
        });
    }
}

impl TodoItem {
    pub fn render(&self, link: &ComponentLink<Model>) -> Html {
        let t = self.text.clone();
        let delete = link.callback(move |_| Msg::DeleteTodo(t.to_string()));
        let t2 = self.text.clone();
        let toggle = link.callback(move |_| Msg::ToggleTodo(t2.to_string()));

        match self.state {
            TodoState::Todo => html! {
                <>
                <li onclick=toggle>{&self.text}</li>
                    <span onclick=delete>{ "🗑" }</span>
                    </>
            },
            TodoState::Done => html! {
                <>
                <li onclick=toggle><del>{&self.text}</del></li>
                    <span onclick=delete>{ "🗑" }</span>
                    </>
            },
        }
    }
}
