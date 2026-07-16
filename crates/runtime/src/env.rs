use crate::value::{Value, object_value};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

#[derive(Debug, Clone)]
pub struct LexicalEnv {
    frames: Vec<ScopeFrame>,
}

impl Default for LexicalEnv {
    fn default() -> Self {
        let global = ScopeFrame::new(ScopeKind::Global);
        let mut this_props = BTreeMap::new();
        this_props.insert("WScript".to_string(), object_value(BTreeMap::new()));
        global
            .record
            .borrow_mut()
            .bindings
            .insert("this".to_string(), object_value(this_props));
        Self {
            frames: vec![global],
        }
    }
}

impl LexicalEnv {
    pub(crate) fn depth(&self) -> usize {
        self.frames.len()
    }

    pub(crate) fn push_frame(&mut self, kind: ScopeKind) {
        self.frames.push(ScopeFrame::new(kind));
    }

    pub(crate) fn pop_frame(&mut self) {
        if self
            .frames
            .last()
            .is_some_and(|frame| frame.kind != ScopeKind::Global)
        {
            self.frames.pop();
        }
    }

    pub(crate) fn truncate_to_depth(&mut self, depth: usize) {
        let depth = depth.max(1);
        while self.frames.len() > depth {
            self.pop_frame();
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<Value> {
        self.frames
            .iter()
            .rev()
            .find_map(|frame| frame.record.borrow().bindings.get(name).cloned())
    }

    pub(crate) fn define_current(&self, name: String, value: Value) {
        if let Some(frame) = self.frames.last() {
            frame.record.borrow_mut().bindings.insert(name, value);
        }
    }

    pub(crate) fn define_current_if_absent(&self, name: String, value: Value) {
        if let Some(frame) = self.frames.last() {
            frame
                .record
                .borrow_mut()
                .bindings
                .entry(name)
                .or_insert(value);
        }
    }

    pub(crate) fn define_slot(&self, slot: u32, value: Value) {
        if let Some(frame) = self.frames.last() {
            frame.record.borrow_mut().slots.insert(slot, value);
        }
    }

    pub(crate) fn define_slot_if_absent(&self, slot: u32, value: Value) {
        if let Some(frame) = self.frames.last() {
            frame.record.borrow_mut().slots.entry(slot).or_insert(value);
        }
    }

    pub(crate) fn define_var_slot_if_absent(&self, slot: u32, value: Value) {
        if let Some(frame) = self
            .frames
            .iter()
            .rev()
            .find(|frame| matches!(frame.kind, ScopeKind::Function | ScopeKind::Global))
        {
            frame.record.borrow_mut().slots.entry(slot).or_insert(value);
        }
    }

    pub(crate) fn get_slot(&self, slot: u32) -> Value {
        for frame in self.frames.iter().rev() {
            if let Some(value) = frame.record.borrow().slots.get(&slot).cloned() {
                return value;
            }
            if frame.kind == ScopeKind::Function || frame.kind == ScopeKind::Global {
                break;
            }
        }
        Value::Undefined
    }

    pub(crate) fn set_slot(&self, slot: u32, value: Value) {
        for frame in self.frames.iter().rev() {
            let mut record = frame.record.borrow_mut();
            if let std::collections::btree_map::Entry::Occupied(mut entry) =
                record.slots.entry(slot)
            {
                entry.insert(value);
                return;
            }
            if frame.kind == ScopeKind::Function || frame.kind == ScopeKind::Global {
                break;
            }
        }
        if let Some(frame) = self.frames.last() {
            frame.record.borrow_mut().slots.insert(slot, value);
        }
    }

    pub(crate) fn define_global_if_absent(&self, name: String, value: Value) {
        if let Some(frame) = self.frames.first() {
            frame
                .record
                .borrow_mut()
                .bindings
                .entry(name)
                .or_insert(value);
        }
    }

    pub(crate) fn define_var_if_absent(&self, name: String, value: Value) {
        if let Some(frame) = self
            .frames
            .iter()
            .rev()
            .find(|frame| matches!(frame.kind, ScopeKind::Function | ScopeKind::Global))
        {
            frame
                .record
                .borrow_mut()
                .bindings
                .entry(name)
                .or_insert(value);
        }
    }

    pub(crate) fn set_or_define_current(&self, name: String, value: Value) {
        if self.set_existing(&name, value.clone()) {
            return;
        }
        self.define_current(name, value);
    }

    pub(crate) fn set_existing(&self, name: &str, value: Value) -> bool {
        for frame in self.frames.iter().rev() {
            let has_binding = frame.record.borrow().bindings.contains_key(name);
            if has_binding {
                frame
                    .record
                    .borrow_mut()
                    .bindings
                    .insert(name.to_string(), value);
                return true;
            }
        }
        false
    }
}

#[derive(Debug, Clone)]
pub struct ScopeFrame {
    kind: ScopeKind,
    record: Rc<RefCell<EnvironmentRecord>>,
}

impl ScopeFrame {
    fn new(kind: ScopeKind) -> Self {
        Self {
            kind,
            record: Rc::new(RefCell::new(EnvironmentRecord::default())),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct EnvironmentRecord {
    bindings: BTreeMap<String, Value>,
    slots: BTreeMap<u32, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Global,
    Function,
    Block,
    Catch,
}
