//! Decoder for the gzip streams produced by `build.rs`.
//!
//! Only the subset the build script emits is supported: stored and fixed
//! Huffman blocks. That keeps the code (and therefore the binary) small while
//! still giving the UI assets a 3.5x size reduction.

use std::sync::OnceLock;

include!("deflate_tables.rs");

/// Expands an embedded asset. Returns `None` when the stream is malformed.
pub fn gzip(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return None;
    }
    let flags = data[3];
    let mut pos = 10;
    if flags & 0x04 != 0 {
        let len = u16::from_le_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2 + len;
    }
    for flag in [0x08u8, 0x10] {
        if flags & flag != 0 {
            while *data.get(pos)? != 0 {
                pos += 1;
            }
            pos += 1;
        }
    }
    if flags & 0x02 != 0 {
        pos += 2;
    }
    let end = data.len().checked_sub(8)?;
    if pos > end {
        return None;
    }
    inflate(data.get(pos..end)?)
}

pub fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut bits = Bits {
        data,
        pos: 0,
        byte: 0,
        left: 0,
    };
    let mut out: Vec<u8> = Vec::with_capacity(data.len() * 4);
    loop {
        let last = bits.bit()?;
        match bits.value(2)? {
            0 => {
                bits.left = 0;
                let len = bits.value(16)? as usize;
                let nlen = bits.value(16)? as usize;
                if len != !nlen & 0xffff {
                    return None;
                }
                for _ in 0..len {
                    out.push(bits.value(8)? as u8);
                }
            }
            1 => {
                let (literals, distances) = fixed_tables();
                loop {
                    match literals.decode(&mut bits)? {
                        symbol @ 0..=255 => out.push(symbol as u8),
                        256 => break,
                        symbol => {
                            let index = symbol as usize - 257;
                            let len = *LENGTH_BASE.get(index)? as usize
                                + bits.value(LENGTH_EXTRA[index] as u32)? as usize;
                            let code = distances.decode(&mut bits)? as usize;
                            let dist = *DIST_BASE.get(code)? as usize
                                + bits.value(DIST_EXTRA[code] as u32)? as usize;
                            if dist == 0 || dist > out.len() {
                                return None;
                            }
                            let start = out.len() - dist;
                            for step in 0..len {
                                let byte = out[start + step];
                                out.push(byte);
                            }
                        }
                    }
                }
            }
            _ => return None,
        }
        if last == 1 {
            break;
        }
    }
    Some(out)
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    byte: u32,
    left: u32,
}

impl Bits<'_> {
    /// Deflate packs bits least significant first within a byte.
    fn bit(&mut self) -> Option<u32> {
        if self.left == 0 {
            self.byte = *self.data.get(self.pos)? as u32;
            self.pos += 1;
            self.left = 8;
        }
        self.left -= 1;
        let bit = self.byte & 1;
        self.byte >>= 1;
        Some(bit)
    }

    /// Reads `count` bits, the first one landing in the least significant bit.
    fn value(&mut self, count: u32) -> Option<u32> {
        let mut value = 0;
        for index in 0..count {
            value |= self.bit()? << index;
        }
        Some(value)
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Self {
        let mut counts = [0u16; 16];
        for &len in lengths {
            counts[len as usize] += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        for len in 1..16 {
            offsets[len] = offsets[len - 1] + counts[len - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbols[offsets[len as usize] as usize] = symbol as u16;
                offsets[len as usize] += 1;
            }
        }
        Self { counts, symbols }
    }

    fn decode(&self, bits: &mut Bits) -> Option<u16> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..16 {
            code |= bits.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

fn fixed_tables() -> &'static (Huffman, Huffman) {
    static TABLES: OnceLock<(Huffman, Huffman)> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut literals = [0u8; 288];
        for (symbol, len) in literals.iter_mut().enumerate() {
            *len = match symbol {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        (Huffman::new(&literals), Huffman::new(&[5u8; 30]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_broken_streams() {
        assert!(gzip(b"not gzip at all, really").is_none());
        assert!(gzip(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255]).is_none());
    }

    #[test]
    fn decodes_stored_blocks() {
        let mut packed = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        packed.extend_from_slice(&[0x01, 0x02, 0x00, 0xfd, 0xff, b'h', b'i']);
        packed.extend_from_slice(&[0; 8]);
        assert_eq!(gzip(&packed), Some(b"hi".to_vec()));
    }

    #[test]
    fn decodes_fixed_huffman_literals() {
        // "hi" as fixed Huffman literals plus the end of block code.
        let mut bits = BitWriter::new();
        bits.bits(1, 1); // BFINAL
        bits.bits(1, 2); // fixed Huffman
        bits.literal(b'h' as u32);
        bits.literal(b'i' as u32);
        bits.literal_end();
        let mut packed = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        packed.extend_from_slice(&bits.finish());
        packed.extend_from_slice(&[0; 8]);
        assert_eq!(gzip(&packed), Some(b"hi".to_vec()));
    }

    struct BitWriter {
        out: Vec<u8>,
        bits: u32,
        count: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                bits: 0,
                count: 0,
            }
        }

        fn bits(&mut self, value: u32, count: u32) {
            self.bits |= value << self.count;
            self.count += count;
            while self.count >= 8 {
                self.out.push(self.bits as u8);
                self.bits >>= 8;
                self.count -= 8;
            }
        }

        fn code(&mut self, code: u32, len: u32) {
            let mut reversed = 0;
            for i in 0..len {
                reversed = (reversed << 1) | ((code >> i) & 1);
            }
            self.bits(reversed, len);
        }

        fn literal(&mut self, symbol: u32) {
            match symbol {
                0..=143 => self.code(0x30 + symbol, 8),
                _ => self.code(0x190 + (symbol - 144), 9),
            }
        }

        fn literal_end(&mut self) {
            self.code(256 - 256, 7);
        }

        fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                self.out.push(self.bits as u8);
            }
            self.out
        }
    }
}
