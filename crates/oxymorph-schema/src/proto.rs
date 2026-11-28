//! Protobuf interchange: the IR to/from upstream's wire format
//! (`ProtoModel.to_proto_bytes()`), from `decisions/0006`/`0010`.
//!
//! This file is the schema. Like the IR, it is hand-written (`decisions/0006`): no
//! `.proto` is compiled and nothing is generated. Each message's `encode`/`decode`
//! pair names its field numbers, which are upstream morphnet's
//! (`vendor/morphnet/protos/`) plus the fields kfnetlist added (`decisions/0011`).
//! Wire names that differ from the IR's are noted where they occur (`ports` are
//! `terminals`, `port_name` is `terminal_name`).
//!
//! Fields are written in field-number order and default scalars are left out, as
//! prost and Python's runtime do. Maps are written and read in the IR's order. A
//! singular field read twice keeps the last value, a oneof its last member.

use crate::*;

/// How deep a message or map entry may nest below the `Circuit` before decoding fails:
/// prost's default, which [`MAX_EXPRESSION_DEPTH`] is sized against.
pub const MAX_MESSAGE_DEPTH: u32 = 100;

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

const VARINT: u8 = 0;
const I64: u8 = 1;
const LEN: u8 = 2;

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.0.push(value as u8 | 0x80);
            value >>= 7;
        }
        self.0.push(value as u8);
    }

    fn key(&mut self, field: u32, wire_type: u8) {
        self.varint(u64::from(field) << 3 | u64::from(wire_type));
    }

    fn bytes(&mut self, field: u32, bytes: &[u8]) {
        self.key(field, LEN);
        self.varint(bytes.len() as u64);
        self.0.extend_from_slice(bytes);
    }

    fn string(&mut self, field: u32, value: &str) {
        if !value.is_empty() {
            self.bytes(field, value.as_bytes());
        }
    }

    fn strings(&mut self, field: u32, values: &[String]) {
        for value in values {
            self.bytes(field, value.as_bytes());
        }
    }

    fn int32(&mut self, field: u32, value: i32) {
        if value != 0 {
            self.key(field, VARINT);
            self.varint(i64::from(value) as u64);
        }
    }

    fn uint32(&mut self, field: u32, value: u32) {
        if value != 0 {
            self.key(field, VARINT);
            self.varint(u64::from(value));
        }
    }

    fn double(&mut self, field: u32, value: f64) {
        if value != 0.0 {
            self.key(field, I64);
            self.0.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn packed_doubles(&mut self, field: u32, values: &[f64]) {
        if !values.is_empty() {
            self.key(field, LEN);
            self.varint(8 * values.len() as u64);
            for value in values {
                self.0.extend_from_slice(&value.to_le_bytes());
            }
        }
    }

    // ponytail: each level encodes into its own buffer and is copied into its parent,
    // so bytes are copied once per nesting level; size-prefix in one pass if that shows.
    fn message<T>(&mut self, field: u32, item: &T, encode: fn(&T, &mut Writer)) {
        let mut inner = Writer::default();
        encode(item, &mut inner);
        self.bytes(field, &inner.0);
    }

    fn optional<T>(&mut self, field: u32, item: Option<&T>, encode: fn(&T, &mut Writer)) {
        if let Some(item) = item {
            self.message(field, item, encode);
        }
    }

    fn messages<T>(&mut self, field: u32, items: &[T], encode: fn(&T, &mut Writer)) {
        for item in items {
            self.message(field, item, encode);
        }
    }

    /// A `map<string, string>`: one `{key = 1, value = 2}` entry per item.
    fn properties(&mut self, field: u32, map: &Properties) {
        for (key, value) in map {
            let mut entry = Writer::default();
            entry.string(1, key);
            entry.string(2, value);
            self.bytes(field, &entry.0);
        }
    }

    /// A `map<string, Message>`. As in prost, an entry leaves out a default (empty) value.
    fn message_map<T>(
        &mut self,
        field: u32,
        map: &IndexMap<String, T>,
        encode: fn(&T, &mut Writer),
    ) {
        for (key, value) in map {
            let mut entry = Writer::default();
            entry.string(1, key);
            let mut inner = Writer::default();
            encode(value, &mut inner);
            if !inner.0.is_empty() {
                entry.bytes(2, &inner.0);
            }
            self.bytes(field, &entry.0);
        }
    }
}

/// One field's value as read off the wire.
enum Value<'a> {
    Varint(u64),
    I64(u64),
    Len(&'a [u8]),
    I32,
}

const WRONG_WIRE_TYPE: &str = "invalid wire type for a known field";
const TRUNCATED: &str = "truncated protobuf message";

impl<'a> Value<'a> {
    fn varint(self) -> Result<u64, String> {
        match self {
            Value::Varint(value) => Ok(value),
            _ => Err(WRONG_WIRE_TYPE.into()),
        }
    }

    /// `int32` and enums: the low 32 bits, as every protobuf runtime reads them.
    fn int32(self) -> Result<i32, String> {
        Ok(self.varint()? as i32)
    }

    fn uint32(self) -> Result<u32, String> {
        Ok(self.varint()? as u32)
    }

    fn double(self) -> Result<f64, String> {
        match self {
            Value::I64(bits) => Ok(f64::from_bits(bits)),
            _ => Err(WRONG_WIRE_TYPE.into()),
        }
    }

    fn bytes(self) -> Result<&'a [u8], String> {
        match self {
            Value::Len(bytes) => Ok(bytes),
            _ => Err(WRONG_WIRE_TYPE.into()),
        }
    }

    fn string(self) -> Result<String, String> {
        std::str::from_utf8(self.bytes()?)
            .map(str::to_owned)
            .map_err(|_| "invalid UTF-8 in a string field".to_string())
    }

    /// A message nested one level below `depth`.
    fn message<T>(
        self,
        depth: u32,
        decode: fn(&[u8], u32) -> Result<T, String>,
    ) -> Result<T, String> {
        let depth = nest(depth)?;
        decode(self.bytes()?, depth)
    }

    fn boxed(self, depth: u32) -> Result<Option<Box<Expression>>, String> {
        Ok(Some(Box::new(self.message(depth, Expression::decode)?)))
    }
}

