//! Git's packet-line protocol — the framing used by `filter-process` and a
//! handful of other long-running git subprocess interfaces.
//!
//! Each packet is `<4-byte hex length><payload>`, where the length includes
//! the 4 length bytes themselves. The special length `0000` (no payload) is
//! the **flush packet**, used as a delimiter.

use std::io::{self, Read, Write};

/// Maximum payload size of a single packet (`65520 − 4` length bytes).
pub const MAX_PACKET_DATA: usize = 65516;

/// Reads packets from an underlying `Read`.
pub struct Reader<R: Read> {
    inner: R,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    /// Read one packet. Returns `Ok(None)` for a flush packet (`0000`).
    /// Returns `Err(UnexpectedEof)` if the underlying stream closes mid-frame
    /// or before a frame begins — callers can treat that as "client done".
    pub fn read_packet(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut hdr = [0u8; 4];
        self.inner.read_exact(&mut hdr)?;
        let len_str = std::str::from_utf8(&hdr).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "non-ASCII pktline length")
        })?;
        let len = u32::from_str_radix(len_str, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid pktline length"))?;
        if len == 0 {
            return Ok(None);
        }
        if len < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("pktline length {len} < 4"),
            ));
        }
        let body_len = (len - 4) as usize;
        let mut buf = vec![0u8; body_len];
        self.inner.read_exact(&mut buf)?;
        Ok(Some(buf))
    }

    /// Read one packet as text, stripping a single trailing `\n` if present.
    /// Returns `Ok(None)` for a flush packet.
    pub fn read_text(&mut self) -> io::Result<Option<String>> {
        let Some(mut bytes) = self.read_packet()? else {
            return Ok(None);
        };
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// Writes packets to an underlying `Write`.
pub struct Writer<W: Write> {
    inner: W,
}

impl<W: Write> Writer<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Write a packet of arbitrary bytes. Errors if `data.len() > MAX_PACKET_DATA`.
    pub fn write_packet(&mut self, data: &[u8]) -> io::Result<()> {
        if data.len() > MAX_PACKET_DATA {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("packet of {} bytes exceeds {MAX_PACKET_DATA}", data.len()),
            ));
        }
        let total = data.len() + 4;
        write!(self.inner, "{total:04x}")?;
        self.inner.write_all(data)?;
        Ok(())
    }

    /// Write a text packet, appending a single `\n` (the convention git uses).
    pub fn write_text(&mut self, text: &str) -> io::Result<()> {
        let mut buf = String::with_capacity(text.len() + 1);
        buf.push_str(text);
        buf.push('\n');
        self.write_packet(buf.as_bytes())
    }

    /// Write a flush packet (`0000`).
    pub fn write_flush(&mut self) -> io::Result<()> {
        self.inner.write_all(b"0000")
    }

    /// Flush the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A `Write` adapter that splits writes into pktline packets, suitable for
/// streaming smudge content through the filter-process response protocol.
///
/// Buffers up to [`MAX_PACKET_DATA`] bytes between packet boundaries; calling
/// [`Write::flush`] sends any partial buffered data as a final packet (but
/// does *not* emit a flush packet — the caller controls that explicitly).
pub struct Sink<'a, W: Write> {
    writer: &'a mut Writer<W>,
    buf: Vec<u8>,
}

impl<'a, W: Write> Sink<'a, W> {
    pub fn new(writer: &'a mut Writer<W>) -> Self {
        Self {
            writer,
            buf: Vec::with_capacity(MAX_PACKET_DATA),
        }
    }
}

impl<W: Write> Write for Sink<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let space = MAX_PACKET_DATA - self.buf.len();
        let n = buf.len().min(space);
        self.buf.extend_from_slice(&buf[..n]);
        if self.buf.len() == MAX_PACKET_DATA {
            self.writer.write_packet(&self.buf)?;
            self.buf.clear();
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.writer.write_packet(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_text_packet() {
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_text("hello").unwrap();
        // "hello\n" is 6 bytes, +4 prefix = 10 = 0x000a.
        assert_eq!(buf, b"000ahello\n");
        let mut r = Reader::new(Cursor::new(&buf));
        assert_eq!(r.read_text().unwrap().as_deref(), Some("hello"));
    }

    #[test]
    fn flush_round_trip() {
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_flush().unwrap();
        assert_eq!(buf, b"0000");
        let mut r = Reader::new(Cursor::new(&buf));
        assert_eq!(r.read_packet().unwrap(), None);
    }

    #[test]
    fn binary_packet_round_trips() {
        let payload = b"\x00\x01\x02\xffbytes";
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_packet(payload).unwrap();
        let mut r = Reader::new(Cursor::new(&buf));
        assert_eq!(r.read_packet().unwrap().as_deref(), Some(&payload[..]));
    }

    #[test]
    fn rejects_oversized_packet() {
        let big = vec![0u8; MAX_PACKET_DATA + 1];
        let err = Writer::new(Vec::new()).write_packet(&big).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn invalid_length_header() {
        let mut r = Reader::new(Cursor::new(b"zzzz"));
        let err = r.read_packet().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn sink_chunks_at_packet_boundary() {
        let mut buf = Vec::new();
        let mut writer = Writer::new(&mut buf);
        let mut sink = Sink::new(&mut writer);
        // Write exactly one full-packet's worth + a little extra, plus flush.
        let big = vec![b'x'; MAX_PACKET_DATA + 100];
        sink.write_all(&big).unwrap();
        sink.flush().unwrap();
        drop(sink);
        writer.write_flush().unwrap();

        // Decode and confirm we got two data packets followed by flush.
        let mut r = Reader::new(Cursor::new(&buf));
        let p1 = r.read_packet().unwrap().unwrap();
        let p2 = r.read_packet().unwrap().unwrap();
        let p3 = r.read_packet().unwrap();
        assert_eq!(p1.len(), MAX_PACKET_DATA);
        assert_eq!(p2.len(), 100);
        assert_eq!(p3, None);
    }
}
