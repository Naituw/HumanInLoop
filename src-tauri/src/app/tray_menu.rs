//! 声明式托盘菜单 + diff 协调器（spec 菜单稳定性）。
//!
//! 背景：`NSStatusItem` 的菜单一旦「整段重建」（清空后重新 append 全部条目）就会把用户**正展开的**
//! 菜单关掉。早先靠「内容签名没变就整次跳过」回避，但只要任一字段（如 uptime 每分钟、忙闲计数）变化
//! 仍会走整段重建 → 展开的菜单被关。
//!
//! 这里把菜单抽象成一棵声明式 [`Node`] 列表（每个节点带稳定 `key`），刷新时与「上次已应用」的影子树做
//! **diff**：文字 / 可用性变化只改属性（不动结构 → 不关菜单）；结构变化（条目增删、待答子项变动）才按
//! `key` 做**最小** insert / remove（子菜单子项递归 diff）。绝不整段清空重建。
//!
//! diff 算法（[`reconcile`] / [`build_live`] / [`update_live`]）对底层菜单实现**泛型**（[`MenuOps`]）：
//! 生产用 [`TauriBackend`] 包 Tauri 菜单对象；单测用 mock 后端记录每步操作，从而既验证 diff 结果、又验证
//! 操作的**最小性**（见文件末尾测试）。

use tauri::menu::{
    IsMenuItem, Menu, MenuItem, MenuItemBuilder, PredefinedMenuItem, Submenu, SubmenuBuilder,
};
use tauri::{AppHandle, Wry};

/// 声明式菜单节点。`key` 在**同一层级内**唯一且稳定，用于 diff 时按身份匹配；可点条目的事件也靠它路由
/// （`key` 即 muda 菜单项 id）。
#[derive(Clone)]
pub enum Node {
    /// 分隔线。
    Separator { key: String },
    /// 普通条目（`enabled=false` 即只读状态行）。
    Item {
        key: String,
        text: String,
        enabled: bool,
    },
    /// 子菜单。`children` 目前只用 `Item`（待答列表），但 diff 逻辑对任意 `Node` 通用。
    Submenu {
        key: String,
        text: String,
        enabled: bool,
        children: Vec<Node>,
    },
}

impl Node {
    pub fn item(key: impl Into<String>, text: impl Into<String>, enabled: bool) -> Node {
        Node::Item {
            key: key.into(),
            text: text.into(),
            enabled,
        }
    }
    pub fn separator(key: impl Into<String>) -> Node {
        Node::Separator { key: key.into() }
    }
    pub fn submenu(
        key: impl Into<String>,
        text: impl Into<String>,
        enabled: bool,
        children: Vec<Node>,
    ) -> Node {
        Node::Submenu {
            key: key.into(),
            text: text.into(),
            enabled,
            children,
        }
    }
    fn key(&self) -> &str {
        match self {
            Node::Separator { key } | Node::Item { key, .. } | Node::Submenu { key, .. } => key,
        }
    }
}

// ===== 底层菜单实现抽象（生产 = Tauri；测试 = mock）=====

/// 某个容器（根菜单或一个子菜单）。`Copy`，方便在 diff 过程中反复传值。
pub enum Container<'a, B: MenuOps + ?Sized> {
    Root,
    Sub(&'a B::Sub),
}
impl<'a, B: MenuOps + ?Sized> Clone for Container<'a, B> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a, B: MenuOps + ?Sized> Copy for Container<'a, B> {}