fn nest(depth: u32) -> Result<u32, String> {
    if depth >= MAX_MESSAGE_DEPTH {
        Err("protobuf recursion limit reached".into())
    } else {
        Ok(depth + 1)
    }
}

fn varint(buf: &mut &[u8]) -> Result<u64, String> {
    let mut value = 0;
    for shift in (0..70).step_by(7) {
        let (&byte, rest) = buf.split_first().ok_or(TRUNCATED)?;
        *buf = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(value);
        }
    }
    Err("varint longer than 10 bytes".into())
}

fn take<'a>(buf: &mut &'a [u8], len: usize) -> Result<&'a [u8], String> {
    if buf.len() < len {
        return Err(TRUNCATED.into());
    }
    let (head, rest) = buf.split_at(len);
    *buf = rest;
    Ok(head)
}

/// Read every field of a message, skipping the field numbers `field` ignores.
fn fields<'a>(
    mut buf: &'a [u8],
    mut field: impl FnMut(u32, Value<'a>) -> Result<(), String>,
) -> Result<(), String> {
    while !buf.is_empty() {
        let key = varint(&mut buf)?;
        let number = u32::try_from(key >> 3)
            .ok()
            .filter(|number| (1..1 << 29).contains(number))
            .ok_or("invalid field number")?;
        let value = match key & 7 {
            0 => Value::Varint(varint(&mut buf)?),
            1 => {
                let mut bits = [0; 8];
                bits.copy_from_slice(take(&mut buf, 8)?);
                Value::I64(u64::from_le_bytes(bits))
            }
            2 => {
                let len = usize::try_from(varint(&mut buf)?).map_err(|_| TRUNCATED)?;
                Value::Len(take(&mut buf, len)?)
            }
            5 => {
                take(&mut buf, 4)?;
                Value::I32
            }
            wire_type => return Err(format!("unsupported wire type {wire_type}")),
        };
        field(number, value)?;
    }
    Ok(())
}

