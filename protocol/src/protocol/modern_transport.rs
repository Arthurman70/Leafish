//! Bounded Minecraft framing used by the modern configuration conformance probe.
//!
//! Does not change the playable protocol list or replace the legacy connection.
//! Wire limits follow the installed 1.21.1 Varint21FrameDecoder and
//! CompressionDecoder. Encryption is the caller's responsibility.
use flate2::{write::ZlibEncoder, Compression, Decompress, FlushDecompress, Status};
use std::io::{self, Cursor, Read, Write};

pub const MAX_FRAME: usize = (1 << 21) - 1;
pub const MAX_UNCOMPRESSED: usize = 8 * 1024 * 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn read_varint(reader: &mut impl Read) -> io::Result<i32> {
    let mut value = 0u32;
    for i in 0..5 {
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        if i == 4 && byte[0] & 0xf0 != 0 {
            return Err(invalid("VarInt exceeds 32 bits"));
        }
        value |= ((byte[0] & 0x7f) as u32) << (7 * i);
        if byte[0] & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    Err(invalid("VarInt exceeds five bytes"))
}

pub fn write_varint(writer: &mut impl Write, value: i32) -> io::Result<()> {
    let mut value = value as u32;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        writer.write_all(&[byte])?;
        if value == 0 {
            return Ok(());
        }
    }
}

pub fn write_string(writer: &mut impl Write, value: &str) -> io::Result<()> {
    if value.len() > 32767 * 3 || value.encode_utf16().count() > 32767 {
        return Err(invalid("String exceeds wire limit"));
    }
    write_varint(writer, value.len() as i32)?;
    writer.write_all(value.as_bytes())
}

/// A frame reader/writer with optional Minecraft packet compression.
pub struct Framed<S> {
    stream: S,
    threshold: Option<usize>,
}

