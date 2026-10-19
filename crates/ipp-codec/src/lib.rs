//! IPP message encoding (RFC 8010, section 3).
//!
//! [`Message::parse`] works on a prefix of the HTTP body: it returns `Ok(None)` until the
//! whole attribute section has arrived, then the message plus the offset where document
//! data starts. Document data is never buffered here.

#![no_std]

extern crate alloc;

mod codes;
mod value;

pub use codes::{DelimiterTag, Operation, StatusCode, ValueTag};
pub use value::{Attribute, ResolutionUnits, StrTag, Value};

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Bytes that cannot be valid IPP (bad tag, unbalanced collection, wrong length).
    Malformed,
}

/// One attribute group, e.g. operation-attributes or printer-attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub tag: DelimiterTag,
    pub attributes: Vec<Attribute>,
}

impl Group {
    pub fn new(tag: DelimiterTag) -> Self {
        Self { tag, attributes: Vec::new() }
    }

    pub fn get(&self, name: &str) -> Option<&Attribute> {
        self.attributes.iter().find(|a| a.name == name)
    }

    pub fn push(&mut self, name: &str, values: impl Into<Vec<Value>>) {
        self.attributes.push(Attribute { name: String::from(name), values: values.into() });
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub version: (u8, u8),
    /// Operation id in requests, status code in responses.
    pub code: u16,
    pub request_id: u32,
    pub groups: Vec<Group>,
}

impl Message {
    pub fn response(request: &Message, status: StatusCode) -> Self {
        let mut ops = Group::new(DelimiterTag::Operation);
        ops.push("attributes-charset", [Value::str(StrTag::Charset, "utf-8")]);
        ops.push("attributes-natural-language", [Value::str(StrTag::NaturalLanguage, "en")]);
        Self {
            version: request.version,
            code: status as u16,
            request_id: request.request_id,
            groups: alloc::vec![ops],
        }
    }

    pub fn operation(&self) -> Option<Operation> {
        Operation::from_u16(self.code)
    }

    pub fn group(&self, tag: DelimiterTag) -> Option<&Group> {
        self.groups.iter().find(|g| g.tag == tag)
    }

    pub fn group_mut(&mut self, tag: DelimiterTag) -> &mut Group {
        match self.groups.iter().position(|g| g.tag == tag) {
            Some(i) => &mut self.groups[i],
            None => {
                self.groups.push(Group::new(tag));
                self.groups.last_mut().unwrap()
            }
        }
    }

    /// Looks up an operation attribute (the usual place for request parameters).
    pub fn op_attr(&self, name: &str) -> Option<&Attribute> {
        self.group(DelimiterTag::Operation)?.get(name)
    }

    /// Parses a message from the start of `buf`.
    ///
    /// Returns `Ok(None)` if `buf` ends before the end-of-attributes tag, otherwise the
    /// message and the number of bytes consumed (document data starts there).
    pub fn parse(buf: &[u8]) -> Result<Option<(Message, usize)>, Error> {
        let mut r = Reader { buf, pos: 0 };
        match parse_message(&mut r) {
            Ok(m) => Ok(Some((m, r.pos))),
            Err(ReadError::Incomplete) => Ok(None),
            Err(ReadError::Malformed) => Err(Error::Malformed),
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[self.version.0, self.version.1]);
        out.extend_from_slice(&self.code.to_be_bytes());
        out.extend_from_slice(&self.request_id.to_be_bytes());
        for group in &self.groups {
            out.push(group.tag as u8);
            for attr in &group.attributes {
                value::encode_attribute(attr, out);
            }
        }
        out.push(DelimiterTag::End as u8);
    }
}

pub(crate) enum ReadError {
    Incomplete,
    Malformed,
}

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], ReadError> {
        let end = self.pos.checked_add(n).ok_or(ReadError::Malformed)?;
        let s = self.buf.get(self.pos..end).ok_or(ReadError::Incomplete)?;
        self.pos = end;
        Ok(s)
    }

    pub(crate) fn peek_u8(&self) -> Result<u8, ReadError> {
        self.buf.get(self.pos).copied().ok_or(ReadError::Incomplete)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, ReadError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, ReadError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, ReadError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

fn parse_message(r: &mut Reader) -> Result<Message, ReadError> {
    let version = (r.u8()?, r.u8()?);
    let code = r.u16()?;
    let request_id = r.u32()?;
    let mut groups: Vec<Group> = Vec::new();
    loop {
        let tag = r.peek_u8()?;
        if tag < 0x10 {
            r.pos += 1;
            let tag = DelimiterTag::from_u8(tag).ok_or(ReadError::Malformed)?;
            if tag == DelimiterTag::End {
                return Ok(Message { version, code, request_id, groups });
            }
            groups.push(Group::new(tag));
        } else {
            let group = groups.last_mut().ok_or(ReadError::Malformed)?;
            value::parse_attribute_into(r, &mut group.attributes)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_printer_attributes() -> Message {
        let mut ops = Group::new(DelimiterTag::Operation);
        ops.push("attributes-charset", [Value::str(StrTag::Charset, "utf-8")]);
        ops.push("attributes-natural-language", [Value::str(StrTag::NaturalLanguage, "en")]);
        ops.push("printer-uri", [Value::str(StrTag::Uri, "ipp://inkplate.local/ipp/print")]);
        ops.push(
            "requested-attributes",
            [Value::keyword("printer-name"), Value::keyword("media-col-database")],
        );
        Message {
            version: (2, 0),
            code: Operation::GetPrinterAttributes as u16,
            request_id: 7,
            groups: alloc::vec![ops],
        }
    }

    #[test]
    fn roundtrip_with_multiple_values() {
        let msg = get_printer_attributes();
        let mut bytes = Vec::new();
        msg.encode(&mut bytes);
        let (parsed, used) = Message::parse(&bytes).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(parsed, msg);
        assert_eq!(parsed.operation(), Some(Operation::GetPrinterAttributes));
    }

    #[test]
    fn roundtrip_collection() {
        let mut msg = Message::response(&get_printer_attributes(), StatusCode::SuccessfulOk);
        let size = Value::Collection(alloc::vec![
            Attribute::new("x-dimension", [Value::Integer(10500)]),
            Attribute::new("y-dimension", [Value::Integer(14800)]),
        ]);
        let media_col = Value::Collection(alloc::vec![
            Attribute::new("media-size", [size]),
            Attribute::new("media-top-margin", [Value::Integer(0)]),
        ]);
        msg.group_mut(DelimiterTag::Printer).push("media-col-default", [media_col]);

        let mut bytes = Vec::new();
        msg.encode(&mut bytes);
        let (parsed, _) = Message::parse(&bytes).unwrap().unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn incomplete_then_document_offset() {
        let mut bytes = Vec::new();
        get_printer_attributes().encode(&mut bytes);
        let header_len = bytes.len();
        for cut in 0..header_len {
            assert!(Message::parse(&bytes[..cut]).unwrap().is_none(), "cut at {cut}");
        }
        bytes.extend_from_slice(b"RaS2...");
        let (_, used) = Message::parse(&bytes).unwrap().unwrap();
        assert_eq!(used, header_len);
    }

    #[test]
    fn rejects_attribute_before_group() {
        let bytes = [2, 0, 0, 0x0b, 0, 0, 0, 1, 0x44, 0, 1, b'a', 0, 1, b'b', 3];
        assert_eq!(Message::parse(&bytes), Err(Error::Malformed));
    }
}
