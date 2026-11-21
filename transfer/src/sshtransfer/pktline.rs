//! Git pkt-line framing for the pure-SSH transfer protocol.
//!
//! Each packet is a 4-digit hex length followed by the payload. The length
//! counts its own 4 bytes, so a packet with N bytes of payload starts with
//! `format!("{:04x}", N + 4)`.
//!
//! Two lengths carry no payload: `0000` is a flush packet, which ends a
//! message, and `0001` is a delim packet, which separates a command and its
//! arguments from the data that follows. The reader rejects `0002` and
//! `0003`.
//!
//! Text packets end in LF on the wire. [`Writer::write_text`] adds it and
//! [`Reader::read_text`] strips it.

use std::io::{self, Read, Write};

/// The largest payload of one packet.
///
/// The protocol spec caps the length at 65519 including the 4 header
/// bytes. Git itself allows one byte more. The reader rejects anything
/// larger than this.
pub const MAX_DATA: usize = 65515;

const FLUSH: u16 = 0;
const DELIM: u16 = 1;
const HEADER: usize = 4;

/// A single pkt-line packet read off the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Flush packet (`0000`), which ends a message.
    Flush,
    /// Delim packet (`0001`), which separates sections of a message.
    Delim,
    /// Data packet with its payload.
    Data(Vec<u8>),
}

/// Reads pkt-line packets from an underlying `Read`.
pub struct Reader<R: Read> {
    inner: R,
}

impl<R: Read> Reader<R> {
    /// Wrap a byte reader.
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    /// Read the next packet.
    ///
    /// Errors with [`io::ErrorKind::UnexpectedEof`] if the stream ends
    /// before or within a packet, and with [`io::ErrorKind::InvalidData`]
    /// for a malformed length, `0002` or `0003`, or a payload over
    /// [`MAX_DATA`].
    pub fn read_packet(&mut self) -> io::Result<Packet> {
        let mut header = [0u8; HEADER];
        self.inner.read_exact(&mut header)?;
        let len = parse_length(&header)?;
        match len {
            FLUSH => Ok(Packet::Flush),
            DELIM => Ok(Packet::Delim),
            2 | 3 => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("reserved pkt-line length {len:04x}"),
            )),
            _ => {
                let payload_len = (len as usize) - HEADER;
                if payload_len > MAX_DATA {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("pkt-line payload {payload_len} exceeds max {MAX_DATA}"),
                    ));
                }
                let mut buf = vec![0u8; payload_len];
                self.inner.read_exact(&mut buf)?;
                Ok(Packet::Data(buf))
            }
        }
    }

    /// Read the next packet, decoding a data packet as UTF-8 text.
    ///
    /// Strips one trailing LF, if present. Errors with
    /// [`io::ErrorKind::InvalidData`] if the payload is not UTF-8.
    pub fn read_text(&mut self) -> io::Result<TextPacket> {
        match self.read_packet()? {
            Packet::Flush => Ok(TextPacket::Flush),
            Packet::Delim => Ok(TextPacket::Delim),
            Packet::Data(mut bytes) => {
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                }
                let s = String::from_utf8(bytes).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("non-UTF8 text packet: {e}"),
                    )
                })?;
                Ok(TextPacket::Text(s))
            }
        }
    }
}

/// A packet read by [`Reader::read_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextPacket {
    /// Flush packet (`0000`).
    Flush,
    /// Delim packet (`0001`).
    Delim,
    /// Data packet decoded as UTF-8, without its trailing LF.
    Text(String),
}

/// Writes pkt-line packets to an underlying `Write`.
///
/// Each `write_*` call writes exactly one packet. Nothing is buffered
/// here, so callers flush the inner writer after a complete message.
pub struct Writer<W: Write> {
    inner: W,
}

impl<W: Write> Writer<W> {
    /// Wrap a byte writer.
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Borrow the underlying writer, for example to flush it.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Write a flush packet (`0000`).
    pub fn write_flush(&mut self) -> io::Result<()> {
        self.inner.write_all(b"0000")
    }

    /// Write a delim packet (`0001`).
    pub fn write_delim(&mut self) -> io::Result<()> {
        self.inner.write_all(b"0001")
    }

    /// Write a binary data packet.
    ///
    /// Errors with [`io::ErrorKind::InvalidInput`] if `data` is empty or
    /// longer than [`MAX_DATA`].
    pub fn write_data(&mut self, data: &[u8]) -> io::Result<()> {
        if data.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pkt-line data packet must be non-empty",
            ));
        }
        if data.len() > MAX_DATA {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pkt-line payload {} exceeds max {MAX_DATA}", data.len()),
            ));
        }
        let len = (data.len() + HEADER) as u16;
        let header = format!("{len:04x}");
        self.inner.write_all(header.as_bytes())?;
        self.inner.write_all(data)
    }

    /// Write a text packet, appending an LF unless `text` already ends in
    /// one.
    pub fn write_text(&mut self, text: &str) -> io::Result<()> {
        if text.ends_with('\n') {
            self.write_data(text.as_bytes())
        } else {
            // The LF belongs inside the same packet.
            let mut buf = Vec::with_capacity(text.len() + 1);
            buf.extend_from_slice(text.as_bytes());
            buf.push(b'\n');
            self.write_data(&buf)
        }
    }
}

