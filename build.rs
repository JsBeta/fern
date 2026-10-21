//! Packs the embedded UI files with gzip before they are linked into the
//! binary, so the shipped `fern.exe` carries ~15 KB of assets instead of ~55 KB.
//!
//! The project has no third party dependencies, so the compressor lives here.
//! It emits a single fixed Huffman block with LZ77 back references, which the
//! runtime decoder (`src/inflate.rs`) understands. Nothing else is required
//! because every byte stays under our control.

use std::env;
use std::fs;
use std::path::PathBuf;

include!("src/deflate_tables.rs");

/// Must match the assets embedded by `src/assets.rs`.
const ASSETS: [&str; 4] = ["index.html", "app.js", "style.css", "favicon.svg"];

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const WINDOW: usize = 32_768;
const HASH_BITS: u32 = 14;
const HASH_SIZE: usize = 1 << HASH_BITS;
const MAX_CHAIN: usize = 48;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/deflate_tables.rs");
    println!("cargo:rerun-if-changed=ui");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    for name in ASSETS {
        println!("cargo:rerun-if-changed=ui/{name}");
        let raw = fs::read(format!("ui/{name}")).unwrap_or_else(|e| panic!("ui/{name}: {e}"));
        let packed = gzip(&raw);
        fs::write(out.join(format!("{name}.gz")), &packed).unwrap_or_else(|e| panic!("{name}.gz: {e}"));
    }
}

struct BitWriter {
    out: Vec<u8>,
    bits: u32,
    count: u32,
}

impl BitWriter {
    fn new(capacity: usize) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
            bits: 0,
            count: 0,
        }
    }

    /// Appends `count` bits of `value`, least significant bit first, which is
    /// the order deflate uses for block headers and extra bits.
    fn bits(&mut self, value: u32, count: u32) {
        self.bits |= value << self.count;
        self.count += count;
        while self.count >= 8 {
            self.out.push(self.bits as u8);
            self.bits >>= 8;
            self.count -= 8;
        }
    }

    /// Huffman codes are packed most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        let mut reversed = 0;
        for i in 0..len {
            reversed = (reversed << 1) | ((code >> i) & 1);
        }
        self.bits(reversed, len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push(self.bits as u8);
        }
        self.out
    }
}

fn fixed_lit(w: &mut BitWriter, symbol: u32) {
    match symbol {
        0..=143 => w.code(0x30 + symbol, 8),
        144..=255 => w.code(0x190 + (symbol - 144), 9),
        256..=279 => w.code(symbol - 256, 7),
        _ => w.code(0xC0 + (symbol - 280), 8),
    }
}

fn put_match(w: &mut BitWriter, len: usize, dist: usize) {
    let mut idx = LENGTH_BASE.len() - 1;
    while LENGTH_BASE[idx] as usize > len {
        idx -= 1;
    }
    fixed_lit(w, 257 + idx as u32);
    let extra = LENGTH_EXTRA[idx] as u32;
    if extra > 0 {
        w.bits(len as u32 - LENGTH_BASE[idx] as u32, extra);
    }

    let mut didx = DIST_BASE.len() - 1;
    while DIST_BASE[didx] as usize > dist {
        didx -= 1;
    }
    w.code(didx as u32, 5);
    let dextra = DIST_EXTRA[didx] as u32;
    if dextra > 0 {
        w.bits(dist as u32 - DIST_BASE[didx] as u32, dextra);
    }
}

fn hash_at(data: &[u8], pos: usize) -> usize {
    let triple = (data[pos] as u32) << 16 | (data[pos + 1] as u32) << 8 | data[pos + 2] as u32;
    (triple.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize & (HASH_SIZE - 1)
}

fn remember(data: &[u8], pos: usize, head: &mut [usize], prev: &mut [usize]) {
    if pos + MIN_MATCH > data.len() {
        return;
    }
    let h = hash_at(data, pos);
    prev[pos] = head[h];
    head[h] = pos;
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new(data.len() / 2 + 64);
    w.bits(1, 1); // BFINAL
    w.bits(1, 2); // BTYPE = fixed Huffman

    let mut head = vec![usize::MAX; HASH_SIZE];
    let mut prev = vec![usize::MAX; data.len()];
    let mut pos = 0;
    while pos < data.len() {
        let mut best_len = 0;
        let mut best_dist = 0;
        if pos + MIN_MATCH <= data.len() {
            let max_len = MAX_MATCH.min(data.len() - pos);
            let mut candidate = head[hash_at(data, pos)];
            let mut steps = 0;
            while candidate != usize::MAX && steps < MAX_CHAIN {
                let dist = pos - candidate;
                if dist > WINDOW {
                    break;
                }
                let mut len = 0;
                while len < max_len && data[candidate + len] == data[pos + len] {
                    len += 1;
                }
                if len > best_len {
                    best_len = len;
                    best_dist = dist;
                    if len == max_len {
                        break;
                    }
                }
                candidate = prev[candidate];
                steps += 1;
            }
        }

        if best_len >= MIN_MATCH {
            put_match(&mut w, best_len, best_dist);
            for step in pos..pos + best_len {
                remember(data, step, &mut head, &mut prev);
            }
            pos += best_len;
        } else {
            remember(data, pos, &mut head, &mut prev);
            fixed_lit(&mut w, data[pos] as u32);
            pos += 1;
        }
    }
    fixed_lit(&mut w, 256); // end of block
    w.finish()
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn gzip(data: &[u8]) -> Vec<u8> {
    // Header: no name or timestamp, so the output only depends on the content.
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    out.extend_from_slice(&deflate(data));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}
