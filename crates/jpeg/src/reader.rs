//! Byte source over an async reader, and the entropy-coded-data bit reader.

use embedded_io_async::Read;

use crate::Error;

/// Bytes from `first` (already read by the caller), then from `rest`, buffered.
pub(crate) struct Source<'a, R> {
    first: &'a [u8],
    rest: &'a mut R,
    buf: [u8; 512],
    pos: usize,
    len: usize,
}

impl<'a, R: Read> Source<'a, R> {
    pub(crate) fn new(first: &'a [u8], rest: &'a mut R) -> Self {
        Self { first, rest, buf: [0; 512], pos: 0, len: 0 }
    }

    pub(crate) async fn byte(&mut self) -> Result<u8, Error> {
        if let [b, tail @ ..] = self.first {
            self.first = tail;
            return Ok(*b);
        }
        if self.pos == self.len {
            self.len = self.rest.read(&mut self.buf).await.map_err(|_| Error::Io)?;
            self.pos = 0;
            if self.len == 0 {
                return Err(Error::Malformed("unexpected end of data"));
            }
        }
        self.pos += 1;
        Ok(self.buf[self.pos - 1])
    }

    pub(crate) async fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from(self.byte().await?) << 8 | u16::from(self.byte().await?))
    }

    pub(crate) async fn skip(&mut self, mut n: usize) -> Result<(), Error> {
        while n > 0 {
            self.byte().await?;
            n -= 1;
        }
        Ok(())
    }
}

/// MSB-first bit reader for entropy-coded segments. `FF 00` is a data byte `FF`; any
/// other `FF xx` is a marker, after which zeros are fed (the scan ends there).
pub(crate) struct Bits {
    acc: u32,
    count: u32,
    pub(crate) marker: Option<u8>,
}

impl Bits {
    pub(crate) fn new() -> Self {
        Self { acc: 0, count: 0, marker: None }
    }

    /// Ensures at least 16 bits are buffered.
    pub(crate) async fn fill<R: Read>(&mut self, src: &mut Source<'_, R>) -> Result<(), Error> {
        while self.count <= 24 {
            let byte = if self.marker.is_some() {
                0
            } else {
                match src.byte().await? {
                    0xFF => match src.byte().await? {
                        0x00 => 0xFF,
                        m => {
                            self.marker = Some(m);
                            0
                        }
                    },
                    b => b,
                }
            };
            self.acc |= u32::from(byte) << (24 - self.count);
            self.count += 8;
        }
        Ok(())
    }

    /// The next `n` (≤ 16) bits, without consuming. Call [`Bits::fill`] first.
    pub(crate) fn peek(&self, n: u32) -> u32 {
        self.acc >> (32 - n)
    }

    pub(crate) fn consume(&mut self, n: u32) {
        self.acc <<= n;
        self.count -= n;
    }

    pub(crate) async fn receive<R: Read>(&mut self, src: &mut Source<'_, R>, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        self.fill(src).await?;
        let v = self.peek(n);
        self.consume(n);
        Ok(v)
    }

    /// At a restart interval: drop the partial byte, find the RSTn marker, start over.
    pub(crate) async fn restart<R: Read>(&mut self, src: &mut Source<'_, R>) -> Result<(), Error> {
        self.acc = 0;
        self.count = 0;
        let marker = match self.marker.take() {
            Some(m) => m,
            None => loop {
                if src.byte().await? == 0xFF {
                    match src.byte().await? {
                        0x00 | 0xFF => continue,
                        m => break m,
                    }
                }
            },
        };
        if (0xD0..=0xD7).contains(&marker) {
            Ok(())
        } else {
            Err(Error::Malformed("missing restart marker"))
        }
    }
}
