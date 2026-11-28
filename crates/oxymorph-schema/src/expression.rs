//! Parameter expression trees (kfnetlist `value.proto` / `_value.py`).
//!
//! `Expression` mirrors the Pydantic model: one optional field per node kind, at
//! most one set, so its JSON is flat (`{"binary": {...}}`). Unset fields are left
//! out of JSON, as kfnetlist's `to_json` does.

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::{PrefixedValue, Siprefix};

/// The deepest expression tree the parsers build, in nodes from the root to the deepest
/// leaf (`a` is 1, `-a` 2, `f(a + 1)` 3). Deeper text stays `raw`.
///
/// Sized so a tree this deep, held where the IR nests one deepest (a model reference
/// argument: `modules[i].module_references[j].parameters[k].default_value.model_ref
/// .arguments[name].expression`), survives every interchange format. A call is the
/// costliest node and a literal the costliest leaf:
/// - JSON and YAML read at most 127 nested levels. The expression object is level 12; a
///   call takes 3 levels (`{"call": {"args": [`), a literal leaf 2: 11 + 3 × 31 + 2 = 106.
/// - protobuf reads at most 100 nested messages (`proto::MAX_MESSAGE_DEPTH`). The expression is message 8; a
///   call takes 2 messages, a literal leaf 2: 7 + 2 × 31 + 2 = 71.
/// - The Python binding's converter walks at most `MAX_NESTING` (128) attribute and item
///   steps; it asserts at compile time that a tree this deep fits.
pub const MAX_EXPRESSION_DEPTH: usize = 32;

/// Operators of an [`Expression`]: unary `Neg`/`Not`, the rest binary.
#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum Operator {
    #[default]
    Unspecified = 0,
    Add = 1,
    Sub = 2,
    Mul = 3,
    Div = 4,
    Pow = 5,
    Mod = 6,
    Neg = 7,
    Not = 8,
    Eq = 9,
    Ne = 10,
    Lt = 11,
    Le = 12,
    Gt = 13,
    Ge = 14,
    And = 15,
    Or = 16,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct UnaryOp {
    #[serde(default)]
    pub op: Operator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operand: Option<Box<Expression>>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct BinaryOp {
    #[serde(default)]
    pub op: Operator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lhs: Option<Box<Expression>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rhs: Option<Box<Expression>>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Call {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub args: Vec<Expression>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Ternary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cond: Option<Box<Expression>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<Box<Expression>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otherwise: Option<Box<Expression>>,
}

/// A parameter expression tree, dialect-neutral; `raw` is dialect text no
/// grammar parsed (kept verbatim).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Expression {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub literal: Option<PrefixedValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unary: Option<UnaryOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<BinaryOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<Call>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ternary: Option<Ternary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl Expression {
    /// A plain number (no SI prefix).
    pub fn literal(double_value: f64) -> Self {
        Self {
            literal: Some(PrefixedValue {
                double_value,
                prefix: Siprefix::Unspecified,
            }),
            ..Default::default()
        }
    }

    /// A reference to a parameter by name.
    pub fn reference(name: &str) -> Self {
        Self {
            reference: Some(name.to_string()),
            ..Default::default()
        }
    }

    /// Mirrors `Expression._check_oneof` over the whole tree: at most one node
    /// kind per expression.
    pub fn validate(&self) -> Result<(), String> {
        self.check_oneof()?;
        for child in self.children() {
            child.validate()?;
        }
        Ok(())
    }

    /// [`Self::validate`] of this node alone, not of its children.
    pub fn check_oneof(&self) -> Result<(), String> {
        let set_count = self.literal.is_some() as u8
            + self.reference.is_some() as u8
            + self.unary.is_some() as u8
            + self.binary.is_some() as u8
            + self.call.is_some() as u8
            + self.ternary.is_some() as u8
            + self.raw.is_some() as u8;
        if set_count > 1 {
            return Err(
                "Expression is a oneof: set exactly one of 'literal', 'reference', \
                 'unary', 'binary', 'call', 'ternary', 'raw'"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn children(&self) -> Vec<&Expression> {
        let mut children: Vec<&Expression> = Vec::new();
        if let Some(unary) = &self.unary {
            children.extend(unary.operand.as_deref());
        }
        if let Some(binary) = &self.binary {
            children.extend(binary.lhs.as_deref());
            children.extend(binary.rhs.as_deref());
        }
        if let Some(call) = &self.call {
            children.extend(call.args.iter());
        }
        if let Some(ternary) = &self.ternary {
            children.extend(ternary.cond.as_deref());
            children.extend(ternary.then.as_deref());
            children.extend(ternary.otherwise.as_deref());
        }
        children
    }
}
