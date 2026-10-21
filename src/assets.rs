//! Embedded UI resources. Serving them from the binary keeps deployment to a
//! single file, matching the original tool.
//!
//! The bytes are stored gzip packed (see `build.rs`) and expanded on first use,
//! which keeps the executable ~40 KB smaller than embedding the plain files.

use std::sync::OnceLock;

pub struct Asset {
    pub name: &'static str,
    pub content_type: &'static str,
    /// gzip stream produced by `build.rs`.
    packed: &'static [u8],
    /// Content hash of the packed bytes, so the validator changes whenever a UI
    /// file changes.
    hash: u64,
    plain: OnceLock<Vec<u8>>,
}

impl Asset {
    pub fn bytes(&self) -> &[u8] {
        self.plain.get_or_init(|| {
            crate::inflate::gzip(self.packed).expect("embedded asset is valid gzip")
        })
    }

    #[cfg(test)]
    fn packed(&self) -> &'static [u8] {
        self.packed
    }
}

/// FNV-1a over the packed bytes; evaluated at compile time by `include_bytes!`.
const fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
        i += 1;
    }
    hash
}

macro_rules! asset {
    ($name:ident, $gz:ident, $file:literal, $content_type:literal) => {
        const $gz: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/", $file, ".gz"));

        pub static $name: Asset = Asset {
            name: $file,
            content_type: $content_type,
            packed: $gz,
            hash: fnv1a64($gz),
            plain: OnceLock::new(),
        };
    };
}

asset!(INDEX, INDEX_GZ, "index.html", "text/html; charset=utf-8");
asset!(APP_JS, APP_JS_GZ, "app.js", "text/javascript; charset=utf-8");
asset!(STYLE_CSS, STYLE_CSS_GZ, "style.css", "text/css; charset=utf-8");
asset!(FAVICON, FAVICON_GZ, "favicon.svg", "image/svg+xml");

static ALL: [&Asset; 4] = [&INDEX, &APP_JS, &STYLE_CSS, &FAVICON];

pub fn find(request_path: &str) -> Option<&'static Asset> {
    let name = request_path.strip_prefix("/assets/")?;
    ALL.iter().copied().find(|a| a.name == name)
}

pub fn etag_for(asset: &Asset) -> String {
    format!("{}-{:016x}", asset.name, asset.hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_assets_expand_to_the_source_files() {
        let raw: [(&Asset, &[u8]); 4] = [
            (&INDEX, include_bytes!("../ui/index.html")),
            (&APP_JS, include_bytes!("../ui/app.js")),
            (&STYLE_CSS, include_bytes!("../ui/style.css")),
            (&FAVICON, include_bytes!("../ui/favicon.svg")),
        ];
        for (asset, source) in raw {
            assert_eq!(asset.bytes(), source, "{} roundtrip", asset.name);
            assert!(
                asset.packed().len() < source.len(),
                "{} should be packed smaller",
                asset.name
            );
        }
    }
}
