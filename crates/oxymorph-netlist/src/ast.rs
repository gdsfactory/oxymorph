//! Tree-transform harness mirroring Lark's `Transformer`.
//!
//! `lark_rs` already reproduces Lark's `?rule`/`_rule` tree shaping, so the
//! `items` list each rule receives matches Python Lark exactly. Each format
//! implements [`TreeTransform`] with its own dynamic value type.

use lark_rs::tree::{Child, ParseTree, Token, Tree};

pub trait TreeTransform {
    type Item;

    /// Wrap a leaf token (Python's default token handling).
    fn token(&mut self, tok: &Token) -> Self::Item;

    /// Value for a `None`/maybe-placeholder child.
    fn none(&mut self) -> Self::Item;

    /// Fallback for rules with no explicit handler: rebuild a tree node.
    fn default_rule(&mut self, name: &str, items: Vec<Self::Item>) -> Self::Item;

    /// Handle a named rule. Defaults to [`Self::default_rule`].
    fn rule(&mut self, name: &str, items: Vec<Self::Item>) -> Self::Item {
        self.default_rule(name, items)
    }

    fn transform(&mut self, tree: &ParseTree) -> Self::Item {
        match tree {
            ParseTree::Tree(t) => self.tree(t),
            ParseTree::Token(t) => self.token(t),
            ParseTree::None => self.none(),
        }
    }

    fn tree(&mut self, t: &Tree) -> Self::Item {
        let items: Vec<Self::Item> = t.children.iter().map(|c| self.child(c)).collect();
        self.rule(&t.data, items)
    }

    fn child(&mut self, c: &Child) -> Self::Item {
        match c {
            Child::Tree(t) => self.tree(t),
            Child::Token(t) => self.token(t),
            Child::None => self.none(),
        }
    }
}

/// How deep a parse tree may nest before a recursive [`TreeTransform`] walks it.
pub const MAX_TREE_DEPTH: usize = 256;

/// `Error::Validation` when `tree` nests deeper than [`MAX_TREE_DEPTH`] (untrusted text:
/// `{{{…}}}`, `(((…)))` or a long operator chain), checked without recursing.
pub fn check_tree_depth(tree: &ParseTree) -> Result<(), crate::Error> {
    if is_deeper_than(tree, MAX_TREE_DEPTH) {
        return Err(crate::Error::Validation(format!(
            "the text nests deeper than {MAX_TREE_DEPTH} levels"
        )));
    }
    Ok(())
}

/// Whether `tree` has a branch of more than `limit` rule nodes (tokens do not count),
/// found without recursing.
pub fn is_deeper_than(tree: &ParseTree, limit: usize) -> bool {
    let ParseTree::Tree(root) = tree else {
        return false;
    };
    let mut pending: Vec<(&Tree, usize)> = vec![(root, 1)];
    while let Some((node, depth)) = pending.pop() {
        if depth > limit {
            return true;
        }
        for child in &node.children {
            if let Child::Tree(subtree) = child {
                pending.push((subtree, depth + 1));
            }
        }
    }
    false
}
