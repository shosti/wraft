use crate::raft;
use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumIter};

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct State {
    pub entries: Vec<Entry>,
    pub filter: Filter,
    pub value: String,
    pub edit_value: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Msg {
    Add,
    Edit(usize),
    Update(String),
    UpdateEdit(String),
    Remove(usize),
    SetFilter(Filter),
    ToggleAll,
    ToggleEdit(usize),
    Toggle(usize),
    ClearCompleted,
}

impl raft::Command for Msg {}

impl State {
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn total_completed(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| Filter::Completed.fits(e))
            .count()
    }

    pub fn is_all_completed(&self) -> bool {
        let mut filtered_iter = self
            .entries
            .iter()
            .filter(|e| self.filter.fits(e))
            .peekable();

        if filtered_iter.peek().is_none() {
            return false;
        }

        filtered_iter.all(|e| e.completed)
    }

    pub fn clear_completed(&mut self) {
        let entries = self
            .entries
            .drain(..)
            .filter(|e| Filter::Active.fits(e))
            .collect();
        self.entries = entries;
    }

    pub fn toggle(&mut self, idx: usize) {
        let filter = self.filter;
        let entry = self
            .entries
            .iter_mut()
            .filter(|e| filter.fits(e))
            .nth(idx)
            .unwrap();
        entry.completed = !entry.completed;
    }

    pub fn toggle_all(&mut self, value: bool) {
        for entry in &mut self.entries {
            if self.filter.fits(entry) {
                entry.completed = value;
            }
        }
    }

    pub fn toggle_edit(&mut self, idx: usize) {
        let filter = self.filter;
        let entry = self
            .entries
            .iter_mut()
            .filter(|e| filter.fits(e))
            .nth(idx)
            .unwrap();
        entry.editing = !entry.editing;
    }

    pub fn clear_all_edit(&mut self) {
        for entry in &mut self.entries {
            entry.editing = false;
        }
    }

    pub fn complete_edit(&mut self, idx: usize, val: String) {
        if val.is_empty() {
            self.remove(idx);
        } else {
            let filter = self.filter;
            let entry = self
                .entries
                .iter_mut()
                .filter(|e| filter.fits(e))
                .nth(idx)
                .unwrap();
            entry.description = val;
            entry.editing = !entry.editing;
        }
    }

    pub fn remove(&mut self, idx: usize) {
        let idx = {
            let entries = self
                .entries
                .iter()
                .enumerate()
                .filter(|&(_, e)| self.filter.fits(e))
                .collect::<Vec<_>>();
            let &(idx, _) = entries.get(idx).unwrap();
            idx
        };
        self.entries.remove(idx);
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Entry {
    pub description: String,
    pub completed: bool,
    pub editing: bool,
}

#[derive(Clone, Copy, Debug, EnumIter, Display, PartialEq, Serialize, Deserialize)]
pub enum Filter {
    All,
    Active,
    Completed,
}
impl Filter {
    pub fn fits(&self, entry: &Entry) -> bool {
        match *self {
            Filter::All => true,
            Filter::Active => !entry.completed,
            Filter::Completed => entry.completed,
        }
    }

    pub fn as_href(&self) -> &'static str {
        match self {
            Filter::All => "#/",
            Filter::Active => "#/active",
            Filter::Completed => "#/completed",
        }
    }
}
impl Default for Filter {
    fn default() -> Self {
        Filter::All
    }
}

impl raft::State for State {
    type Command = Msg;
    type Item = Self;
    type Key = ();
    type Update = ();

    fn apply(&mut self, cmd: Self::Command) {
        match cmd {
            Msg::Add => {
                let description = self.value.trim();
                if !description.is_empty() {
                    let entry = Entry {
                        description: description.to_string(),
                        completed: false,
                        editing: false,
                    };
                    self.entries.push(entry);
                }
                self.value = "".to_string();
            }
            Msg::Edit(idx) => {
                let edit_value = self.edit_value.trim().to_string();
                self.complete_edit(idx, edit_value);
                self.edit_value = "".to_string();
            }
            Msg::Update(val) => {
                self.value = val;
            }
            Msg::UpdateEdit(val) => {
                self.edit_value = val;
            }
            Msg::Remove(idx) => {
                self.remove(idx);
            }
            Msg::SetFilter(filter) => {
                self.filter = filter;
            }
            Msg::ToggleEdit(idx) => {
                self.edit_value = self.entries[idx].description.clone();
                self.clear_all_edit();
                self.toggle_edit(idx);
            }
            Msg::ToggleAll => {
                let status = !self.is_all_completed();
                self.toggle_all(status);
            }
            Msg::Toggle(idx) => {
                self.toggle(idx);
            }
            Msg::ClearCompleted => {
                self.clear_completed();
            }
        }
    }

    fn get(&self, _key: Self::Key) -> Option<Self::Item> {
        Some(self.clone())
    }
}
