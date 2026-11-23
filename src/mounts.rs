//! Mount point detection.
//!
//! Deleting or renaming a mount point is not a file operation: the kernel
//! refuses to move the root of a mounted volume (`EBUSY`), and a recursive
//! delete would walk into the mounted volume and destroy its contents. The
//! file API therefore has to recognise a mount point before it touches it.
//!
//! Comparing `st_dev` with the parent directory is not enough: bind mounts,
//! btrfs subvolumes and mounted folders keep the device number of the
//! filesystem they live on. Linux asks the authoritative mount list in
//! `/proc/self/mountinfo`; Windows uses `GetVolumePathNameW`, which reports
//! folder mount points as well as drive letters.

use std::path::{Path, PathBuf};

/// True when `path` is a directory that is the root of a mounted volume, as
/// opposed to an ordinary directory inside one.
pub fn is_mount_point(path: &Path) -> bool {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    // Only directories matter: `remove_dir_all` on a mount point empties the
    // mounted volume before failing to remove the mount point itself, while
    // unlinking a bind-mounted file just fails with `EBUSY` and leaves the
    // volume alone. Links are not directories for `symlink_metadata`, so a
    // link to a mount point is a plain link.
    if !meta.file_type().is_dir() {
        return false;
    }
    platform_is_mount_point(path)
}

/// True when a mounted volume lives somewhere below `path` (not at `path`
/// itself). Deleting such a tree would either move the live volume into the
/// recycle bin or walk into it and destroy its contents.
pub fn contains_mount_point(path: &Path) -> bool {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.file_type().is_dir() {
        return false;
    }
    platform_contains_mount_point(path)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn platform_is_mount_point(path: &Path) -> bool {
    let real = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => return false,
    };
    match mount_points() {
        Some(points) => points.iter().any(|point| point == &real),
        // A restricted /proc (some Android sandboxes) still gives us the
        // device number as a reason to refuse the delete.
        None => device_boundary(path),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn platform_contains_mount_point(path: &Path) -> bool {
    let real = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => return false,
    };
    match mount_points() {
        Some(points) => points
            .iter()
            .any(|point| point != &real && point.starts_with(&real)),
        // Without the mount list a subtree check is not possible; the volume
        // boundary itself is still caught by `is_mount_point`.
        None => false,
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn mount_points() -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    Some(parse_mountinfo(&text))
}

/// Field 5 of every `mountinfo` line is the mount point. Spaces and
/// backslashes inside a path are escaped as `\040`, `\011`, `\012` or `\134`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn parse_mountinfo(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(raw) = line.split(' ').nth(4) {
            out.push(PathBuf::from(unescape_mountinfo(raw)));
        }
    }
    out
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn unescape_mountinfo(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let digits = &bytes[i + 1..i + 4];
            if digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                let value = digits
                    .iter()
                    .fold(0u32, |acc, b| acc * 8 + u32::from(*b - b'0'));
                out.push(value as u8);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Fallback for a missing `/proc/self/mountinfo`: a real volume changes the
/// device number, a bind mount does not (and stays unprotected here).
#[cfg(unix)]
fn device_boundary(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return false,
    };
    match std::fs::metadata(parent) {
        Ok(p) => p.dev() != meta.dev(),
        Err(_) => false,
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn platform_is_mount_point(path: &Path) -> bool {
    device_boundary(path)
}

#[cfg(windows)]
fn platform_is_mount_point(path: &Path) -> bool {
    match volume_path(path) {
        Some(volume) => same_path(&volume, path),
        None => false,
    }
}

/// Volume mount point that serves `path`: a drive root such as `C:\` or the
/// folder a volume was mounted into.
#[cfg(windows)]
pub fn volume_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumePathNameW(path: *const u16, volume: *mut u16, len: u32) -> i32;
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = vec![0u16; 1024];
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buf[..end])))
}

#[cfg(windows)]
fn same_path(a: &Path, b: &Path) -> bool {
    normalized(a) == normalized(b)
}

#[cfg(windows)]
fn platform_contains_mount_point(path: &Path) -> bool {
    let base = normalized(path);
    for volume in volumes() {
        for mount in volume_paths(&volume) {
            let parts = normalized(&mount);
            if parts.len() > base.len() && parts[..base.len()] == base[..] {
                return true;
            }
        }
    }
    false
}