/// 指向某个已建条目句柄（用于 insert/remove 时把条目交给底层）。
pub enum ChildRef<'a, B: MenuOps + ?Sized> {
    Sep(&'a B::Sep),
    Item(&'a B::Item),
    Sub(&'a B::Sub),
}

/// 底层菜单操作集合：建条目、改属性、在容器里增删。diff 算法只依赖本 trait，从而可被 mock 测试。
pub trait MenuOps {
    /// 分隔符句柄。
    type Sep;
    /// 普通条目句柄。
    type Item;
    /// 子菜单句柄。
    type Sub;

    fn make_separator(&self, key: &str) -> Self::Sep;
    fn make_item(&self, key: &str, text: &str, enabled: bool) -> Self::Item;
    /// 建**空**子菜单；子项由 diff 逐个 `insert` 进去（统一走增删路径）。
    fn make_submenu(&self, key: &str, text: &str, enabled: bool) -> Self::Sub;

    fn set_item_text(&self, item: &Self::Item, text: &str);
    fn set_item_enabled(&self, item: &Self::Item, enabled: bool);
    fn set_sub_text(&self, sub: &Self::Sub, text: &str);
    fn set_sub_enabled(&self, sub: &Self::Sub, enabled: bool);

    fn insert(&self, container: Container<'_, Self>, child: ChildRef<'_, Self>, pos: usize);
    fn remove(&self, container: Container<'_, Self>, child: ChildRef<'_, Self>);
}

/// 已应用到底层的活动节点（持句柄 + 当前文字 / 可用性，供 diff 比对）。
enum Live<B: MenuOps> {
    Separator {
        key: String,
        handle: B::Sep,
    },
    Item {
        key: String,
        text: String,
        enabled: bool,
        handle: B::Item,
    },
    Submenu {
        key: String,
        text: String,
        enabled: bool,
        handle: B::Sub,
        children: Vec<Live<B>>,
    },
}

impl<B: MenuOps> Live<B> {
    fn key(&self) -> &str {
        match self {
            Live::Separator { key, .. } | Live::Item { key, .. } | Live::Submenu { key, .. } => key,
        }
    }
    fn child_ref(&self) -> ChildRef<'_, B> {
        match self {
            Live::Separator { handle, .. } => ChildRef::Sep(handle),
            Live::Item { handle, .. } => ChildRef::Item(handle),
            Live::Submenu { handle, .. } => ChildRef::Sub(handle),
        }
    }
}

/// 构建一个 [`Live`] 并在 `container` 的 `pos` 处插入；子菜单则递归建其子项。
fn build_live<B: MenuOps>(b: &B, container: Container<'_, B>, pos: usize, node: &Node) -> Live<B> {
    match node {
        Node::Separator { key } => {
            let handle = b.make_separator(key);
            b.insert(container, ChildRef::Sep(&handle), pos);
            Live::Separator {
                key: key.clone(),
                handle,
            }
        }
        Node::Item { key, text, enabled } => {
            let handle = b.make_item(key, text, *enabled);
            b.insert(container, ChildRef::Item(&handle), pos);
            Live::Item {
                key: key.clone(),
                text: text.clone(),
                enabled: *enabled,
                handle,
            }
        }
        Node::Submenu {
            key,
            text,
            enabled,
            children,
        } => {
            let handle = b.make_submenu(key, text, *enabled);
            b.insert(container, ChildRef::Sub(&handle), pos);
            // 子项从空开始 diff（全部走 insert 分支），统一增删路径。
            let mut child_lives: Vec<Live<B>> = Vec::new();
            reconcile(
                b,
                Container::Sub(&handle),
                &mut child_lives,
                children.clone(),
            );
            Live::Submenu {
                key: key.clone(),
                text: text.clone(),
                enabled: *enabled,
                handle,
                children: child_lives,
            }
        }
    }
}

/// 就地更新一个已存在的节点（`key` 已匹配；同一 `key` 永远同一类型，故 kind 视为一致）。
/// 子菜单递归 diff 其子项。
fn update_live<B: MenuOps>(b: &B, live: &mut Live<B>, node: Node) {
    match (live, node) {
        (Live::Separator { .. }, Node::Separator { .. }) => {}
        (
            Live::Item {
                text,
                enabled,
                handle,
                ..
            },
            Node::Item {
                text: nt,
                enabled: ne,
                ..
            },
        ) => {
            if *text != nt {
                b.set_item_text(handle, &nt);
                *text = nt;
            }
            if *enabled != ne {
                b.set_item_enabled(handle, ne);
                *enabled = ne;
            }
        }
        (
            Live::Submenu {
                text,
                enabled,
                handle,
                children,
                ..
            },
            Node::Submenu {
                text: nt,
                enabled: ne,
                children: nc,
                ..
            },
        ) => {
            if *text != nt {
                b.set_sub_text(handle, &nt);
                *text = nt;
            }
            if *enabled != ne {
                b.set_sub_enabled(handle, ne);
                *enabled = ne;
            }
            reconcile(b, Container::Sub(&*handle), children, nc);
        }
        // key 相同但 kind 不同：按取名约定不会发生；保底忽略。
        _ => {}
    }
}

/// 按 `key` 做位置敏感的 keyed reconcile：相同 key 就地更新，缺失的最小移除，新增的最小插入。
fn reconcile<B: MenuOps>(
    b: &B,
    container: Container<'_, B>,
    applied: &mut Vec<Live<B>>,
    desired: Vec<Node>,
) {
    let mut i = 0usize;
    for spec in desired.into_iter() {
        let key = spec.key().to_string();
        if i < applied.len() && applied[i].key() == key.as_str() {
            // 同位置同 key → 就地更新。
            update_live(b, &mut applied[i], spec);
            i += 1;
        } else if let Some(j) = applied
            .iter()
            .enumerate()
            .skip(i + 1)
            .find_map(|(idx, l)| (l.key() == key.as_str()).then_some(idx))
        {
            // key 出现在后面 → 中间这些已被删掉，先逐个移除再就地更新命中项。
            for _ in i..j {
                let removed = applied.remove(i);
                b.remove(container, removed.child_ref());
            }
            update_live(b, &mut applied[i], spec);
            i += 1;
        } else {
            // 全新节点 → 在当前位置构建并插入。
            let live = build_live(b, container, i, &spec);
            applied.insert(i, live);
            i += 1;
        }
    }
    // 末尾多余的（期望里已不存在）→ 移除。
    while applied.len() > i {
        let removed = applied.remove(i);
        b.remove(container, removed.child_ref());
    }
}

// ===== 生产后端：Tauri 菜单对象 =====

/// 包住 Tauri 的根 `Menu` + `AppHandle`，实现 [`MenuOps`]。
struct TauriBackend {
    app: AppHandle,
    root: Menu<Wry>,
}

impl MenuOps for TauriBackend {
    type Sep = PredefinedMenuItem<Wry>;
    type Item = MenuItem<Wry>;
    type Sub = Submenu<Wry>;

    fn make_separator(&self, _key: &str) -> Self::Sep {
        // 菜单条目构建是纯内存操作，实践中不会失败。
        PredefinedMenuItem::separator(&self.app).expect("create tray separator")
    }
    fn make_item(&self, key: &str, text: &str, enabled: bool) -> Self::Item {
        MenuItemBuilder::with_id(key.to_string(), text)
            .enabled(enabled)
            .build(&self.app)
            .expect("create tray item")
    }
    fn make_submenu(&self, key: &str, text: &str, enabled: bool) -> Self::Sub {
        SubmenuBuilder::with_id(&self.app, key.to_string(), text)
            .enabled(enabled)
            .build()
            .expect("create tray submenu")
    }

    fn set_item_text(&self, item: &Self::Item, text: &str) {
        let _ = item.set_text(text);
    }
    fn set_item_enabled(&self, item: &Self::Item, enabled: bool) {
        let _ = item.set_enabled(enabled);
    }
    fn set_sub_text(&self, sub: &Self::Sub, text: &str) {
        let _ = sub.set_text(text);
    }
    fn set_sub_enabled(&self, sub: &Self::Sub, enabled: bool) {
        let _ = sub.set_enabled(enabled);
    }

    fn insert(&self, container: Container<'_, Self>, child: ChildRef<'_, Self>, pos: usize) {
        let item: &dyn IsMenuItem<Wry> = match child {
            ChildRef::Sep(s) => s,
            ChildRef::Item(i) => i,
            ChildRef::Sub(s) => s,
        };
        match container {
            Container::Root => {
                let _ = self.root.insert(item, pos);
            }
            Container::Sub(s) => {
                let _ = s.insert(item, pos);
            }
        }
    }
    fn remove(&self, container: Container<'_, Self>, child: ChildRef<'_, Self>) {
        let item: &dyn IsMenuItem<Wry> = match child {
            ChildRef::Sep(s) => s,
            ChildRef::Item(i) => i,
            ChildRef::Sub(s) => s,
        };
        match container {
            Container::Root => {
                let _ = self.root.remove(item);
            }
            Container::Sub(s) => {
                let _ = s.remove(item);
            }
        }
    }
}

/// 持久托盘菜单：长期持有同一个根 `Menu` 对象 + 影子树，刷新时 diff 应用期望节点列表。
pub struct TrayMenu {
    backend: TauriBackend,
    applied: Vec<Live<TauriBackend>>,
}

impl TrayMenu {
    pub fn new(app: AppHandle, menu: Menu<Wry>) -> Self {
        Self {
            backend: TauriBackend { app, root: menu },
            applied: Vec::new(),
        }
    }
    /// 将期望的节点列表 diff 应用到菜单（最小增删 + 就地改文字 / 可用性）。
    pub fn apply(&mut self, desired: Vec<Node>) {
        reconcile(&self.backend, Container::Root, &mut self.applied, desired);
    }
}

// ===== 测试：用 mock 后端验证 diff 结果 + 操作最小性 =====

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    /// mock 节点（落在 mock 的扁平 `nodes` 表里，按 id 索引）。
    #[derive(Clone)]
    struct MockNode {
        kind: char, // 'I' item, 'S' separator, 'M' submenu
        key: String,
        text: String,
        enabled: bool,
        children: Vec<usize>,
    }

    /// mock 后端：句柄即 `usize` id；记录每步操作日志，便于断言「最小性」。
    struct MockOps {
        next: Cell<usize>,
        nodes: RefCell<HashMap<usize, MockNode>>,
        root: RefCell<Vec<usize>>,
        log: RefCell<Vec<String>>,
    }

    impl MockOps {
        fn new() -> Self {
            MockOps {
                next: Cell::new(1),
                nodes: RefCell::new(HashMap::new()),
                root: RefCell::new(Vec::new()),
                log: RefCell::new(Vec::new()),
            }
        }
        fn alloc(&self, n: MockNode) -> usize {
            let id = self.next.get();
            self.next.set(id + 1);
            self.nodes.borrow_mut().insert(id, n);
            id
        }
        fn key_of(&self, id: usize) -> String {
            self.nodes.borrow()[&id].key.clone()
        }
        fn child_id(child: &ChildRef<'_, MockOps>) -> usize {
            match child {
                ChildRef::Sep(s) => **s,
                ChildRef::Item(i) => **i,
                ChildRef::Sub(s) => **s,
            }
        }
        fn clear_log(&self) {
            self.log.borrow_mut().clear();
        }
        /// 把当前 mock 树渲染成可对比的结构。
        fn render(&self) -> Vec<R> {
            let root = self.root.borrow().clone();
            self.render_ids(&root)
        }
        fn render_ids(&self, ids: &[usize]) -> Vec<R> {
            ids.iter()
                .map(|id| {
                    // 短暂借用取出本节点字段（含子 id），随即释放再递归，避免重入借用冲突。
                    let (kind, key, text, enabled, children) = {
                        let nodes = self.nodes.borrow();
                        let n = &nodes[id];
                        (
                            n.kind,
                            n.key.clone(),
                            n.text.clone(),
                            n.enabled,
                            n.children.clone(),
                        )
                    };
                    let kids = self.render_ids(&children);
                    R {
                        kind,
                        key,
                        text,
                        enabled,
                        children: kids,
                    }
                })
                .collect()
        }
        /// 结构性操作数（insert/remove），用来断言最小性。
        fn structural_ops(&self) -> usize {
            self.log
                .borrow()
                .iter()
                .filter(|l| l.starts_with("insert ") || l.starts_with("remove "))
                .count()
        }
        fn ops(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    /// 渲染结果（与期望对比）。
    #[derive(Debug, PartialEq, Eq)]
    struct R {
        kind: char,
        key: String,
        text: String,
        enabled: bool,
        children: Vec<R>,
    }

    impl MenuOps for MockOps {
        type Sep = usize;
        type Item = usize;
        type Sub = usize;

        fn make_separator(&self, key: &str) -> usize {
            let id = self.alloc(MockNode {
                kind: 'S',
                key: key.to_string(),
                text: String::new(),
                enabled: false,
                children: vec![],
            });
            self.log.borrow_mut().push(format!("make S {key}"));
            id
        }
        fn make_item(&self, key: &str, text: &str, enabled: bool) -> usize {
            let id = self.alloc(MockNode {
                kind: 'I',
                key: key.to_string(),
                text: text.to_string(),
                enabled,
                children: vec![],
            });
            self.log.borrow_mut().push(format!("make I {key}"));
            id
        }
        fn make_submenu(&self, key: &str, text: &str, enabled: bool) -> usize {
            let id = self.alloc(MockNode {
                kind: 'M',
                key: key.to_string(),
                text: text.to_string(),
                enabled,
                children: vec![],
            });
            self.log.borrow_mut().push(format!("make M {key}"));
            id
        }

        fn set_item_text(&self, item: &usize, text: &str) {
            let key = self.key_of(*item);
            self.nodes.borrow_mut().get_mut(item).unwrap().text = text.to_string();
            self.log.borrow_mut().push(format!("settext {key} {text}"));
        }
        fn set_item_enabled(&self, item: &usize, enabled: bool) {
            let key = self.key_of(*item);
            self.nodes.borrow_mut().get_mut(item).unwrap().enabled = enabled;
            self.log
                .borrow_mut()
                .push(format!("setenabled {key} {enabled}"));
        }
        fn set_sub_text(&self, sub: &usize, text: &str) {
            let key = self.key_of(*sub);
            self.nodes.borrow_mut().get_mut(sub).unwrap().text = text.to_string();
            self.log.borrow_mut().push(format!("settext {key} {text}"));
        }
        fn set_sub_enabled(&self, sub: &usize, enabled: bool) {
            let key = self.key_of(*sub);
            self.nodes.borrow_mut().get_mut(sub).unwrap().enabled = enabled;
            self.log
                .borrow_mut()
                .push(format!("setenabled {key} {enabled}"));
        }

        fn insert(&self, container: Container<'_, Self>, child: ChildRef<'_, Self>, pos: usize) {
            let id = Self::child_id(&child);
            let key = self.key_of(id);
            match container {
                Container::Root => {
                    self.root.borrow_mut().insert(pos, id);
                    self.log
                        .borrow_mut()
                        .push(format!("insert {key} @{pos} root"));
                }
                Container::Sub(sid) => {
                    let pkey = self.key_of(*sid);
                    self.nodes
                        .borrow_mut()
                        .get_mut(sid)
                        .unwrap()
                        .children
                        .insert(pos, id);
                    self.log
                        .borrow_mut()
                        .push(format!("insert {key} @{pos} {pkey}"));
                }
            }
        }
        fn remove(&self, container: Container<'_, Self>, child: ChildRef<'_, Self>) {
            let id = Self::child_id(&child);
            let key = self.key_of(id);
            match container {
                Container::Root => {
                    self.root.borrow_mut().retain(|x| *x != id);
                    self.log.borrow_mut().push(format!("remove {key} root"));
                }
                Container::Sub(sid) => {
                    let pkey = self.key_of(*sid);
                    self.nodes
                        .borrow_mut()
                        .get_mut(sid)
                        .unwrap()
                        .children
                        .retain(|x| *x != id);
                    self.log.borrow_mut().push(format!("remove {key} {pkey}"));
                }
            }
        }
    }

    /// 测试驱动：持影子树，apply 一批期望节点。
    struct Driver<'a> {
        b: &'a MockOps,
        applied: Vec<Live<MockOps>>,
    }
    impl<'a> Driver<'a> {
        fn new(b: &'a MockOps) -> Self {
            Driver {
                b,
                applied: Vec::new(),
            }
        }
        fn apply(&mut self, desired: Vec<Node>) {
            reconcile(self.b, Container::Root, &mut self.applied, desired);
        }
    }

    // —— 便捷构造器 ——
    fn it(key: &str, text: &str) -> Node {
        Node::item(key, text, true)
    }
    fn dis(key: &str, text: &str) -> Node {
        Node::item(key, text, false)
    }
    fn sep(key: &str) -> Node {
        Node::separator(key)
    }
    fn ri(key: &str, text: &str, enabled: bool) -> R {
        R {
            kind: 'I',
            key: key.into(),
            text: text.into(),
            enabled,
            children: vec![],
        }
    }
    fn rs(key: &str) -> R {
        R {
            kind: 'S',
            key: key.into(),
            text: String::new(),
            enabled: false,
            children: vec![],
        }
    }

    #[test]
    fn initial_build_inserts_all_in_order() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![
            dis("title", "Running"),
            sep("s1"),
            it("settings", "Settings"),
        ]);
        assert_eq!(
            b.render(),
            vec![
                ri("title", "Running", false),
                rs("s1"),
                ri("settings", "Settings", true)
            ]
        );
        // 三个条目 → 三次 insert。
        assert_eq!(b.structural_ops(), 3);
    }

    #[test]
    fn identical_apply_is_noop() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        let spec = || vec![dis("title", "Running"), it("settings", "Settings")];
        d.apply(spec());
        b.clear_log();
        d.apply(spec());
        // 完全相同 → 零操作（不 set_text、不增删）。
        assert!(b.ops().is_empty(), "expected no ops, got {:?}", b.ops());
    }

    #[test]
    fn text_change_only_sets_text() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![dis("uptime", "0s"), it("settings", "Settings")]);
        b.clear_log();
        d.apply(vec![dis("uptime", "15s"), it("settings", "Settings")]);
        assert_eq!(b.ops(), vec!["settext uptime 15s".to_string()]);
        assert_eq!(b.structural_ops(), 0);
        assert_eq!(
            b.render(),
            vec![
                ri("uptime", "0s", false).with_text("15s"),
                ri("settings", "Settings", true)
            ]
        );
    }

    #[test]
    fn enabled_change_only_sets_enabled() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![it("stop", "Stop")]);
        b.clear_log();
        d.apply(vec![dis("stop", "Stop")]);
        assert_eq!(b.ops(), vec!["setenabled stop false".to_string()]);
        assert_eq!(b.structural_ops(), 0);
    }

    #[test]
    fn insert_in_middle_is_minimal() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![dis("title", "Running"), it("settings", "Settings")]);
        b.clear_log();
        // 在中间插入 uptime。
        d.apply(vec![
            dis("title", "Running"),
            dis("uptime", "1m"),
            it("settings", "Settings"),
        ]);
        assert_eq!(b.structural_ops(), 1);
        assert_eq!(
            b.ops(),
            vec![
                "make I uptime".to_string(),
                "insert uptime @1 root".to_string()
            ]
        );
        assert_eq!(
            b.render(),
            vec![
                ri("title", "Running", false),
                ri("uptime", "1m", false),
                ri("settings", "Settings", true)
            ]
        );
    }

    #[test]
    fn remove_in_middle_is_minimal() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![
            dis("title", "Running"),
            dis("uptime", "1m"),
            it("settings", "Settings"),
        ]);
        b.clear_log();
        d.apply(vec![dis("title", "Running"), it("settings", "Settings")]);
        assert_eq!(b.ops(), vec!["remove uptime root".to_string()]);
        assert_eq!(
            b.render(),
            vec![
                ri("title", "Running", false),
                ri("settings", "Settings", true)
            ]
        );
    }

    #[test]
    fn remove_multiple_contiguous() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![
            dis("title", "Running"),
            dis("version", "0.7"),
            dis("uptime", "1m"),
            it("settings", "Settings"),
        ]);
        b.clear_log();
        d.apply(vec![dis("title", "Running"), it("settings", "Settings")]);
        assert_eq!(b.structural_ops(), 2);
        assert_eq!(
            b.ops(),
            vec![
                "remove version root".to_string(),
                "remove uptime root".to_string()
            ]
        );
    }

    #[test]
    fn trailing_removal() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![dis("a", "A"), dis("b", "B"), dis("c", "C")]);
        b.clear_log();
        d.apply(vec![dis("a", "A")]);
        assert_eq!(
            b.ops(),
            vec!["remove b root".to_string(), "remove c root".to_string()]
        );
        assert_eq!(b.render(), vec![ri("a", "A", false)]);
    }

    #[test]
    fn swap_key_at_same_slot_replaces() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        // 待答从「只读计数行」切到「子菜单」即此类：同一槽位 key 变了。
        d.apply(vec![
            dis("pending_count", "1 pending"),
            it("settings", "Settings"),
        ]);
        b.clear_log();
        d.apply(vec![
            Node::submenu(
                "pending_menu",
                "1 pending",
                true,
                vec![it("focus:a", "Q a")],
            ),
            it("settings", "Settings"),
        ]);
        // pending_menu 新增（含 1 个子项）+ pending_count 移除。
        assert!(b.ops().contains(&"insert pending_menu @0 root".to_string()));
        assert!(b
            .ops()
            .contains(&"insert focus:a @0 pending_menu".to_string()));
        assert!(b.ops().contains(&"remove pending_count root".to_string()));
        assert_eq!(
            b.render(),
            vec![
                R {
                    kind: 'M',
                    key: "pending_menu".into(),
                    text: "1 pending".into(),
                    enabled: true,
                    children: vec![ri("focus:a", "Q a", true)],
                },
                ri("settings", "Settings", true),
            ]
        );
    }

    #[test]
    fn reorder_results_in_correct_tree() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![it("a", "A"), it("b", "B")]);
        b.clear_log();
        d.apply(vec![it("b", "B"), it("a", "A")]);
        // 结果树正确（顺序换了）。
        assert_eq!(b.render(), vec![ri("b", "B", true), ri("a", "A", true)]);
    }

    #[test]
    fn submenu_child_text_change_only_sets_text() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![Node::submenu(
            "pending",
            "1 pending",
            true,
            vec![it("focus:a", "Q a")],
        )]);
        b.clear_log();
        d.apply(vec![Node::submenu(
            "pending",
            "1 pending",
            true,
            vec![it("focus:a", "Q a (edited)")],
        )]);
        assert_eq!(b.ops(), vec!["settext focus:a Q a (edited)".to_string()]);
        assert_eq!(b.structural_ops(), 0);
    }

    #[test]
    fn submenu_child_added_and_removed() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![Node::submenu(
            "pending",
            "1 pending",
            true,
            vec![it("focus:a", "Q a")],
        )]);
        b.clear_log();
        // 加一个子项。
        d.apply(vec![Node::submenu(
            "pending",
            "2 pending",
            true,
            vec![it("focus:a", "Q a"), it("focus:b", "Q b")],
        )]);
        assert!(b.ops().contains(&"settext pending 2 pending".to_string()));
        assert!(b.ops().contains(&"insert focus:b @1 pending".to_string()));
        // 子菜单本身没有被增删（只动其子项 + 标题）。
        assert!(!b.ops().iter().any(|l| l.contains("insert pending")));

        b.clear_log();
        // 再删回去。
        d.apply(vec![Node::submenu(
            "pending",
            "1 pending",
            true,
            vec![it("focus:a", "Q a")],
        )]);
        assert!(b.ops().contains(&"remove focus:b pending".to_string()));
        assert_eq!(
            b.render(),
            vec![R {
                kind: 'M',
                key: "pending".into(),
                text: "1 pending".into(),
                enabled: true,
                children: vec![ri("focus:a", "Q a", true)],
            }]
        );
    }

    #[test]
    fn submenu_title_change_sets_text() {
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(vec![Node::submenu(
            "pending",
            "1 pending",
            true,
            vec![it("x", "X")],
        )]);
        b.clear_log();
        d.apply(vec![Node::submenu(
            "pending",
            "1 待答",
            true,
            vec![it("x", "X")],
        )]);
        assert_eq!(b.ops(), vec!["settext pending 1 待答".to_string()]);
    }

    #[test]
    fn agents_submenu_add_remove_and_badge_text_change() {
        // spec agent-interject D7：「Agent 状态」父项为子菜单，首项打开窗口 + 分隔线 + 逐 agent 子菜单。
        let menu = |agents: Vec<(&str, &str, bool)>| {
            let mut children = vec![it("open_agents", "Open Status Window"), sep("sep.agents")];
            for (sid, msg_text, focusable) in agents {
                let mut sub = vec![it(&format!("ij:{sid}"), msg_text)];
                if focusable {
                    sub.push(it(&format!("term:{sid}"), "Focus Terminal"));
                }
                children.push(Node::submenu(
                    format!("agent:{sid}"),
                    format!("Claude Code · proj-{sid}"),
                    true,
                    sub,
                ));
            }
            vec![Node::submenu(
                "agents_menu",
                "Agent Status (1 · 0)",
                true,
                children,
            )]
        };
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(menu(vec![("s1", "Send Message…", true)]));

        // 新 agent 上线：仅插入其子菜单（含子项），其余不动。
        b.clear_log();
        d.apply(menu(vec![
            ("s1", "Send Message…", true),
            ("s2", "Send Message…", false),
        ]));
        assert!(b
            .ops()
            .contains(&"insert agent:s2 @3 agents_menu".to_string()));
        assert!(b.ops().contains(&"insert ij:s2 @0 agent:s2".to_string()));
        assert!(!b.ops().iter().any(|l| l.contains("remove")));

        // 待送达徽标：仅该 agent 的「发送消息」文字变化，零结构操作。
        b.clear_log();
        d.apply(menu(vec![
            ("s1", "Send Message… (queued)", true),
            ("s2", "Send Message…", false),
        ]));
        assert_eq!(
            b.ops(),
            vec!["settext ij:s1 Send Message… (queued)".to_string()]
        );
        assert_eq!(b.structural_ops(), 0);

        // agent 下线：仅移除其子菜单。
        b.clear_log();
        d.apply(menu(vec![("s2", "Send Message…", false)]));
        assert_eq!(b.ops(), vec!["remove agent:s1 agents_menu".to_string()]);
    }

    #[test]
    fn full_menu_uptime_tick_only_sets_text() {
        // 还原真实场景：完整菜单，仅 uptime 文案变化 → 只 1 次 set_text、0 结构操作。
        let menu = |uptime: &str| {
            vec![
                dis("st.title", "Running"),
                dis("st.version", "0.7.1"),
                dis("st.uptime", uptime),
                sep("sep.actions"),
                it("open_settings", "Settings"),
                it("open_history", "History"),
                sep("sep.daemon"),
                it("restart_daemon", "Restart"),
                it("stop_daemon", "Stop"),
            ]
        };
        let b = MockOps::new();
        let mut d = Driver::new(&b);
        d.apply(menu("0s"));
        b.clear_log();
        d.apply(menu("15s"));
        assert_eq!(b.ops(), vec!["settext st.uptime 15s".to_string()]);
        assert_eq!(b.structural_ops(), 0);
    }

    impl R {
        fn with_text(mut self, t: &str) -> R {
            self.text = t.to_string();
            self
        }
    }
}
