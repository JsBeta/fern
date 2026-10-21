//! Single range support (`Range: bytes=...`).

#[derive(Debug, PartialEq, Eq)]
pub enum RangeSpec {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

pub fn parse(header: Option<&str>, size: u64) -> RangeSpec {
    let value = match header {
        Some(v) => v.trim(),
        None => return RangeSpec::Full,
    };
    let rest = match value.strip_prefix("bytes=") {
        Some(r) => r.trim(),
        None => return RangeSpec::Full,
    };
    if rest.contains(',') {
        return RangeSpec::Full;
    }
    let (start_text, end_text) = match rest.split_once('-') {
        Some(v) => v,
        None => return RangeSpec::Full,
    };
    let start_text = start_text.trim();
    let end_text = end_text.trim();

    if start_text.is_empty() {
        let suffix: u64 = match end_text.parse() {
            Ok(v) => v,
            Err(_) => return RangeSpec::Full,
        };
        if suffix == 0 || size == 0 {
            return RangeSpec::Unsatisfiable;
        }
        let start = size.saturating_sub(suffix);
        return RangeSpec::Partial {
            start,
            end: size - 1,
        };
    }

    let start: u64 = match start_text.parse() {
        Ok(v) => v,
        Err(_) => return RangeSpec::Full,
    };
    if start >= size && size > 0 {
        return RangeSpec::Unsatisfiable;
    }
    let end = if end_text.is_empty() {
        size.saturating_sub(1)
    } else {
        match end_text.parse::<u64>() {
            Ok(v) => v.min(size.saturating_sub(1)),
            Err(_) => return RangeSpec::Full,
        }
    };
    if end < start {
        return RangeSpec::Unsatisfiable;
    }
    RangeSpec::Partial { start, end }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_ranges() {
        assert_eq!(
            parse(Some("bytes=0-4"), 10),
            RangeSpec::Partial { start: 0, end: 4 }
        );
        assert_eq!(
            parse(Some("bytes=5-"), 10),
            RangeSpec::Partial { start: 5, end: 9 }
        );
        assert_eq!(
            parse(Some("bytes=-3"), 10),
            RangeSpec::Partial { start: 7, end: 9 }
        );
    }

    #[test]
    fn clamps_and_rejects() {
        assert_eq!(
            parse(Some("bytes=5-100"), 10),
            RangeSpec::Partial { start: 5, end: 9 }
        );
        assert_eq!(parse(Some("bytes=10-20"), 10), RangeSpec::Unsatisfiable);
        assert_eq!(parse(Some("bytes=wrong"), 10), RangeSpec::Full);
        assert_eq!(parse(None, 10), RangeSpec::Full);
        assert_eq!(parse(Some("bytes=0-1,4-5"), 10), RangeSpec::Full);
    }
}