/// Every volume name (`\\?\Volume{...}\`) known to the mount manager.
#[cfg(windows)]
fn volumes() -> Vec<Vec<u16>> {
    #[link(name = "kernel32")]
    extern "system" {
        fn FindFirstVolumeW(volume: *mut u16, len: u32) -> isize;
        fn FindNextVolumeW(handle: isize, volume: *mut u16, len: u32) -> i32;
        fn FindVolumeClose(handle: isize) -> i32;
    }

    let mut out = Vec::new();
    let mut buf = vec![0u16; 1024];
    let handle = unsafe { FindFirstVolumeW(buf.as_mut_ptr(), buf.len() as u32) };
    if handle == -1 {
        return out;
    }
    loop {
        out.push(buf.clone());
        let next = unsafe { FindNextVolumeW(handle, buf.as_mut_ptr(), buf.len() as u32) };
        if next == 0 {
            break;
        }
    }
    unsafe {
        FindVolumeClose(handle);
    }
    out
}

/// All mount paths of one volume: drive letters and mounted folders.
#[cfg(windows)]
fn volume_paths(volume: &[u16]) -> Vec<PathBuf> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumePathNamesForVolumeNameW(
            name: *const u16,
            paths: *mut u16,
            len: u32,
            needed: *mut u32,
        ) -> i32;
    }

    fn query(volume: &[u16], buf: &mut [u16], needed: &mut u32) -> i32 {
        unsafe {
            GetVolumePathNamesForVolumeNameW(
                volume.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                needed,
            )
        }
    }

    let mut buf = vec![0u16; 512];
    let mut needed = 0u32;
    if query(volume, &mut buf, &mut needed) == 0 {
        // `needed` is filled in and the error is ERROR_MORE_DATA when the
        // buffer was too small; any other failure leaves the volume unlisted.
        if std::io::Error::last_os_error().raw_os_error() != Some(234) || needed == 0 {
            return Vec::new();
        }
        buf.resize(needed as usize, 0);
        if query(volume, &mut buf, &mut needed) == 0 {
            return Vec::new();
        }
    }
    // The result is a sequence of NUL terminated strings ended by a NUL.
    let mut out = Vec::new();
    let mut start = 0;
    while start < buf.len() && buf[start] != 0 {
        let end = match buf[start..].iter().position(|c| *c == 0) {
            Some(offset) => start + offset,
            None => break,
        };
        out.push(PathBuf::from(String::from_utf16_lossy(&buf[start..end])));
        start = end + 1;
    }
    out
}

/// Component-wise, case-insensitive comparison for Windows paths.
#[cfg(windows)]
fn normalized(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn platform_is_mount_point(_path: &Path) -> bool {
    false
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn platform_contains_mount_point(_path: &Path) -> bool {
    // No mount table to scan on these targets; the volume root itself is
    // still protected by `platform_is_mount_point`.
    false
}

#[cfg(not(any(unix, windows)))]
fn platform_contains_mount_point(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn parses_mountinfo_and_unescapes_paths() {
        let text = "\
36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw,errors=continue
37 36 0:33 / /home/user/my\\040disk rw - tmpfs tmpfs rw
";
        let points = parse_mountinfo(text);
        assert_eq!(
            points,
            vec![PathBuf::from("/mnt2"), PathBuf::from("/home/user/my disk")]
        );
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn recognises_a_real_mount_point() {
        if !Path::new("/proc/self/mountinfo").exists() {
            return;
        }
        assert!(is_mount_point(Path::new("/proc")));
        assert!(!is_mount_point(Path::new("/proc/uptime")));
        assert!(contains_mount_point(Path::new("/")));
    }

    #[test]
    fn missing_paths_are_not_mount_points() {
        // Without a real mount the check must still be false, and a missing
        // path must never panic.
        assert!(!is_mount_point(Path::new("definitely-not-there")));
    }

    #[test]
    fn ordinary_directories_are_not_mount_points() {
        let dir = std::env::temp_dir().join(format!("fern-mounts-plain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_mount_point(&dir));
        assert!(!contains_mount_point(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn recognises_a_drive_root() {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        assert!(is_mount_point(Path::new(&format!("{}\\", drive))));
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_not_mount_points() {
        use std::os::windows::process::CommandExt;

        let dir = std::env::temp_dir().join(format!("fern-mounts-junction-{}", std::process::id()));
        let target = dir.join("target");
        let link = dir.join("link");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&target).unwrap();
        let script = format!("mklink /J \"{}\" \"{}\"", link.display(), target.display());
        // `raw_arg` keeps the quotes the builtin needs; the regular argument
        // quoting would let cmd.exe re-parse them.
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .raw_arg(script)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!is_mount_point(&link));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn links_are_not_mount_points() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("fern-mounts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("proc-link");
        symlink("/proc", &link).unwrap();
        assert!(!is_mount_point(&link));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
