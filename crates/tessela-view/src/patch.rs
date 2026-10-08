//! Stable node ids and minimal updates between successive trees.
//!
//! The host keeps a [`Reconciler`] per extension. Each new tree is matched
//! against the previous one — by `key` when present, otherwise by position
//! and kind — and only the differences go out as [`Patch`]es. Renderers keep a
//! [`ViewTree`] and apply them. Patches are plain serde data so the same
//! stream can cross a socket to a remote renderer (docs/adr/0011).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{Node, Props};

pub type NodeId = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Patch {
    /// A new node, without children yet.
    Create {
        id: NodeId,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default)]
        props: Props,
    },
    /// Changed props of an existing node.
    Update {
        id: NodeId,
        #[serde(default)]
        set: Props,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unset: Vec<String>,
    },
    /// The complete, ordered child list of a node.
    Children { id: NodeId, children: Vec<NodeId> },
    /// The tree's root is now this node.
    Root { id: NodeId },
    /// Nodes that left the tree (already detached from their parents).
    Remove { ids: Vec<NodeId> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Retained {
    pub kind: String,
    pub key: Option<String>,
    pub props: Props,
    pub children: Vec<NodeId>,
}

/// A tree addressed by [`NodeId`], built by applying patches.
#[derive(Debug, Clone, Default)]
pub struct ViewTree {
    nodes: HashMap<NodeId, Retained>,
    root: Option<NodeId>,
}

impl ViewTree {
    pub fn apply(&mut self, patches: &[Patch]) -> Result<(), String> {
        for p in patches {
            match p {
                Patch::Create {
                    id,
                    kind,
                    key,
                    props,
                } => {
                    let node = Retained {
                        kind: kind.clone(),
                        key: key.clone(),
                        props: props.clone(),
                        children: vec![],
                    };
                    if self.nodes.insert(*id, node).is_some() {
                        return Err(format!("node {id} created twice"));
                    }
                }
                Patch::Update { id, set, unset } => {
                    let n = self
                        .nodes
                        .get_mut(id)
                        .ok_or_else(|| format!("update of unknown node {id}"))?;
                    for k in unset {
                        n.props.remove(k);
                    }
                    n.props
                        .extend(set.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                Patch::Children { id, children } => {
                    if let Some(c) = children.iter().find(|c| !self.nodes.contains_key(c)) {
                        return Err(format!("node {id} lists unknown child {c}"));
                    }
                    self.nodes
                        .get_mut(id)
                        .ok_or_else(|| format!("children of unknown node {id}"))?
                        .children = children.clone();
                }
                Patch::Root { id } => {
                    if !self.nodes.contains_key(id) {
                        return Err(format!("unknown root {id}"));
                    }
                    self.root = Some(*id);
                }
                Patch::Remove { ids } => {
                    for id in ids {
                        self.nodes.remove(id);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    pub fn get(&self, id: NodeId) -> Option<&Retained> {
        self.nodes.get(&id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The current tree as plain [`Node`]s.
    pub fn to_node(&self) -> Option<Node> {
        self.root.and_then(|r| self.node(r))
    }

    pub fn node(&self, id: NodeId) -> Option<Node> {
        let r = self.nodes.get(&id)?;
        Some(Node {
            kind: r.kind.clone(),
            key: r.key.clone(),
            props: r.props.clone(),
            children: r.children.iter().filter_map(|c| self.node(*c)).collect(),
        })
    }
}

/// Produces patches from successive trees, keeping ids stable.
#[derive(Debug, Default)]
pub struct Reconciler {
    tree: ViewTree,
    next: NodeId,
}

impl Reconciler {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tree as the last update left it.
    pub fn tree(&self) -> &ViewTree {
        &self.tree
    }

    /// Everything needed to rebuild the current tree from nothing, for a
    /// renderer that connects late.
    pub fn snapshot(&self) -> Vec<Patch> {
        let mut out = Vec::new();
        if let Some(root) = self.tree.root {
            self.emit_subtree(root, &mut out);
            out.push(Patch::Root { id: root });
        }
        out
    }

    fn emit_subtree(&self, id: NodeId, out: &mut Vec<Patch>) {
        let n = &self.tree.nodes[&id];
        out.push(Patch::Create {
            id,
            kind: n.kind.clone(),
            key: n.key.clone(),
            props: n.props.clone(),
        });
        for c in &n.children {
            self.emit_subtree(*c, out);
        }
        if !n.children.is_empty() {
            out.push(Patch::Children {
                id,
                children: n.children.clone(),
            });
        }
    }

    pub fn update(&mut self, node: &Node) -> Vec<Patch> {
        let mut out = Vec::new();
        let mut removed = Vec::new();
        let old = self.tree.root;
        let id = self.reconcile(old, node, &mut out, &mut removed);
        if Some(id) != old {
            out.push(Patch::Root { id });
        }
        if !removed.is_empty() {
            out.push(Patch::Remove { ids: removed });
        }
        self.tree
            .apply(&out)
            .expect("a reconciler's own patches always apply");
        out
    }

    fn fresh(&mut self) -> NodeId {
        self.next += 1;
        self.next
    }

    fn collect(&self, id: NodeId, removed: &mut Vec<NodeId>) {
        removed.push(id);
        if let Some(n) = self.tree.nodes.get(&id) {
            for c in &n.children {
                self.collect(*c, removed);
            }
        }
    }

    fn reconcile(
        &mut self,
        old: Option<NodeId>,
        new: &Node,
        out: &mut Vec<Patch>,
        removed: &mut Vec<NodeId>,
    ) -> NodeId {
        if let Some(oid) = old {
            let o = &self.tree.nodes[&oid];
            if o.kind == new.kind && o.key == new.key {
                let set: Props = new
                    .props
                    .iter()
                    .filter(|(k, v)| o.props.get(*k) != Some(*v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let unset: Vec<String> = o
                    .props
                    .keys()
                    .filter(|k| !new.props.contains_key(*k))
                    .cloned()
                    .collect();
                let old_children = o.children.clone();
                if !set.is_empty() || !unset.is_empty() {
                    out.push(Patch::Update {
                        id: oid,
                        set,
                        unset,
                    });
                }
                let ids = self.reconcile_children(&old_children, &new.children, out, removed);
                if ids != old_children {
                    out.push(Patch::Children {
                        id: oid,
                        children: ids,
                    });
                }
                return oid;
            }
            self.collect(oid, removed);
        }
        let id = self.fresh();
        out.push(Patch::Create {
            id,
            kind: new.kind.clone(),
            key: new.key.clone(),
            props: new.props.clone(),
        });
        let ids: Vec<NodeId> = new
            .children
            .iter()
            .map(|c| self.reconcile(None, c, out, removed))
            .collect();
        if !ids.is_empty() {
            out.push(Patch::Children { id, children: ids });
        }
        id
    }

    fn reconcile_children(
        &mut self,
        old: &[NodeId],
        new: &[Node],
        out: &mut Vec<Patch>,
        removed: &mut Vec<NodeId>,
    ) -> Vec<NodeId> {
        let mut keyed: HashMap<String, NodeId> = HashMap::new();
        let mut unkeyed: std::collections::VecDeque<NodeId> = Default::default();
        for id in old {
            match &self.tree.nodes[id].key {
                Some(k) => {
                    keyed.insert(k.clone(), *id);
                }
                None => unkeyed.push_back(*id),
            }
        }
        let ids = new
            .iter()
            .map(|child| {
                let matched = match &child.key {
                    Some(k) => keyed.remove(k),
                    None => unkeyed.pop_front(),
                };
                self.reconcile(matched, child, out, removed)
            })
            .collect();
        for id in keyed.into_values().chain(unkeyed) {
            self.collect(id, removed);
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Node {
        Node::new("Text").prop("text", s)
    }

    fn keyed(k: &str, s: &str) -> Node {
        let mut n = text(s);
        n.key = Some(k.into());
        n
    }

    fn row(children: Vec<Node>) -> Node {
        children.into_iter().fold(Node::new("Row"), Node::child)
    }

    #[test]
    fn identical_trees_produce_no_patches() {
        let mut r = Reconciler::new();
        let t = row(vec![text("a"), text("b")]);
        assert_eq!(
            r.update(&t).len(),
            1 + 2 + 1 + 1,
            "create row, 2 texts, children, root"
        );
        assert!(r.update(&t).is_empty());
    }

    #[test]
    fn prop_change_is_a_single_update() {
        let mut r = Reconciler::new();
        r.update(&row(vec![text("a"), text("b")]));
        let p = r.update(&row(vec![text("a"), text("c")]));
        assert!(
            matches!(&p[..], [Patch::Update { set, unset, .. }] if set["text"] == "c" && unset.is_empty()),
            "{p:?}"
        );
    }

    #[test]
    fn keyed_reorder_moves_without_recreating() {
        let mut r = Reconciler::new();
        r.update(&row(vec![
            keyed("1", "a"),
            keyed("2", "b"),
            keyed("3", "c"),
        ]));
        let p = r.update(&row(vec![
            keyed("3", "c"),
            keyed("1", "a"),
            keyed("2", "b"),
        ]));
        assert!(matches!(&p[..], [Patch::Children { .. }]), "{p:?}");
    }

    #[test]
    fn kind_change_replaces_the_subtree() {
        let mut r = Reconciler::new();
        r.update(&row(vec![row(vec![text("x"), text("y")])]));
        let p = r.update(&row(vec![text("x")]));
        let removed = p.iter().find_map(|p| {
            if let Patch::Remove { ids } = p {
                Some(ids.len())
            } else {
                None
            }
        });
        assert_eq!(removed, Some(3), "inner row and its two texts: {p:?}");
    }

    #[test]
    fn snapshot_rebuilds_the_tree() {
        let mut r = Reconciler::new();
        let t = row(vec![keyed("k", "a"), row(vec![text("b")])]);
        r.update(&t);
        let mut late = ViewTree::default();
        late.apply(&r.snapshot()).unwrap();
        assert_eq!(late.to_node().unwrap(), t);
    }

    /// Random edits: a renderer applying the patch stream always ends up with
    /// exactly the tree the reconciler saw, and never loses ids it still uses.
    #[test]
    fn patch_stream_reproduces_every_tree() {
        let mut seed: u64 = 0x9e3779b97f4a7c15;
        let mut rand = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        fn make_tree(rand: &mut dyn FnMut(u64) -> u64, depth: u32) -> Node {
            let kinds = ["Text", "Pill", "Row", "Column"];
            let kind = kinds[rand(if depth > 2 { 2 } else { 4 }) as usize];
            let mut n = Node::new(kind);
            match kind {
                "Text" => n = n.prop("text", format!("t{}", rand(3))),
                "Pill" => {
                    n = n
                        .prop("label", format!("p{}", rand(3)))
                        .prop("active", rand(2) == 0)
                }
                _ => {
                    for _ in 0..rand(5) {
                        n = n.child(make_tree(rand, depth + 1));
                    }
                }
            }
            if rand(3) == 0 {
                n.key = Some(format!("k{}", rand(4)));
            }
            n
        }
        fn dedup_keys(n: &mut Node) {
            let mut seen = std::collections::HashSet::new();
            for c in &mut n.children {
                if let Some(k) = &c.key {
                    if !seen.insert(k.clone()) {
                        c.key = None;
                    }
                }
                dedup_keys(c);
            }
        }
        let mut r = Reconciler::new();
        let mut view = ViewTree::default();
        for _ in 0..500 {
            let mut t = make_tree(&mut rand, 0);
            dedup_keys(&mut t);
            let patches = r.update(&t);
            view.apply(&patches).unwrap();
            assert_eq!(view.to_node().as_ref(), Some(&t));
            assert_eq!(view.len(), r.tree().len(), "no leaked nodes");
        }
    }
}