/// A `repeated double`, packed (as written) or not (as parsers must also accept).
fn doubles(values: &mut Vec<f64>, value: Value) -> Result<(), String> {
    if let Value::Len(bytes) = value {
        let (chunks, rest) = bytes.as_chunks::<8>();
        if !rest.is_empty() {
            return Err(TRUNCATED.into());
        }
        values.extend(chunks.iter().map(|chunk| f64::from_le_bytes(*chunk)));
    } else {
        values.push(value.double()?);
    }
    Ok(())
}

/// One `{key = 1, value = 2}` map entry, a message of its own one level down.
/// A repeated key keeps its first position and its last value.
fn entry<V: Default>(
    map: &mut IndexMap<String, V>,
    value: Value,
    depth: u32,
    read: fn(Value, u32) -> Result<V, String>,
) -> Result<(), String> {
    let depth = nest(depth)?;
    let (mut key, mut item) = (String::new(), V::default());
    fields(value.bytes()?, |field, value| {
        match field {
            1 => key = value.string()?,
            2 => item = read(value, depth)?,
            _ => {}
        }
        Ok(())
    })?;
    map.insert(key, item);
    Ok(())
}

fn property(map: &mut Properties, value: Value, depth: u32) -> Result<(), String> {
    entry(map, value, depth, |value, _| value.string())
}

fn parameter_entry(
    map: &mut IndexMap<String, ParameterValue>,
    value: Value,
    depth: u32,
) -> Result<(), String> {
    entry(map, value, depth, |value, depth| {
        value.message(depth, ParameterValue::decode)
    })
}

// ---------------------------------------------------------------------------
// Enum conversion (numeric values match the proto enums 1:1)
// ---------------------------------------------------------------------------