fn parse_length(header: &[u8; HEADER]) -> io::Result<u16> {
    let s = std::str::from_utf8(header).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "pkt-line length header is not ASCII",
        )
    })?;
    u16::from_str_radix(s, 16).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pkt-line length header {s:?} is not hex"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_flush_emits_0000() {
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_flush().unwrap();
        assert_eq!(buf, b"0000");
    }

    #[test]
    fn write_delim_emits_0001() {
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_delim().unwrap();
        assert_eq!(buf, b"0001");
    }

    #[test]
    fn write_data_includes_header_and_payload() {
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_data(b"hi").unwrap();
        // header = 4 + 2 = 6 = "0006"
        assert_eq!(buf, b"0006hi");
    }

    #[test]
    fn write_text_appends_lf() {
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_text("version 1").unwrap();
        // header = 4 + 10 = "000e"
        assert_eq!(buf, b"000eversion 1\n");
    }

    #[test]
    fn write_text_preserves_existing_lf() {
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_text("version 1\n").unwrap();
        assert_eq!(buf, b"000eversion 1\n");
    }

    #[test]
    fn write_data_rejects_oversized_payload() {
        let mut buf = Vec::new();
        let big = vec![b'x'; MAX_DATA + 1];
        let err = Writer::new(&mut buf).write_data(&big).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn write_data_rejects_empty_payload() {
        // 0004 is technically a valid length header (header-only,
        // zero payload), but we reject the empty-data case at write
        // time to keep callers from accidentally emitting an
        // ambiguous packet.
        let mut buf = Vec::new();
        let err = Writer::new(&mut buf).write_data(&[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn read_flush_packet() {
        let mut r = Reader::new(&b"0000"[..]);
        assert_eq!(r.read_packet().unwrap(), Packet::Flush);
    }

    #[test]
    fn read_delim_packet() {
        let mut r = Reader::new(&b"0001"[..]);
        assert_eq!(r.read_packet().unwrap(), Packet::Delim);
    }

    #[test]
    fn read_data_packet() {
        let mut r = Reader::new(&b"0008data"[..]);
        assert_eq!(r.read_packet().unwrap(), Packet::Data(b"data".to_vec()));
    }

    #[test]
    fn read_text_strips_trailing_lf() {
        let mut r = Reader::new(&b"000eversion 1\n"[..]);
        assert_eq!(
            r.read_text().unwrap(),
            TextPacket::Text("version 1".to_owned()),
        );
    }

    #[test]
    fn read_text_without_trailing_lf() {
        let mut r = Reader::new(&b"000dversion 1"[..]);
        assert_eq!(
            r.read_text().unwrap(),
            TextPacket::Text("version 1".to_owned()),
        );
    }

    #[test]
    fn read_text_flush_and_delim() {
        let mut r = Reader::new(&b"00000001"[..]);
        assert_eq!(r.read_text().unwrap(), TextPacket::Flush);
        assert_eq!(r.read_text().unwrap(), TextPacket::Delim);
    }

    #[test]
    fn read_rejects_reserved_length() {
        let mut r = Reader::new(&b"0002"[..]);
        let err = r.read_packet().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let mut r = Reader::new(&b"0003"[..]);
        let err = r.read_packet().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_rejects_non_hex_header() {
        let mut r = Reader::new(&b"xxxx"[..]);
        let err = r.read_packet().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_rejects_non_utf8_text() {
        // header = 0005 (1 byte payload), payload = 0xff
        let mut r = Reader::new(&b"0005\xff"[..]);
        let err = r.read_text().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_unexpected_eof_in_payload() {
        // header says 8 bytes total but only 2 payload bytes are present
        let mut r = Reader::new(&b"0008ab"[..]);
        let err = r.read_packet().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_unexpected_eof_in_header() {
        let mut r = Reader::new(&b"00"[..]);
        let err = r.read_packet().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn round_trip_max_data() {
        let payload = vec![b'a'; MAX_DATA];
        let mut buf = Vec::new();
        Writer::new(&mut buf).write_data(&payload).unwrap();
        let mut r = Reader::new(&buf[..]);
        assert_eq!(r.read_packet().unwrap(), Packet::Data(payload));
    }

    #[test]
    fn round_trip_sequence() {
        // A handshake: the server sends "version=1" and a flush, the
        // client "version 1" and a flush, the server "status 200" and a
        // flush.
        let mut buf = Vec::new();
        {
            let mut w = Writer::new(&mut buf);
            w.write_text("version=1").unwrap();
            w.write_flush().unwrap();
            w.write_text("version 1").unwrap();
            w.write_flush().unwrap();
            w.write_text("status 200").unwrap();
            w.write_flush().unwrap();
        }

        let mut r = Reader::new(&buf[..]);
        assert_eq!(
            r.read_text().unwrap(),
            TextPacket::Text("version=1".to_owned())
        );
        assert_eq!(r.read_text().unwrap(), TextPacket::Flush);
        assert_eq!(
            r.read_text().unwrap(),
            TextPacket::Text("version 1".to_owned())
        );
        assert_eq!(r.read_text().unwrap(), TextPacket::Flush);
        assert_eq!(
            r.read_text().unwrap(),
            TextPacket::Text("status 200".to_owned())
        );
        assert_eq!(r.read_text().unwrap(), TextPacket::Flush);
    }
}
