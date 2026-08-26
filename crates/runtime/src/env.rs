//! 运行时词法环境。
//!
//! `LexicalEnv` 对应 ECMAScript 里的 Lexical Environment 链。为了减小 bytecode 中 names
//! 段体积，函数局部变量优先通过 `LocalSlot` 访问；只有真正需要全局/闭包名字时才保留字符串。
//! 因此这里同时维护字符串绑定和 slot 绑定。

use crate::value::{Value, object_value};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

const FAST_SLOT_LIMIT: usize = 65_536;

/// 词法环境链。
///
/// `frames` 从全局作用域到当前作用域顺序排列。查找名字时从后向前，查找 slot 时使用
/// `slot_cache` 加速最近命中的 frame 下标，避免热路径反复克隆 `Rc`。
#[derive(Debug)]
pub struct LexicalEnv {
    frames: Vec<ScopeFrame>,
    slot_cache: Rc<RefCell<Vec<Option<usize>>>>,
    name_cache: Rc<RefCell<BTreeMap<String, usize>>>,
}

impl Clone for LexicalEnv {
    fn clone(&self) -> Self {
        Self {
            frames: self.frames.clone(),
            slot_cache: Rc::new(RefCell::new(Vec::new())),
            name_cache: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }
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
            slot_cache: Rc::new(RefCell::new(Vec::new())),
            name_cache: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }
}

impl LexicalEnv {
    /// 当前作用域深度。
    pub(crate) fn depth(&self) -> usize {
        self.frames.len()
    }

    /// 进入一个新作用域。
    pub(crate) fn push_frame(&mut self, kind: ScopeKind) {
        self.frames.push(ScopeFrame::new(kind));
        self.clear_lookup_caches();
    }

    /// 离开当前作用域。
    ///
    /// 全局作用域永远不会被弹出。
    pub(crate) fn pop_frame(&mut self) {
        if self
            .frames
            .last()
            .is_some_and(|frame| frame.kind != ScopeKind::Global)
        {
            self.frames.pop();
            self.clear_lookup_caches();
        }
    }

    /// 截断到指定作用域深度。
    ///
    /// try/catch/finally 或函数返回时用它恢复执行前的环境形状。
    pub(crate) fn truncate_to_depth(&mut self, depth: usize) {
        let depth = depth.max(1);
        while self.frames.len() > depth {
            self.pop_frame();
        }
    }

    /// 按字符串名字查找绑定。
    pub(crate) fn get(&self, name: &str) -> Option<Value> {
        if let Some(frame_index) = self.cached_name_frame(name)
            && let Some(value) = self.frames[frame_index]
                .record
                .borrow()
                .bindings
                .get(name)
                .cloned()
        {
            return Some(value);
        }
        for (frame_index, frame) in self.frames.iter().enumerate().rev() {
            if let Some(value) = frame.record.borrow().bindings.get(name).cloned() {
                self.cache_name_frame(name, frame_index);
                return Some(value);
            }
        }
        None
    }

    /// 在当前作用域定义字符串绑定。
    pub(crate) fn define_current(&self, name: String, value: Value) {
        if let Some((frame_index, frame)) = self.frames.iter().enumerate().next_back() {
            frame
                .record
                .borrow_mut()
                .bindings
                .insert(name.clone(), value);
            self.cache_name_frame(&name, frame_index);
        }
    }

    /// 当前作用域不存在时才定义字符串绑定。
    pub(crate) fn define_current_if_absent(&self, name: String, value: Value) {
        if let Some((frame_index, frame)) = self.frames.iter().enumerate().next_back() {
            frame
                .record
                .borrow_mut()
                .bindings
                .entry(name.clone())
                .or_insert(value);
            self.cache_name_frame(&name, frame_index);
        }
    }

    /// 在当前作用域定义 local slot。
    pub(crate) fn define_slot(&self, slot: u32, value: Value) {
        if let Some((frame_index, frame)) = self.frames.iter().enumerate().next_back() {
            frame.record.borrow_mut().insert_slot(slot, value);
            self.cache_slot_frame(slot, frame_index);
        }
    }

    /// 当前作用域不存在该 slot 时才定义。
    pub(crate) fn define_slot_if_absent(&self, slot: u32, value: Value) {
        if let Some((frame_index, frame)) = self.frames.iter().enumerate().next_back() {
            frame.record.borrow_mut().insert_slot_if_absent(slot, value);
            self.cache_slot_frame(slot, frame_index);
        }
    }

    /// 按 `var` 语义定义 slot。
    ///
    /// `var` 应提升到最近的函数作用域或全局作用域，而不是块级作用域。
    pub(crate) fn define_var_slot_if_absent(&self, slot: u32, value: Value) {
        if let Some((frame_index, frame)) = self
            .frames
            .iter()
            .enumerate()
            .rev()
            .find(|(_, frame)| matches!(frame.kind, ScopeKind::Function | ScopeKind::Global))
        {
            frame.record.borrow_mut().insert_slot_if_absent(slot, value);
            self.cache_slot_frame(slot, frame_index);
        }
    }

    /// 查找 slot 值。
    ///
    /// 查找不会跨越函数边界；这是函数局部 slot 能压缩 names 段的关键约束。
    pub(crate) fn get_slot(&self, slot: u32) -> Value {
        if let Some(frame_index) = self.cached_slot_frame(slot)
            && let Some(value) = self.frames[frame_index].record.borrow().get_slot(slot)
        {
            return value;
        }
        for (frame_index, frame) in self.frames.iter().enumerate().rev() {
            if let Some(value) = frame.record.borrow().get_slot(slot) {
                self.cache_slot_frame(slot, frame_index);
                return value;
            }
            if frame.kind == ScopeKind::Function || frame.kind == ScopeKind::Global {
                break;
            }
        }
        Value::Undefined
    }

