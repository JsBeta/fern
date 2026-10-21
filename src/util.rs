//! Small helpers: percent coding, base36, HTTP dates, size formatting.

pub fn percent_decode_path(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn percent_decode_query(s: &str) -> Option<String> {
    let replaced = s.replace('+', " ");
    percent_decode_path(&replaced)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Percent-encodes a `/`-separated path while keeping the separators intact.
pub fn percent_encode_path(s: &str) -> String {
    s.split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

pub fn base36(mut v: u64) -> String {
    if v == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = [0u8; 16];
    let mut i = buf.len();
    while v > 0 {
        i -= 1;
        buf[i] = DIGITS[(v % 36) as usize];
        v /= 36;
    }
    String::from_utf8_lossy(&buf[i..]).into_owned()
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// IMF-fixdate, e.g. `Sun, 06 Nov 1994 08:49:37 GMT`.
pub fn http_date(ts: u64) -> String {
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = ts as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let wd = ((days + 4).rem_euclid(7)) as usize;
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WD[wd],
        d,
        MO[(m - 1) as usize],
        y,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

pub fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

pub fn file_etag(size: u64, mtime: u64) -> String {
    format!("{}-{}", base36(size), base36(mtime))
}

#[cfg(windows)]
pub fn is_reserved_windows_name(seg: &str) -> bool {
    let base = seg.split('.').next().unwrap_or(seg);
    let upper = base.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6"
        | "COM7" | "COM8" | "COM9" | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6"
        | "LPT7" | "LPT8" | "LPT9" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent() {
        assert_eq!(percent_decode_path("/a%20b").unwrap(), "/a b");
        assert_eq!(percent_decode_query("a+b").unwrap(), "a b");
        assert!(percent_decode_path("%zz").is_none());
    }

    #[test]
    fn encodes_roundtrip() {
        let s = "文件 name+1.txt";
        assert_eq!(percent_decode_path(&percent_encode(s)).unwrap(), s);
    }

    #[test]
    fn base36_values() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    #[test]
    fn formats_http_date() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(784_111_777), "Sun, 06 Nov 1994 08:49:37 GMT");
    }
}
