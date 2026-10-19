use alloc::string::String;
use alloc::vec::Vec;

use crate::{ReadError, Reader, ValueTag};

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub name: String,
    pub values: Vec<Value>,
}

impl Attribute {
    pub fn new(name: &str, values: impl Into<Vec<Value>>) -> Self {
        Self { name: String::from(name), values: values.into() }
    }

    pub fn first(&self) -> Option<&Value> {
        self.values.first()
    }
}

/// String-valued syntaxes that share the plain UTF-8 wire encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StrTag {
    Text = ValueTag::TextWithoutLanguage as u8,
    Name = ValueTag::NameWithoutLanguage as u8,
    Keyword = ValueTag::Keyword as u8,
    Uri = ValueTag::Uri as u8,
    UriScheme = ValueTag::UriScheme as u8,
    Charset = ValueTag::Charset as u8,
    NaturalLanguage = ValueTag::NaturalLanguage as u8,
    MimeMediaType = ValueTag::MimeMediaType as u8,
}

impl StrTag {
    fn from_u8(v: u8) -> Option<Self> {
        use StrTag::*;
        [Text, Name, Keyword, Uri, UriScheme, Charset, NaturalLanguage, MimeMediaType]
            .into_iter()
            .find(|t| *t as u8 == v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResolutionUnits {
    DotsPerInch = 3,
    DotsPerCm = 4,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Integer(i32),
    Boolean(bool),
    Enum(i32),
    Str(StrTag, String),
    Resolution { x: i32, y: i32, units: u8 },
    Range { lower: i32, upper: i32 },
    DateTime([u8; 11]),
    Collection(Vec<Attribute>),
    /// unsupported / unknown / no-value.
    OutOfBand(u8),
    /// octetString, *WithLanguage and unregistered tags, kept as raw bytes.
    Other(u8, Vec<u8>),
}

impl Value {
    pub fn str(tag: StrTag, s: &str) -> Self {
        Value::Str(tag, String::from(s))
    }

    pub fn keyword(s: &str) -> Self {
        Self::str(StrTag::Keyword, s)
    }

    pub fn text(s: &str) -> Self {
        Self::str(StrTag::Text, s)
    }

    pub fn name(s: &str) -> Self {
        Self::str(StrTag::Name, s)
    }

    pub fn mime(s: &str) -> Self {
        Self::str(StrTag::MimeMediaType, s)
    }

    pub fn uri(s: &str) -> Self {
        Self::str(StrTag::Uri, s)
    }

    pub fn dpi(x: i32, y: i32) -> Self {
        Value::Resolution { x, y, units: ResolutionUnits::DotsPerInch as u8 }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(_, s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i32> {
        match self {
            Value::Integer(v) | Value::Enum(v) => Some(*v),
            _ => None,
        }
    }

    fn tag(&self) -> u8 {
        match self {
            Value::Integer(_) => ValueTag::Integer as u8,
            Value::Boolean(_) => ValueTag::Boolean as u8,
            Value::Enum(_) => ValueTag::Enum as u8,
            Value::Str(t, _) => *t as u8,
            Value::Resolution { .. } => ValueTag::Resolution as u8,
            Value::Range { .. } => ValueTag::RangeOfInteger as u8,
            Value::DateTime(_) => ValueTag::DateTime as u8,
            Value::Collection(_) => ValueTag::BegCollection as u8,
            Value::OutOfBand(t) | Value::Other(t, _) => *t,
        }
    }
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u16).to_be_bytes());
    out.extend_from_slice(b);
}

fn encode_value(name: &str, value: &Value, out: &mut Vec<u8>) {
    out.push(value.tag());
    put_bytes(out, name.as_bytes());
    match value {
        Value::Integer(v) | Value::Enum(v) => put_bytes(out, &v.to_be_bytes()),
        Value::Boolean(b) => put_bytes(out, &[*b as u8]),
        Value::Str(_, s) => put_bytes(out, s.as_bytes()),
        Value::Resolution { x, y, units } => {
            out.extend_from_slice(&9u16.to_be_bytes());
            out.extend_from_slice(&x.to_be_bytes());
            out.extend_from_slice(&y.to_be_bytes());
            out.push(*units);
        }
        Value::Range { lower, upper } => {
            out.extend_from_slice(&8u16.to_be_bytes());
            out.extend_from_slice(&lower.to_be_bytes());
            out.extend_from_slice(&upper.to_be_bytes());
        }
        Value::DateTime(d) => put_bytes(out, d),
        Value::OutOfBand(_) => put_bytes(out, &[]),
        Value::Other(_, b) => put_bytes(out, b),
        Value::Collection(members) => {
            put_bytes(out, &[]);
            for member in members {
                out.push(ValueTag::MemberAttrName as u8);
                put_bytes(out, &[]);
                put_bytes(out, member.name.as_bytes());
                for v in &member.values {
                    encode_value("", v, out);
                }
            }
            out.push(ValueTag::EndCollection as u8);
            put_bytes(out, &[]);
            put_bytes(out, &[]);
        }
    }
}

pub(crate) fn encode_attribute(attr: &Attribute, out: &mut Vec<u8>) {
    for (i, v) in attr.values.iter().enumerate() {
        encode_value(if i == 0 { &attr.name } else { "" }, v, out);
    }
}

fn be_i32(b: &[u8]) -> i32 {
    i32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Reads one `value-tag name-length name value-length value` record.
fn read_raw<'a>(r: &mut Reader<'a>) -> Result<(u8, &'a [u8], &'a [u8]), ReadError> {
    let tag = r.u8()?;
    let name_len = r.u16()? as usize;
    let name = r.take(name_len)?;
    let value_len = r.u16()? as usize;
    let value = r.take(value_len)?;
    Ok((tag, name, value))
}

fn decode_value(r: &mut Reader, tag: u8, v: &[u8]) -> Result<Value, ReadError> {
    let fixed = |n: usize| if v.len() == n { Ok(()) } else { Err(ReadError::Malformed) };
    Ok(match tag {
        t if t == ValueTag::Integer as u8 => {
            fixed(4)?;
            Value::Integer(be_i32(v))
        }
        t if t == ValueTag::Enum as u8 => {
            fixed(4)?;
            Value::Enum(be_i32(v))
        }
        t if t == ValueTag::Boolean as u8 => {
            fixed(1)?;
            Value::Boolean(v[0] != 0)
        }
        t if t == ValueTag::Resolution as u8 => {
            fixed(9)?;
            Value::Resolution { x: be_i32(&v[0..4]), y: be_i32(&v[4..8]), units: v[8] }
        }
        t if t == ValueTag::RangeOfInteger as u8 => {
            fixed(8)?;
            Value::Range { lower: be_i32(&v[0..4]), upper: be_i32(&v[4..8]) }
        }
        t if t == ValueTag::DateTime as u8 => {
            fixed(11)?;
            Value::DateTime(v.try_into().unwrap())
        }
        t if t == ValueTag::BegCollection as u8 => Value::Collection(parse_collection(r)?),
        t if t == ValueTag::EndCollection as u8 || t == ValueTag::MemberAttrName as u8 => {
            return Err(ReadError::Malformed);
        }
        0x10..=0x1f => Value::OutOfBand(tag),
        t => match (StrTag::from_u8(t), core::str::from_utf8(v)) {
            (Some(st), Ok(s)) => Value::Str(st, String::from(s)),
            _ => Value::Other(tag, v.to_vec()),
        },
    })
}

fn parse_collection(r: &mut Reader) -> Result<Vec<Attribute>, ReadError> {
    let mut members: Vec<Attribute> = Vec::new();
    loop {
        let (tag, name, value) = read_raw(r)?;
        if !name.is_empty() {
            return Err(ReadError::Malformed);
        }
        if tag == ValueTag::EndCollection as u8 {
            return Ok(members);
        }
        if tag == ValueTag::MemberAttrName as u8 {
            let name = core::str::from_utf8(value).map_err(|_| ReadError::Malformed)?;
            members.push(Attribute::new(name, Vec::new()));
        } else {
            let member = members.last_mut().ok_or(ReadError::Malformed)?;
            member.values.push(decode_value(r, tag, value)?);
        }
    }
}

/// Parses one value; a nameless value is appended to the previous attribute.
pub(crate) fn parse_attribute_into(
    r: &mut Reader,
    attrs: &mut Vec<Attribute>,
) -> Result<(), ReadError> {
    let (tag, name, value) = read_raw(r)?;
    let value = decode_value(r, tag, value)?;
    if name.is_empty() {
        attrs.last_mut().ok_or(ReadError::Malformed)?.values.push(value);
    } else {
        let name = core::str::from_utf8(name).map_err(|_| ReadError::Malformed)?;
        attrs.push(Attribute::new(name, alloc::vec![value]));
    }
    Ok(())
}