fn pdir_from(v: i32) -> PortDirection {
    match v {
        1 => PortDirection::Input,
        2 => PortDirection::Output,
        _ => PortDirection::Inout,
    }
}
fn sdom_from(v: i32) -> SignalDomain {
    match v {
        1 => SignalDomain::Electrical,
        2 => SignalDomain::Waveguide,
        _ => SignalDomain::Unspecified,
    }
}
fn siprefix_from(v: i32) -> Siprefix {
    match v {
        1 => Siprefix::Quecto,
        2 => Siprefix::Ronto,
        3 => Siprefix::Yocto,
        4 => Siprefix::Zepto,
        5 => Siprefix::Atto,
        6 => Siprefix::Femto,
        7 => Siprefix::Pico,
        8 => Siprefix::Nano,
        9 => Siprefix::Micro,
        10 => Siprefix::Milli,
        11 => Siprefix::Centi,
        12 => Siprefix::Deci,
        13 => Siprefix::Deca,
        14 => Siprefix::Hecto,
        15 => Siprefix::Kilo,
        16 => Siprefix::Mega,
        17 => Siprefix::Giga,
        18 => Siprefix::Tera,
        19 => Siprefix::Peta,
        20 => Siprefix::Exa,
        21 => Siprefix::Zetta,
        22 => Siprefix::Yotta,
        23 => Siprefix::Ronna,
        24 => Siprefix::Quetta,
        _ => Siprefix::Unspecified,
    }
}
fn extkind_from(v: i32) -> ExternalModuleKind {
    match v {
        1 => ExternalModuleKind::Device,
        2 => ExternalModuleKind::Model,
        3 => ExternalModuleKind::Nature,
        4 => ExternalModuleKind::Discipline,
        _ => ExternalModuleKind::Unspecified,
    }
}
fn dkind_from(v: i32) -> DirectiveKind {
    match v {
        1 => DirectiveKind::Include,
        2 => DirectiveKind::Lib,
        3 => DirectiveKind::Global,
        4 => DirectiveKind::Option,
        5 => DirectiveKind::Param,
        6 => DirectiveKind::Define,
        7 => DirectiveKind::Timescale,
        _ => DirectiveKind::Unspecified,
    }
}
fn akind_from(v: i32) -> AnalysisKind {
    match v {
        1 => AnalysisKind::Op,
        2 => AnalysisKind::Dc,
        3 => AnalysisKind::Ac,
        4 => AnalysisKind::Tran,
        5 => AnalysisKind::Noise,
        6 => AnalysisKind::Tf,
        7 => AnalysisKind::Sens,
        8 => AnalysisKind::Pz,
        9 => AnalysisKind::Disto,
        10 => AnalysisKind::Four,
        11 => AnalysisKind::Fft,
        _ => AnalysisKind::Unspecified,
    }
}
fn operator_from(value: i32) -> Operator {
    match value {
        1 => Operator::Add,
        2 => Operator::Sub,
        3 => Operator::Mul,
        4 => Operator::Div,
        5 => Operator::Pow,
        6 => Operator::Mod,
        7 => Operator::Neg,
        8 => Operator::Not,
        9 => Operator::Eq,
        10 => Operator::Ne,
        11 => Operator::Lt,
        12 => Operator::Le,
        13 => Operator::Gt,
        14 => Operator::Ge,
        15 => Operator::And,
        16 => Operator::Or,
        _ => Operator::Unspecified,
    }
}
fn waveform_kind_from(value: i32) -> WaveformKind {
    match value {
        1 => WaveformKind::Pulse,
        2 => WaveformKind::Sin,
        3 => WaveformKind::Exp,
        4 => WaveformKind::Sffm,
        5 => WaveformKind::Pwl,
        _ => WaveformKind::Unspecified,
    }
}
fn okind_from(v: i32) -> OutputRequestKind {
    match v {
        1 => OutputRequestKind::Print,
        2 => OutputRequestKind::Plot,
        3 => OutputRequestKind::Probe,
        4 => OutputRequestKind::Save,
        _ => OutputRequestKind::Unspecified,
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

impl PrefixedValue {
    fn encode(&self, w: &mut Writer) {
        w.double(1, self.double_value);
        w.int32(2, self.prefix as i32);
    }
    fn decode(buf: &[u8], _depth: u32) -> Result<Self, String> {
        let mut out = PrefixedValue::default();
        fields(buf, |field, value| {
            match field {
                1 => out.double_value = value.double()?,
                2 => out.prefix = siprefix_from(value.int32()?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl ParameterValue {
    /// `oneof value`: the first member set, in field order.
    fn encode(&self, w: &mut Writer) {
        if let Some(prefixed_value) = &self.prefixed_value {
            w.message(1, prefixed_value, PrefixedValue::encode);
        } else if let Some(model_ref) = &self.model_ref {
            w.message(2, model_ref, ModelReference::encode);
        } else if let Some(expression) = &self.expression {
            w.message(3, expression, Expression::encode);
        }
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = ParameterValue::default();
        fields(buf, |field, value| {
            out = match field {
                1 => ParameterValue {
                    prefixed_value: Some(value.message(depth, PrefixedValue::decode)?),
                    ..Default::default()
                },
                2 => ParameterValue {
                    model_ref: Some(value.message(depth, ModelReference::decode)?),
                    ..Default::default()
                },
                3 => ParameterValue {
                    expression: Some(value.message(depth, Expression::decode)?),
                    ..Default::default()
                },
                _ => return Ok(()),
            };
            Ok(())
        })?;
        Ok(out)
    }
}

impl UnaryOp {
    fn encode(&self, w: &mut Writer) {
        w.int32(1, self.op as i32);
        w.optional(2, self.operand.as_deref(), Expression::encode);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = UnaryOp::default();
        fields(buf, |field, value| {
            match field {
                1 => out.op = operator_from(value.int32()?),
                2 => out.operand = value.boxed(depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl BinaryOp {
    fn encode(&self, w: &mut Writer) {
        w.int32(1, self.op as i32);
        w.optional(2, self.lhs.as_deref(), Expression::encode);
        w.optional(3, self.rhs.as_deref(), Expression::encode);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = BinaryOp::default();
        fields(buf, |field, value| {
            match field {
                1 => out.op = operator_from(value.int32()?),
                2 => out.lhs = value.boxed(depth)?,
                3 => out.rhs = value.boxed(depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Call {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.messages(2, &self.args, Expression::encode);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Call::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                2 => out.args.push(value.message(depth, Expression::decode)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Ternary {
    fn encode(&self, w: &mut Writer) {
        w.optional(1, self.cond.as_deref(), Expression::encode);
        w.optional(2, self.then.as_deref(), Expression::encode);
        w.optional(3, self.otherwise.as_deref(), Expression::encode);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Ternary::default();
        fields(buf, |field, value| {
            match field {
                1 => out.cond = value.boxed(depth)?,
                2 => out.then = value.boxed(depth)?,
                3 => out.otherwise = value.boxed(depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Expression {
    /// `oneof node`: the first node set, in field order (an invalid tree keeps one node).
    fn encode(&self, w: &mut Writer) {
        if let Some(literal) = &self.literal {
            w.message(1, literal, PrefixedValue::encode);
        } else if let Some(reference) = &self.reference {
            w.bytes(2, reference.as_bytes());
        } else if let Some(unary) = &self.unary {
            w.message(3, unary, UnaryOp::encode);
        } else if let Some(binary) = &self.binary {
            w.message(4, binary, BinaryOp::encode);
        } else if let Some(call) = &self.call {
            w.message(5, call, Call::encode);
        } else if let Some(ternary) = &self.ternary {
            w.message(6, ternary, Ternary::encode);
        } else if let Some(raw) = &self.raw {
            w.bytes(7, raw.as_bytes());
        }
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Expression::default();
        fields(buf, |field, value| {
            out = match field {
                1 => Expression {
                    literal: Some(value.message(depth, PrefixedValue::decode)?),
                    ..Default::default()
                },
                2 => Expression {
                    reference: Some(value.string()?),
                    ..Default::default()
                },
                3 => Expression {
                    unary: Some(value.message(depth, UnaryOp::decode)?),
                    ..Default::default()
                },
                4 => Expression {
                    binary: Some(value.message(depth, BinaryOp::decode)?),
                    ..Default::default()
                },
                5 => Expression {
                    call: Some(value.message(depth, Call::decode)?),
                    ..Default::default()
                },
                6 => Expression {
                    ternary: Some(value.message(depth, Ternary::decode)?),
                    ..Default::default()
                },
                7 => Expression {
                    raw: Some(value.string()?),
                    ..Default::default()
                },
                _ => return Ok(()),
            };
            Ok(())
        })?;
        Ok(out)
    }
}

impl ModelReference {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.model_interface_name);
        w.message_map(10, &self.arguments, ParameterValue::encode);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = ModelReference::default();
        fields(buf, |field, value| {
            match field {
                1 => out.model_interface_name = value.string()?,
                10 => parameter_entry(&mut out.arguments, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Parameter {
    /// Field 1 (`uid`) is reserved.
    fn encode(&self, w: &mut Writer) {
        w.string(10, &self.name);
        w.optional(20, self.default_value.as_ref(), ParameterValue::encode);
        w.string(30, &self.description);
        w.properties(40, &self.properties);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Parameter::default();
        fields(buf, |field, value| {
            match field {
                10 => out.name = value.string()?,
                20 => out.default_value = Some(value.message(depth, ParameterValue::decode)?),
                30 => out.description = value.string()?,
                40 => property(&mut out.properties, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Directive {
    fn encode(&self, w: &mut Writer) {
        w.int32(1, self.kind as i32);
        w.string(2, &self.name);
        w.string(3, &self.value);
    }
    fn decode(buf: &[u8], _depth: u32) -> Result<Self, String> {
        let mut out = Directive::default();
        fields(buf, |field, value| {
            match field {
                1 => out.kind = dkind_from(value.int32()?),
                2 => out.name = value.string()?,
                3 => out.value = value.string()?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Macro {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.string(2, &self.language);
        w.string(3, &self.body);
        w.properties(4, &self.properties);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Macro::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                2 => out.language = value.string()?,
                3 => out.body = value.string()?,
                4 => property(&mut out.properties, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl ModelInterface {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.string(2, &self.function_name);
        w.messages(10, &self.parameters, Parameter::encode);
        w.properties(20, &self.properties);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = ModelInterface::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                2 => out.function_name = value.string()?,
                10 => out
                    .parameters
                    .push(value.message(depth, Parameter::decode)?),
                20 => property(&mut out.properties, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Port {
    /// The wire's `Terminal`. Fields 1 (`uid`) and 50 (`cross_section`) are reserved.
    fn encode(&self, w: &mut Writer) {
        w.string(10, &self.name);
        w.int32(20, self.direction as i32);
        w.int32(30, self.domain as i32);
        w.uint32(40, self.width);
        w.properties(60, &self.properties);
        w.string(61, &self.discipline);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Port::default();
        fields(buf, |field, value| {
            match field {
                10 => out.name = value.string()?,
                20 => out.direction = pdir_from(value.int32()?),
                30 => out.domain = sdom_from(value.int32()?),
                40 => out.width = value.uint32()?,
                60 => property(&mut out.properties, value, depth)?,
                61 => out.discipline = value.string()?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl PortReference {
    /// The wire's `TerminalReference`; `port_name` is `terminal_name`.
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.instance_name);
        w.string(2, &self.port_name);
    }
    fn decode(buf: &[u8], _depth: u32) -> Result<Self, String> {
        let mut out = PortReference::default();
        fields(buf, |field, value| {
            match field {
                1 => out.instance_name = value.string()?,
                2 => out.port_name = value.string()?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Connection {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.optional(10, self.source.as_ref(), PortReference::encode);
        w.optional(20, self.target.as_ref(), PortReference::encode);
        w.int32(30, self.domain as i32);
        w.uint32(40, self.weight);
        w.properties(50, &self.properties);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Connection::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                10 => out.source = Some(value.message(depth, PortReference::decode)?),
                20 => out.target = Some(value.message(depth, PortReference::decode)?),
                30 => out.domain = sdom_from(value.int32()?),
                40 => out.weight = value.uint32()?,
                50 => property(&mut out.properties, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Bus {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.uint32(10, self.width);
        w.int32(20, self.domain as i32);
        w.messages(30, &self.connections, Connection::encode);
        w.properties(40, &self.properties);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Bus::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                10 => out.width = value.uint32()?,
                20 => out.domain = sdom_from(value.int32()?),
                30 => out
                    .connections
                    .push(value.message(depth, Connection::decode)?),
                40 => property(&mut out.properties, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl ModuleReference {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.string(2, &self.module_name);
        w.string(3, &self.class_name);
        w.messages(10, &self.parameters, Parameter::encode);
        w.properties(30, &self.properties);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = ModuleReference::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                2 => out.module_name = value.string()?,
                3 => out.class_name = value.string()?,
                10 => out
                    .parameters
                    .push(value.message(depth, Parameter::decode)?),
                30 => property(&mut out.properties, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Module {
    /// Field 1 (`uid`) is reserved; `ports` are `terminals`.
    fn encode(&self, w: &mut Writer) {
        w.string(2, &self.name);
        w.string(3, &self.class_name);
        w.messages(10, &self.ports, Port::encode);
        w.messages(20, &self.parameters, Parameter::encode);
        w.messages(30, &self.model_interfaces, ModelInterface::encode);
        w.messages(40, &self.module_references, ModuleReference::encode);
        w.messages(50, &self.connections, Connection::encode);
        w.messages(60, &self.buses, Bus::encode);
        w.properties(70, &self.properties);
        w.messages(71, &self.directives, Directive::encode);
        w.messages(72, &self.macros, Macro::encode);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Module::default();
        fields(buf, |field, value| {
            match field {
                2 => out.name = value.string()?,
                3 => out.class_name = value.string()?,
                10 => out.ports.push(value.message(depth, Port::decode)?),
                20 => out
                    .parameters
                    .push(value.message(depth, Parameter::decode)?),
                30 => out
                    .model_interfaces
                    .push(value.message(depth, ModelInterface::decode)?),
                40 => out
                    .module_references
                    .push(value.message(depth, ModuleReference::decode)?),
                50 => out
                    .connections
                    .push(value.message(depth, Connection::decode)?),
                60 => out.buses.push(value.message(depth, Bus::decode)?),
                70 => property(&mut out.properties, value, depth)?,
                71 => out
                    .directives
                    .push(value.message(depth, Directive::decode)?),
                72 => out.macros.push(value.message(depth, Macro::decode)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl ExternalModule {
    /// A device or model from an external library: `library_name` names an entry of
    /// `Circuit.libraries`, `group_name` the group within it (usually the corner).
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.string(3, &self.class_name);
        w.string(4, &self.library_name);
        w.string(5, &self.group_name);
        w.messages(10, &self.ports, Port::encode);
        w.messages(20, &self.parameters, Parameter::encode);
        w.properties(30, &self.properties);
        w.int32(31, self.kind as i32);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = ExternalModule::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                3 => out.class_name = value.string()?,
                4 => out.library_name = value.string()?,
                5 => out.group_name = value.string()?,
                10 => out.ports.push(value.message(depth, Port::decode)?),
                20 => out
                    .parameters
                    .push(value.message(depth, Parameter::decode)?),
                30 => property(&mut out.properties, value, depth)?,
                31 => out.kind = extkind_from(value.int32()?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl ExternalLibrary {
    /// Field 1 (`uid`) is reserved.
    fn encode(&self, w: &mut Writer) {
        w.string(2, &self.path);
        w.string(3, &self.model_type);
        w.strings(4, &self.external_model_names);
    }
    fn decode(buf: &[u8], _depth: u32) -> Result<Self, String> {
        let mut out = ExternalLibrary::default();
        fields(buf, |field, value| {
            match field {
                2 => out.path = value.string()?,
                3 => out.model_type = value.string()?,
                4 => out.external_model_names.push(value.string()?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Analysis {
    fn encode(&self, w: &mut Writer) {
        w.int32(1, self.kind as i32);
        w.string(2, &self.name);
        w.strings(10, &self.arguments);
        w.properties(20, &self.options);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Analysis::default();
        fields(buf, |field, value| {
            match field {
                1 => out.kind = akind_from(value.int32()?),
                2 => out.name = value.string()?,
                10 => out.arguments.push(value.string()?),
                20 => property(&mut out.options, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl OutputRequest {
    fn encode(&self, w: &mut Writer) {
        w.int32(1, self.kind as i32);
        w.string(2, &self.analysis_type);
        w.strings(10, &self.variables);
    }
    fn decode(buf: &[u8], _depth: u32) -> Result<Self, String> {
        let mut out = OutputRequest::default();
        fields(buf, |field, value| {
            match field {
                1 => out.kind = okind_from(value.int32()?),
                2 => out.analysis_type = value.string()?,
                10 => out.variables.push(value.string()?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Measurement {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.string(2, &self.analysis_type);
        w.string(3, &self.body);
    }
    fn decode(buf: &[u8], _depth: u32) -> Result<Self, String> {
        let mut out = Measurement::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                2 => out.analysis_type = value.string()?,
                3 => out.body = value.string()?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl InitialCondition {
    fn encode(&self, w: &mut Writer) {
        w.properties(1, &self.conditions);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = InitialCondition::default();
        fields(buf, |field, value| {
            if field == 1 {
                property(&mut out.conditions, value, depth)?;
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Waveform {
    fn encode(&self, w: &mut Writer) {
        w.int32(1, self.kind as i32);
        w.message_map(2, &self.args, ParameterValue::encode);
        w.messages(3, &self.points, ParameterValue::encode);
        w.string(
            4,
            &self
                .file
                .as_deref()
                .and_then(stored_file)
                .unwrap_or_default(),
        );
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Waveform::default();
        fields(buf, |field, value| {
            match field {
                1 => out.kind = waveform_kind_from(value.int32()?),
                2 => parameter_entry(&mut out.args, value, depth)?,
                3 => out
                    .points
                    .push(value.message(depth, ParameterValue::decode)?),
                4 => out.file = stored_file(&value.string()?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Stimulus {
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.optional(2, self.dc.as_ref(), ParameterValue::encode);
        w.optional(3, self.ac_magnitude.as_ref(), ParameterValue::encode);
        w.optional(4, self.ac_phase.as_ref(), ParameterValue::encode);
        w.optional(5, self.waveform.as_ref(), Waveform::encode);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Stimulus::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                2 => out.dc = Some(value.message(depth, ParameterValue::decode)?),
                3 => out.ac_magnitude = Some(value.message(depth, ParameterValue::decode)?),
                4 => out.ac_phase = Some(value.message(depth, ParameterValue::decode)?),
                5 => out.waveform = Some(value.message(depth, Waveform::decode)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Simulation {
    fn encode(&self, w: &mut Writer) {
        w.messages(1, &self.analyses, Analysis::encode);
        w.messages(2, &self.output_requests, OutputRequest::encode);
        w.messages(3, &self.measurements, Measurement::encode);
        w.optional(
            4,
            self.initial_conditions.as_ref(),
            InitialCondition::encode,
        );
        w.optional(5, self.node_sets.as_ref(), InitialCondition::encode);
        w.packed_doubles(6, &self.temperatures);
        w.messages(7, &self.stimuli, Stimulus::encode);
        w.properties(10, &self.options);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Simulation::default();
        fields(buf, |field, value| {
            match field {
                1 => out.analyses.push(value.message(depth, Analysis::decode)?),
                2 => out
                    .output_requests
                    .push(value.message(depth, OutputRequest::decode)?),
                3 => out
                    .measurements
                    .push(value.message(depth, Measurement::decode)?),
                4 => out.initial_conditions = Some(value.message(depth, InitialCondition::decode)?),
                5 => out.node_sets = Some(value.message(depth, InitialCondition::decode)?),
                6 => doubles(&mut out.temperatures, value)?,
                7 => out.stimuli.push(value.message(depth, Stimulus::decode)?),
                10 => property(&mut out.options, value, depth)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl Circuit {
    /// `libraries` indexes the external model libraries by name, in the IR's order;
    /// `ExternalModule.library_name` / `group_name` point into it.
    fn encode(&self, w: &mut Writer) {
        w.string(1, &self.name);
        w.string(3, &self.top_module);
        w.messages(10, &self.modules, Module::encode);
        w.messages(20, &self.ext_modules, ExternalModule::encode);
        w.properties(30, &self.properties);
        w.messages(31, &self.directives, Directive::encode);
        w.optional(40, self.simulation.as_ref(), Simulation::encode);
        w.message_map(45, &self.libraries, ExternalLibrary::encode);
        w.messages(46, &self.macros, Macro::encode);
    }
    fn decode(buf: &[u8], depth: u32) -> Result<Self, String> {
        let mut out = Circuit::default();
        fields(buf, |field, value| {
            match field {
                1 => out.name = value.string()?,
                3 => out.top_module = value.string()?,
                10 => out.modules.push(value.message(depth, Module::decode)?),
                20 => out
                    .ext_modules
                    .push(value.message(depth, ExternalModule::decode)?),
                30 => property(&mut out.properties, value, depth)?,
                31 => out
                    .directives
                    .push(value.message(depth, Directive::decode)?),
                40 => out.simulation = Some(value.message(depth, Simulation::decode)?),
                45 => entry(&mut out.libraries, value, depth, |value, depth| {
                    value.message(depth, ExternalLibrary::decode)
                })?,
                46 => out.macros.push(value.message(depth, Macro::decode)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// Serialize to upstream protobuf bytes (`ProtoModel.to_proto_bytes`).
    pub fn to_proto_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        self.encode(&mut w);
        w.0
    }

    /// Deserialize from upstream protobuf bytes (`ProtoModel.from_proto_bytes`).
    pub fn from_proto_bytes(bytes: &[u8]) -> Result<Self, String> {
        Circuit::decode(bytes, 0)
    }
}
