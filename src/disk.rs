//! Capacity of the volume that holds `Fern.Root`.
//!
//! Only volume level numbers are reported: walking the share just to add up
//! file sizes would stall the request on large trees, and it also would not
//! answer "how much room is left".

use std::path::Path;

#[derive(Clone, Copy, Debug, Default)]
pub struct Space {
    /// Size of the volume.
    pub total: u64,
    /// Space not in use, including whatever is reserved for the system.
    pub free: u64,
    /// Space the serving process may actually still write.
    pub available: u64,
}

impl Space {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

#[cfg(windows)]
pub fn space_of(path: &Path) -> Option<Space> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_bytes_available: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free)
    };
    if ok == 0 || total == 0 {
        return None;
    }
    Some(Space {
        total,
        free,
        available,
    })
}

#[cfg(unix)]
pub fn space_of(path: &Path) -> Option<Space> {
    // `statvfs` cannot be described by one struct: the block counters are
    // `unsigned long` on Linux and FreeBSD but a 32-bit type on Darwin, so a
    // shared definition would read the wrong fields on macOS. `df -Pk` is the
    // portable equivalent, and the 5 s cache in `Server` keeps the fork cost
    // out of the request path.
    let out = crate::platform::command("df")
        .arg("-Pk")
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// Reads the last (and only data) line of `df -Pk` output: the three numeric
/// columns before the capacity percentage are blocks, used and available, in
/// 1024-byte units.
#[cfg(unix)]
fn parse_df(text: &str) -> Option<Space> {
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 5 {
        return None;
    }
    // The mount point may contain spaces, so anchor the parse on the capacity
    // column instead of assuming a fixed offset from the start.
    let capacity = fields
        .iter()
        .position(|f| f.ends_with('%') && f[..f.len() - 1].parse::<u64>().is_ok())?;
    if capacity < 3 {
        return None;
    }
    let kb = |i: usize| fields[i].parse::<u64>().ok();
    let total = kb(capacity - 3)?.saturating_mul(1024);
    let used = kb(capacity - 2)?.saturating_mul(1024);
    let available = kb(capacity - 1)?.saturating_mul(1024);
    if total == 0 {
        return None;
    }
    Some(Space {
        total,
        free: total.saturating_sub(used.min(total)),
        available: available.min(total),
    })
}

#[cfg(not(any(windows, unix)))]
pub fn space_of(_path: &Path) -> Option<Space> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn parses_df_output_with_spaced_mount_point() {
        let text = "Filesystem 1024-blocks      Used Available Capacity Mounted on\n\
                    /dev/disk1s5   488245288 120000000 360000000      25% /Volumes/My Disk\n";
        let space = parse_df(text).expect("parse");
        assert_eq!(space.total, 488245288 * 1024);
        assert_eq!(space.used(), 120000000 * 1024);
        assert_eq!(space.available, 360000000 * 1024);
    }

    #[test]
    fn rejects_unparsable_output() {
        assert!(parse_df("").is_none());
        assert!(parse_df("df: /missing: No such file or directory\n").is_none());
    }
}