impl<S: Read + Write> Framed<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            threshold: None,
        }
    }

    pub fn set_compression(&mut self, threshold: i32) -> io::Result<()> {
        if threshold < 0 {
            return Err(invalid("Invalid compression threshold"));
        }
        self.threshold = Some(threshold as usize);
        Ok(())
    }

    pub fn read_packet(&mut self) -> io::Result<(i32, Vec<u8>)> {
        let mut length = 0usize;
        for i in 0..3 {
            let mut byte = [0];
            self.stream.read_exact(&mut byte)?;
            length |= ((byte[0] & 0x7f) as usize) << (7 * i);
            if byte[0] & 0x80 == 0 {
                break;
            }
            if i == 2 {
                return Err(invalid("Frame length exceeds 21 bits"));
            }
        }
        if length == 0 || length > MAX_FRAME {
            return Err(invalid("Invalid frame length"));
        }
        let mut frame = vec![0; length];
        self.stream.read_exact(&mut frame)?;
        let packet = if let Some(threshold) = self.threshold {
            let mut cursor = Cursor::new(frame.as_slice());
            let declared = read_varint(&mut cursor)?;
            let remaining = &frame[cursor.position() as usize..];
            if declared == 0 {
                remaining.to_vec()
            } else {
                if declared < 0
                    || declared as usize > MAX_UNCOMPRESSED
                    || (declared as usize) < threshold
                {
                    return Err(invalid("Invalid uncompressed packet length"));
                }
                // Require an actual zlib end marker/checksum, independently of
                // a Read adapter's treatment of EOF or backend buffer errors.
                let mut decoder = Decompress::new(true);
                let mut decoded = vec![0; declared as usize + 1];
                let status = decoder
                    .decompress(remaining, &mut decoded, FlushDecompress::Finish)
                    .map_err(|_| invalid("Invalid zlib stream"))?;
                if status != Status::StreamEnd
                    || decoder.total_out() != declared as u64
                    || decoder.total_in() != remaining.len() as u64
                {
                    return Err(invalid(
                        "Incomplete compressed stream, length mismatch or trailing bytes",
                    ));
                }
                decoded.truncate(declared as usize);
                decoded
            }
        } else {
            frame
        };
        let mut cursor = Cursor::new(packet.as_slice());
        let id = read_varint(&mut cursor)?;
        if id < 0 {
            return Err(invalid("Negative packet ID"));
        }
        Ok((id, packet[cursor.position() as usize..].to_vec()))
    }

    pub fn write_packet(&mut self, id: i32, payload: &[u8]) -> io::Result<()> {
        if id < 0 || payload.len() > MAX_UNCOMPRESSED - 5 {
            return Err(invalid("Packet exceeds limit"));
        }
        let mut packet = Vec::with_capacity(payload.len() + 5);
        write_varint(&mut packet, id)?;
        packet.extend_from_slice(payload);
        let frame = if let Some(threshold) = self.threshold {
            let mut frame = Vec::new();
            if packet.len() >= threshold {
                write_varint(&mut frame, packet.len() as i32)?;
                let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
                encoder.write_all(&packet)?;
                frame.extend_from_slice(&encoder.finish()?);
            } else {
                write_varint(&mut frame, 0)?;
                frame.extend_from_slice(&packet);
            }
            frame
        } else {
            packet
        };
        if frame.len() > MAX_FRAME {
            return Err(invalid("Encoded frame exceeds limit"));
        }
        write_varint(&mut self.stream, frame.len() as i32)?;
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    pub fn into_inner(self) -> S {
        self.stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_varints_and_overflow() {
        for value in [0, 1, 127, 128, 767, i32::MAX, -1, i32::MIN] {
            let mut bytes = Vec::new();
            write_varint(&mut bytes, value).unwrap();
            assert_eq!(read_varint(&mut bytes.as_slice()).unwrap(), value);
        }
        assert!(read_varint(&mut [0xff, 0xff, 0xff, 0xff, 0x10].as_slice()).is_err());
        assert!(read_varint(&mut [0x80].as_slice()).is_err());
    }

    #[test]
    fn compression_negotiation_preserves_both_frame_forms() {
        for threshold in [None, Some(0), Some(64), Some(4096)] {
            let mut wire = Framed::new(Cursor::new(Vec::new()));
            if let Some(value) = threshold {
                wire.set_compression(value).unwrap();
            }
            wire.write_packet(7, &[42; 1024]).unwrap();
            wire.write_packet(3, &[]).unwrap();
            let mut buffer = wire.into_inner();
            buffer.set_position(0);
            let mut wire = Framed::new(buffer);
            if let Some(value) = threshold {
                wire.set_compression(value).unwrap();
            }
            assert_eq!(wire.read_packet().unwrap(), (7, vec![42; 1024]));
            assert_eq!(wire.read_packet().unwrap(), (3, vec![]));
        }
    }

    #[test]
    fn rejects_length_overflow_empty_and_truncation() {
        for bytes in [vec![0], vec![0x80, 0x80, 0x80, 0], vec![4, 1, 2]] {
            assert!(Framed::new(Cursor::new(bytes)).read_packet().is_err());
        }
        let mut wire = Framed::new(Cursor::new(vec![5, 0xff, 0xff, 0xff, 0xff, 0x07]));
        wire.set_compression(0).unwrap();
        assert!(wire.read_packet().is_err());
    }

    #[test]
    fn detects_compressed_size_lie_and_trailing_bytes() {
        let mut wire = Framed::new(Cursor::new(Vec::new()));
        wire.set_compression(0).unwrap();
        wire.write_packet(7, &[1; 32]).unwrap();
        let encoded = wire.into_inner().into_inner();
        let mut lie = encoded.clone();
        lie[1] += 1;
        let mut wire = Framed::new(Cursor::new(lie));
        wire.set_compression(0).unwrap();
        assert!(wire.read_packet().is_err());
        let mut trailing = encoded;
        trailing[0] += 1;
        trailing.push(42);
        let mut wire = Framed::new(Cursor::new(trailing));
        wire.set_compression(0).unwrap();
        assert!(wire.read_packet().is_err());
    }

    #[test]
    fn rejects_zlib_with_complete_output_but_missing_checksum() {
        // Full deflate output: packet 7 followed by 32 bytes of 0x01. The zlib
        // Adler32 trailer is absent; matching output length is insufficient.
        let bytes = vec![0x08, 0x21, 0x78, 0x01, 0x63, 0x67, 0x24, 0x00, 0x00];
        let mut wire = Framed::new(Cursor::new(bytes));
        wire.set_compression(0).unwrap();
        assert!(wire.read_packet().is_err());
    }
}
