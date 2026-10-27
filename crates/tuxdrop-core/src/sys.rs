//! Operating-system specific helpers: everything else in the core is
//! portable. Linux, macOS and Windows are supported.

use std::io;
use std::path::Path;

/// `deviceModel` announced to other devices (LocalSend shows it next to the
/// alias).
pub fn device_model() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        other => other,
    }
}

/// Default alias: the name the user gave this computer.
pub fn hostname() -> String {
    #[cfg(target_os = "macos")]
    {
        // "MacBook Pro de Marie", rather than the network name
        // "MacBook-Pro-de-Marie.local".
        if let Ok(out) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if out.status.success() && !name.is_empty() {
                return name;
            }
        }
    }
    #[cfg(unix)]
    {
        rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(windows)]
    {
        // The device name of the Settings app, with its case, rather than
        // the upper-case NetBIOS name in COMPUTERNAME.
        use windows_sys::Win32::System::SystemInformation::{
            ComputerNamePhysicalDnsHostname, GetComputerNameExW,
        };
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` characters.
        let ok = unsafe {
            GetComputerNameExW(ComputerNamePhysicalDnsHostname, buf.as_mut_ptr(), &mut len)
        };
        if ok != 0 && len > 0 {
            return String::from_utf16_lossy(&buf[..len as usize]);
        }
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".to_owned())
    }
}

/// Whether the network interface `name` is backed by hardware (Ethernet,
/// Wi-Fi), as opposed to bridges, VPN tunnels, container links…
pub fn is_physical_interface(name: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/sys/class/net")
            .join(name)
            .join("device")
            .exists()
    }
    #[cfg(target_os = "macos")]
    {
        // en0 is Wi-Fi or built-in Ethernet, enN the other adapters; utun,
        // bridge, awdl, llw, vmenet… are virtual.
        name.starts_with("en")
    }
    #[cfg(windows)]
    {
        // if-addrs reports adapter names; virtual ones usually say so.
        let name = name.to_ascii_lowercase();
        ![
            "vethernet",
            "virtual",
            "vmware",
            "loopback",
            "bluetooth",
            "wsl",
        ]
        .iter()
        .any(|v| name.contains(v))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = name;
        true
    }
}

/// A short directory private to the user, for local sockets: their path is
/// limited to about 100 bytes.
#[cfg(unix)]
pub fn runtime_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        // /var/folders/…/T/, what $TMPDIR points to in a login session.
        let mut buf = vec![0u8; 1024];
        // SAFETY: `buf` is writable for its whole length.
        let len = unsafe {
            libc::confstr(
                libc::_CS_DARWIN_USER_TEMP_DIR,
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if len > 1 && len <= buf.len() {
            buf.truncate(len - 1);
            use std::os::unix::ffi::OsStringExt;
            return std::ffi::OsString::from_vec(buf).into();
        }
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(Into::into)
        .unwrap_or_else(crate::config::data_dir)
}

/// Writes a file only its owner can read (the private key).
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // On Windows the user's profile is already private to the user.
    options.open(path)?.write_all(contents)
}

/// Renames `from` to `to`, failing with `AlreadyExists` instead of replacing
/// an existing `to`.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            // Filesystems without RENAME_NOREPLACE / RENAME_EXCL (e.g. some
            // FUSE mounts).
            Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOTSUP) => rename_checked(from, to),
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let wide = |p: &Path| -> Vec<u16> {
            p.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        };
        let (from, to) = (wide(from), wide(to));
        // Without MOVEFILE_REPLACE_EXISTING, an existing `to` is an error
        // (ERROR_ALREADY_EXISTS, which std maps to `AlreadyExists`).
        // SAFETY: both paths are NUL-terminated.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        windows
    )))]
    {
        // A hard link cannot replace an existing file.
        match std::fs::hard_link(from, to) {
            Ok(()) => std::fs::remove_file(from),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            // Filesystems without hard links (FAT…).
            Err(_) => rename_checked(from, to),
        }
    }
}

/// Best effort: a file created at `to` between the check and the rename is
/// replaced.
#[cfg(not(windows))]
fn rename_checked(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

/// Bytes available to this user on the filesystem holding `path`.
pub fn available_space(path: &Path) -> io::Result<u64> {
    #[cfg(unix)]
    {
        let st = rustix::fs::statvfs(path)?;
        Ok(st.f_bavail.saturating_mul(st.f_frsize))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut available = 0u64;
        // SAFETY: `wide` is NUL-terminated; the other pointers may be null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_noreplace_keeps_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "new").unwrap();
        std::fs::write(&b, "old").unwrap();
        let err = rename_noreplace(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "old");
        let c = dir.path().join("c");
        rename_noreplace(&a, &c).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read_to_string(&c).unwrap(), "new");
    }

    #[test]
    fn available_space_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(available_space(dir.path()).unwrap() > 0);
    }

    #[test]
    fn hostname_is_not_empty() {
        assert!(!hostname().is_empty());
    }
}