    /// 写入 slot 值。
    pub(crate) fn set_slot(&self, slot: u32, value: Value) {
        if let Some(frame_index) = self.cached_slot_frame(slot)
            && self.frames[frame_index].record.borrow().has_slot(slot)
        {
            self.frames[frame_index]
                .record
                .borrow_mut()
                .insert_slot(slot, value);
            return;
        }
        for (frame_index, frame) in self.frames.iter().enumerate().rev() {
            let mut record = frame.record.borrow_mut();
            if record.has_slot(slot) {
                record.insert_slot(slot, value);
                drop(record);
                self.cache_slot_frame(slot, frame_index);
                return;
            }
            if frame.kind == ScopeKind::Function || frame.kind == ScopeKind::Global {
                break;
            }
        }
        if let Some((frame_index, frame)) = self.frames.iter().enumerate().next_back() {
            frame.record.borrow_mut().insert_slot(slot, value);
            self.cache_slot_frame(slot, frame_index);
        }
    }

    /// 全局作用域不存在时才定义名字。
    pub(crate) fn define_global_if_absent(&self, name: String, value: Value) {
        if let Some((_, frame)) = self.frames.iter().enumerate().next() {
            frame
                .record
                .borrow_mut()
                .bindings
                .entry(name)
                .or_insert(value);
            self.clear_name_cache();
        }
    }

    /// 按 `var` 语义定义字符串绑定。
    pub(crate) fn define_var_if_absent(&self, name: String, value: Value) {
        if let Some((_, frame)) = self
            .frames
            .iter()
            .enumerate()
            .rev()
            .find(|(_, frame)| matches!(frame.kind, ScopeKind::Function | ScopeKind::Global))
        {
            frame
                .record
                .borrow_mut()
                .bindings
                .entry(name)
                .or_insert(value);
            self.clear_name_cache();
        }
    }

    /// 如果外层已有同名绑定则写入，否则在当前作用域定义。
    pub(crate) fn set_or_define_current(&self, name: String, value: Value) {
        if self.set_existing(&name, value.clone()) {
            return;
        }
        self.define_current(name, value);
    }

    /// 写入已经存在的字符串绑定。
    pub(crate) fn set_existing(&self, name: &str, value: Value) -> bool {
        for (frame_index, frame) in self.frames.iter().enumerate().rev() {
            let has_binding = frame.record.borrow().bindings.contains_key(name);
            if has_binding {
                frame
                    .record
                    .borrow_mut()
                    .bindings
                    .insert(name.to_string(), value);
                self.cache_name_frame(name, frame_index);
                return true;
            }
        }
        false
    }

    fn cached_slot_frame(&self, slot: u32) -> Option<usize> {
        let index = slot as usize;
        self.slot_cache
            .borrow()
            .get(index)
            .copied()
            .flatten()
            .filter(|frame_index| *frame_index < self.frames.len())
    }

    fn cache_slot_frame(&self, slot: u32, frame_index: usize) {
        let index = slot as usize;
        if index >= FAST_SLOT_LIMIT {
            return;
        }
        let mut cache = self.slot_cache.borrow_mut();
        if cache.len() <= index {
            cache.resize_with(index + 1, || None);
        }
        cache[index] = Some(frame_index);
    }

    fn cached_name_frame(&self, name: &str) -> Option<usize> {
        self.name_cache
            .borrow()
            .get(name)
            .copied()
            .filter(|frame_index| *frame_index < self.frames.len())
    }

    fn cache_name_frame(&self, name: &str, frame_index: usize) {
        self.name_cache
            .borrow_mut()
            .insert(name.to_string(), frame_index);
    }

    fn clear_lookup_caches(&self) {
        self.clear_slot_cache();
        self.clear_name_cache();
    }

    fn clear_slot_cache(&self) {
        self.slot_cache.borrow_mut().clear();
    }

    fn clear_name_cache(&self) {
        self.name_cache.borrow_mut().clear();
    }
}

/// 单个作用域帧。
///
/// frame 本身保存作用域类型，实际绑定数据放在可共享的 `EnvironmentRecord` 中。
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

/// ECMAScript Environment Record 的简化实现。
///
/// `bindings` 保存仍需按名字访问的值；`slot_values`/`overflow_slots` 保存压缩后的 local slot。
#[derive(Debug, Default, Clone)]
pub struct EnvironmentRecord {
    bindings: BTreeMap<String, Value>,
    slot_values: Vec<Option<Value>>,
    overflow_slots: BTreeMap<u32, Value>,
}

impl EnvironmentRecord {
    fn get_slot(&self, slot: u32) -> Option<Value> {
        let index = slot as usize;
        if index < self.slot_values.len() {
            return self.slot_values[index].clone();
        }
        self.overflow_slots.get(&slot).cloned()
    }

    fn has_slot(&self, slot: u32) -> bool {
        let index = slot as usize;
        if index < self.slot_values.len() && self.slot_values[index].is_some() {
            return true;
        }
        self.overflow_slots.contains_key(&slot)
    }

    fn insert_slot(&mut self, slot: u32, value: Value) {
        let index = slot as usize;
        if index < FAST_SLOT_LIMIT {
            if self.slot_values.len() <= index {
                self.slot_values.resize_with(index + 1, || None);
            }
            self.slot_values[index] = Some(value);
        } else {
            self.overflow_slots.insert(slot, value);
        }
    }

    fn insert_slot_if_absent(&mut self, slot: u32, value: Value) {
        if !self.has_slot(slot) {
            self.insert_slot(slot, value);
        }
    }
}

/// 运行时作用域类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// 全局作用域。
    Global,
    /// 函数作用域。
    Function,
    /// 块级作用域。
    Block,
    /// catch 作用域。
    Catch,
}
