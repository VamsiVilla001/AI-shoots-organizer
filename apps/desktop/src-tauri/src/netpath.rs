//! Where a folder on *this* machine can be reached from the server.
//!
//! A client picks folders with its own file dialog, but the server is what
//! scans them, and a path like `D:\shoot` or `/Users/ann/shoot` means
//! nothing on another machine. Three cases translate:
//!
//! - a **mapped drive** (`Z:\shoot`) is its network path (`\\nas\media\shoot`);
//! - a **network volume** on a Mac (`/Volumes/media/shoot`) is the share it
//!   was mounted from (`\\nas\media\shoot`);
//! - a folder inside something **this machine shares** (Windows file sharing
//!   or macOS File Sharing over SMB) is reachable as `\\this-machine\share\…`,
//!   which is what lets a shoot live on a laptop without being copied.
//!
//! The last case answers with several spellings — the machine's name, then
//! the address the server sees it from — because which one the server can
//! resolve depends on the network; the caller tries them in order and asks
//! the server to list the folder. Anything else is a folder only this
//! machine can read, and the answer says how to share it.
//!
//! Every parser here is platform-independent and tested; only the handful of
//! lines that run `sharing`, `mount` and `scutil` (macOS) or read the
//! registry (Windows) are platform-specific.

use std::net::{ToSocketAddrs, UdpSocket};
use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPaths {
    /// Network spellings of the folder to try, best first. Empty when only
    /// this machine can read it.
    pub candidates: Vec<String>,
    /// What sharing the folder takes on this operating system; shown when
    /// `candidates` is empty.
    pub how_to_share: String,
}

/// Every network spelling of `path` this machine can vouch for.
pub fn resolve(path: &str, server_url: Option<&str>) -> NetworkPaths {
    let mut candidates = Vec::new();

    if path.starts_with("\\\\") || path.starts_with("//") {
        // Already a network path; the server can be asked as is.
        candidates.push(path.replace('/', "\\"));
    } else if let Some(mapped) = mapped_drive(path) {
        candidates.push(mapped);
    } else if let Some(mounted) = mounted_volume(path) {
        candidates.push(mounted);
    } else if let Some((share, rest)) = local_share_for(path) {
        for name in machine_names(server_url) {
            candidates.push(unc(&name, &share, &rest));
        }
    }

    NetworkPaths {
        candidates,
        how_to_share: HOW_TO_SHARE.to_string(),
    }
}

#[cfg(windows)]
const HOW_TO_SHARE: &str = "Share the folder from this computer: right-click it → Properties → Sharing → Share…, add the account the server uses, then choose the folder again.";

#[cfg(target_os = "macos")]
const HOW_TO_SHARE: &str = "Share the folder from this Mac: System Settings → General → Sharing → File Sharing (i) → add the folder, and under Options turn on \"Share files and folders using SMB\" for your account. Then choose the folder again.";

#[cfg(not(any(windows, target_os = "macos")))]
const HOW_TO_SHARE: &str = "Share the folder from this computer over SMB (Samba) and choose it again.";

// --- shared folders on this machine ---------------------------------------------

/// The share that contains `path`, as `(share name, path inside the share)`.
/// The longest matching share wins, so a folder shared on its own is not
/// answered through a share of its parent.
fn local_share_for(path: &str) -> Option<(String, String)> {
    let shares = local_shares();
    best_share(&shares, path)
}

fn best_share(shares: &[(String, String)], path: &str) -> Option<(String, String)> {
    shares
        .iter()
        .filter_map(|(name, root)| rest_under(root, path).map(|rest| (root.len(), name.clone(), rest)))
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, name, rest)| (name, rest))
}

#[cfg(windows)]
fn local_shares() -> Vec<(String, String)> {
    windows_shares::list()
}

#[cfg(not(windows))]
fn local_shares() -> Vec<(String, String)> {
    // `sharing -l` lists File Sharing's share points, SMB flag included.
    run("/usr/sbin/sharing", &["-l"])
        .map(|text| parse_sharing_list(&text))
        .unwrap_or_default()
}

/// Parses `sharing -l` (macOS). Only share points that are shared over SMB
/// count: that is the protocol a Windows server speaks.
///
/// ```text
/// List of Share Points
/// name:      bmsd
/// path:      /Users/ann/BMSD
///     afp:        {
///         shared:     0
///     }
///     smb:        {
///         name:       bmsd
///         shared:     1
///     }
/// ```
pub fn parse_sharing_list(text: &str) -> Vec<(String, String)> {
    #[derive(Default)]
    struct Point {
        name: String,
        path: String,
        smb_name: Option<String>,
        smb_shared: bool,
    }

    let mut points: Vec<Point> = Vec::new();
    let mut section: Option<String> = None;

    for raw in text.lines() {
        let indented = raw.starts_with(['\t', ' ']);
        let line = raw.trim();
        let Some((key, value)) = line.split_once(':') else {
            if line == "}" {
                section = None;
            }
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if !indented && key == "name" {
            points.push(Point {
                name: value.to_string(),
                ..Point::default()
            });
            section = None;
            continue;
        }
        let Some(current) = points.last_mut() else { continue };
        if !indented && key == "path" {
            current.path = value.to_string();
            continue;
        }
        if value.starts_with('{') {
            section = Some(key.to_string());
            continue;
        }
        if section.as_deref() == Some("smb") {
            match key {
                "name" => current.smb_name = Some(value.to_string()),
                "shared" => current.smb_shared = value == "1",
                _ => {}
            }
        }
    }

    points
        .into_iter()
        .filter(|p| p.smb_shared && !p.path.is_empty())
        .map(|p| (p.smb_name.unwrap_or(p.name), p.path))
        .collect()
}

#[cfg(windows)]
mod windows_shares {
    //! Windows keeps its shares under
    //! `HKLM\SYSTEM\CurrentControlSet\Services\LanmanServer\Shares`, one
    //! multi-string value per share (`Path=…`, `ShareName=…`, `Type=0`).
    //! Reading that key needs no privilege, unlike the `NetShareEnum` level
    //! that carries paths, which is for administrators only.

    use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_MULTI_SZ,
    };

    const SHARES_KEY: &str = r"SYSTEM\CurrentControlSet\Services\LanmanServer\Shares";

    pub fn list() -> Vec<(String, String)> {
        let key_name: Vec<u16> = SHARES_KEY.encode_utf16().chain(std::iter::once(0)).collect();
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `key_name` is NUL-terminated and `key` receives the handle.
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, key_name.as_ptr(), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
            return Vec::new();
        }

        let mut shares = Vec::new();
        let mut index = 0u32;
        let mut name = vec![0u16; 512];
        let mut data = vec![0u16; 32 * 1024];
        loop {
            let mut name_len = name.len() as u32;
            let mut data_len = (data.len() * 2) as u32;
            let mut value_type = 0u32;
            // SAFETY: every pointer is to a live buffer of the stated size.
            let status = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    &mut value_type,
                    data.as_mut_ptr() as *mut u8,
                    &mut data_len,
                )
            };
            index += 1;
            match status {
                ERROR_SUCCESS => {}
                ERROR_MORE_DATA => continue, // a share record too large to matter
                ERROR_NO_MORE_ITEMS => break,
                _ => break,
            }
            if value_type != REG_MULTI_SZ {
                continue;
            }
            let value_name = String::from_utf16_lossy(&name[..name_len as usize]);
            let words = data_len as usize / 2;
            let strings = super::split_multi_sz(&data[..words]);
            if let Some((share, path)) = super::parse_share_record(&value_name, &strings) {
                shares.push((share, path));
            }
        }
        // SAFETY: `key` was opened above and is not used afterwards.
        unsafe { RegCloseKey(key) };
        shares
    }
}

/// A registry `REG_MULTI_SZ` payload as its strings.
#[cfg_attr(not(windows), allow(dead_code))]
fn split_multi_sz(words: &[u16]) -> Vec<String> {
    words
        .split(|&w| w == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// One share record as Windows stores it: the value name is the share name,
/// and the strings hold `Path=`, `ShareName=` and `Type=`. Administrative
/// shares (`C$`, `ADMIN$`) are skipped: they need an administrator's
/// credentials on the server, which is not what a person shares a folder
/// with.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_share_record(value_name: &str, strings: &[String]) -> Option<(String, String)> {
    let mut path = None;
    let mut name = None;
    let mut disk = true;
    for entry in strings {
        if let Some(rest) = entry.strip_prefix("Path=") {
            path = Some(rest.trim().to_string());
        } else if let Some(rest) = entry.strip_prefix("ShareName=") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = entry.strip_prefix("Type=") {
            // STYPE_DISKTREE is 0; anything else is a printer, device or IPC.
            disk = rest.trim() == "0";
        }
    }
    let name = name.unwrap_or_else(|| value_name.to_string());
    let path = path?;
    if !disk || name.ends_with('$') || path.is_empty() {
        return None;
    }
    Some((name, path))
}

// --- mapped drives and mounted volumes -----------------------------------------

/// `Z:\shoots\day1` → `\\nas\media\shoots\day1` when `Z:` is a mapped drive.
#[cfg(windows)]
fn mapped_drive(path: &str) -> Option<String> {
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;

    let bytes = path.as_bytes();
    let is_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if !is_drive {
        return None;
    }
    let drive: Vec<u16> = path[..2].encode_utf16().chain(std::iter::once(0)).collect();
    let mut remote = vec![0u16; 1024];
    let mut length = remote.len() as u32;
    // SAFETY: both buffers outlive the call and `length` is their capacity.
    let status = unsafe { WNetGetConnectionW(drive.as_ptr(), remote.as_mut_ptr(), &mut length) };
    if status != 0 {
        return None;
    }
    let end = remote.iter().position(|&c| c == 0).unwrap_or(remote.len());
    let share = String::from_utf16_lossy(&remote[..end]);
    let rest = path[2..].trim_start_matches(['\\', '/']).replace('/', "\\");
    Some(if rest.is_empty() {
        share
    } else {
        format!("{}\\{}", share.trim_end_matches('\\'), rest)
    })
}

#[cfg(not(windows))]
fn mapped_drive(_path: &str) -> Option<String> {
    None
}

/// `/Volumes/media/shoots/day1` → `\\nas\media\shoots\day1` when
/// `/Volumes/media` is an SMB mount.
#[cfg(not(windows))]
fn mounted_volume(path: &str) -> Option<String> {
    let mounts = run("/sbin/mount", &[]).map(|text| parse_mounts(&text)).unwrap_or_default();
    mounted_volume_in(&mounts, path)
}

#[cfg(windows)]
fn mounted_volume(_path: &str) -> Option<String> {
    None
}

#[cfg_attr(windows, allow(dead_code))]
fn mounted_volume_in(mounts: &[SmbMount], path: &str) -> Option<String> {
    mounts
        .iter()
        .filter_map(|m| rest_under(&m.mount_point, path).map(|rest| (m.mount_point.len(), m, rest)))
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, m, rest)| unc(&m.host, &m.share, &rest))
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code))]
pub struct SmbMount {
    pub host: String,
    pub share: String,
    pub mount_point: String,
}

/// Parses `mount` (macOS) for SMB volumes:
///
/// ```text
/// //ann@nas._smb._tcp.local/media on /Volumes/media (smbfs, nodev, nosuid, mounted by ann)
/// ```
#[cfg_attr(windows, allow(dead_code))]
pub fn parse_mounts(text: &str) -> Vec<SmbMount> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let source = line.strip_prefix("//")?;
            let (source, remainder) = source.split_once(" on ")?;
            let options_at = remainder.rfind(" (")?;
            let mount_point = remainder[..options_at].trim().to_string();
            let options = &remainder[options_at + 2..];
            if !options.starts_with("smbfs") {
                return None;
            }
            let (authority, share) = source.split_once('/')?;
            let host = authority.rsplit('@').next().unwrap_or(authority);
            let share = percent_decode(share.trim_end_matches('/'));
            if host.is_empty() || share.is_empty() {
                return None;
            }
            Some(SmbMount {
                host: plain_hostname(host),
                share,
                mount_point,
            })
        })
        .collect()
}

/// `nas._smb._tcp.local` (how Finder names a Bonjour server) → `nas.local`,
/// which is what another machine can resolve.
fn plain_hostname(host: &str) -> String {
    match host.find("._smb._tcp.") {
        Some(at) => format!("{}.{}", &host[..at], &host[at + "._smb._tcp.".len()..]),
        None => host.to_string(),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// --- this machine's names, as the server would use them ------------------------

/// The names the server might reach this machine by, best first: its host
/// name, then the address it is seen from on the way to the server. A name
/// is what should end up in a shoot row — addresses change — but a name
/// only works when the server can resolve it, so both are offered.
fn machine_names(server_url: Option<&str>) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(host) = host_name() {
        names.push(host);
    }
    if let Some(ip) = server_url.and_then(local_ip_towards) {
        if !names.contains(&ip) {
            names.push(ip);
        }
    }
    names
}

#[cfg(windows)]
fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok().filter(|n| !n.is_empty())
}

#[cfg(not(windows))]
fn host_name() -> Option<String> {
    // The Bonjour name; Windows resolves `<name>.local` through mDNS.
    run("/usr/sbin/scutil", &["--get", "LocalHostName"])
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .map(|n| format!("{n}.local"))
}

/// The local address a packet to the server leaves from. No packet is sent:
/// connecting a UDP socket only picks the route.
fn local_ip_towards(server_url: &str) -> Option<String> {
    let (host, port) = host_and_port(server_url)?;
    let target = (host.as_str(), port).to_socket_addrs().ok()?.next()?;
    let socket = UdpSocket::bind(if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    socket.set_write_timeout(Some(Duration::from_millis(200))).ok()?;
    socket.connect(target).ok()?;
    let local = socket.local_addr().ok()?.ip();
    if local.is_loopback() || local.is_unspecified() {
        return None;
    }
    Some(local.to_string())
}

/// `http://192.168.1.229:8420/x` → `("192.168.1.229", 8420)`.
fn host_and_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let default_port = if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 };
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default_port);
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default_port)),
    }
}

// --- helpers --------------------------------------------------------------------

/// The part of `path` below `root`, with backslashes, or `None` when `path`
/// is not inside `root`. Component-wise and case-insensitive, since both
/// Windows and the default Mac file system are; `/` and `\` are the same.
fn rest_under(root: &str, path: &str) -> Option<String> {
    let root: Vec<String> = components(root);
    let path: Vec<String> = components(path);
    if root.is_empty() || path.len() < root.len() {
        return None;
    }
    let matches = root
        .iter()
        .zip(&path)
        .all(|(a, b)| a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase());
    if !matches {
        return None;
    }
    Some(path[root.len()..].join("\\"))
}

fn components(path: &str) -> Vec<String> {
    path.split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn unc(host: &str, share: &str, rest: &str) -> String {
    if rest.is_empty() {
        format!("\\\\{host}\\{share}")
    } else {
        format!("\\\\{host}\\{share}\\{rest}")
    }
}

#[cfg(not(windows))]
fn run(program: &str, args: &[&str]) -> Option<String> {
    let path = std::path::Path::new(program);
    let program = if path.exists() {
        program.to_string()
    } else {
        // Not at the usual place: let PATH find it.
        path.file_name().map(|n| n.to_string_lossy().into_owned())?
    };
    let output = std::process::Command::new(program).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_under_matches_whole_components_only() {
        assert_eq!(rest_under(r"D:\shoot", r"D:\shoot\day1\cam a"), Some("day1\\cam a".into()));
        assert_eq!(rest_under(r"D:\shoot", r"d:/SHOOT"), Some(String::new()));
        assert_eq!(rest_under(r"D:\shoot", r"D:\shoot2\day1"), None);
        assert_eq!(rest_under("/Users/ann/BMSD", "/Users/ann/BMSD/Assets/Player Photos"), Some("Assets\\Player Photos".into()));
        assert_eq!(rest_under("/Users/ann/BMSD", "/Users/ann"), None);
    }

    #[test]
    fn longest_share_wins() {
        let shares = vec![
            ("Users".to_string(), r"C:\Users".to_string()),
            ("bmsd".to_string(), r"C:\Users\ann\BMSD".to_string()),
        ];
        assert_eq!(
            best_share(&shares, r"C:\Users\ann\BMSD\Assets"),
            Some(("bmsd".into(), "Assets".into()))
        );
        assert_eq!(best_share(&shares, r"C:\Users\bob"), Some(("Users".into(), "bob".into())));
        assert_eq!(best_share(&shares, r"D:\elsewhere"), None);
    }

    #[test]
    fn share_records_keep_disk_shares_people_made() {
        let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_share_record(
                "BMPS_23",
                &strings(&["CATimeout=0", "Path=F:\\BMPS 2023\\BMPS_23", "Permissions=860", "ShareName=BMPS_23", "Type=0"])
            ),
            Some(("BMPS_23".into(), "F:\\BMPS 2023\\BMPS_23".into()))
        );
        assert_eq!(parse_share_record("C$", &strings(&["Path=C:\\", "ShareName=C$", "Type=0"])), None);
        assert_eq!(parse_share_record("print", &strings(&["Path=", "ShareName=print", "Type=1"])), None);
        assert_eq!(split_multi_sz(&[b'a' as u16, 0, b'b' as u16, 0, 0]), vec!["a", "b"]);
    }

    #[test]
    fn sharing_list_keeps_smb_share_points_only() {
        let text = "List of Share Points\n\
name:\t\tAnn's Public Folder\n\
path:\t\t/Users/ann/Public\n\
\tafp:\t\t{\n\
\t\tname:\t\tAnn's Public Folder\n\
\t\tshared:\t\t1\n\
\t}\n\
\tsmb:\t\t{\n\
\t\tname:\t\tAnn's Public Folder\n\
\t\tshared:\t\t0\n\
\t}\n\
name:\t\tBMSD 2026\n\
path:\t\t/Users/ann/Library/CloudStorage/OneDrive-Tess/Rajesh's files - BMSD 2026\n\
\tafp:\t\t{\n\
\t\tshared:\t\t0\n\
\t}\n\
\tsmb:\t\t{\n\
\t\tname:\t\tbmsd\n\
\t\tshared:\t\t1\n\
\t\tguest access:\t0\n\
\t}\n";
        assert_eq!(
            parse_sharing_list(text),
            vec![(
                "bmsd".to_string(),
                "/Users/ann/Library/CloudStorage/OneDrive-Tess/Rajesh's files - BMSD 2026".to_string()
            )]
        );
    }

    #[test]
    fn mounts_are_read_back_to_their_shares() {
        let text = "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
//ann@nas._smb._tcp.local/media on /Volumes/media (smbfs, nodev, nosuid, mounted by ann)\n\
//192.168.1.10/Player%20Photos on /Volumes/Player Photos (smbfs, nodev, nosuid, mounted by ann)\n\
afp_0TQ3v on /Volumes/old (afpfs, nodev, nosuid, mounted by ann)\n";
        let mounts = parse_mounts(text);
        assert_eq!(
            mounts,
            vec![
                SmbMount { host: "nas.local".into(), share: "media".into(), mount_point: "/Volumes/media".into() },
                SmbMount {
                    host: "192.168.1.10".into(),
                    share: "Player Photos".into(),
                    mount_point: "/Volumes/Player Photos".into()
                },
            ]
        );
        assert_eq!(
            mounted_volume_in(&mounts, "/Volumes/media/shoots/day1"),
            Some(r"\\nas.local\media\shoots\day1".into())
        );
        assert_eq!(
            mounted_volume_in(&mounts, "/Volumes/Player Photos"),
            Some(r"\\192.168.1.10\Player Photos".into())
        );
        assert_eq!(mounted_volume_in(&mounts, "/Users/ann/shoot"), None);
    }

    #[test]
    fn server_urls_yield_host_and_port() {
        assert_eq!(host_and_port("http://192.168.1.229:8420"), Some(("192.168.1.229".into(), 8420)));
        assert_eq!(host_and_port("https://skwad.example.com/api"), Some(("skwad.example.com".into(), 443)));
        assert_eq!(host_and_port("http://[fe80::1]:8420/"), Some(("fe80::1".into(), 8420)));
        assert_eq!(host_and_port("not a url"), None);
    }

    #[test]
    fn unc_spelling() {
        assert_eq!(unc("PC", "share", ""), r"\\PC\share");
        assert_eq!(unc("mac.local", "bmsd", "Assets\\Player Photos"), r"\\mac.local\bmsd\Assets\Player Photos");
        assert_eq!(resolve(r"\\nas\media\x", None).candidates, vec![r"\\nas\media\x".to_string()]);
        assert_eq!(resolve("//nas/media/x", None).candidates, vec![r"\\nas\media\x".to_string()]);
    }

    #[cfg(windows)]
    #[test]
    fn registry_shares_are_readable_without_privilege() {
        // Whatever this machine shares, reading the list must not fail; each
        // entry is a disk share with a path.
        for (name, path) in windows_shares::list() {
            assert!(!name.ends_with('$'), "{name}");
            assert!(!path.is_empty(), "{name}");
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// A folder inside something this machine shares comes back as
    /// `\<this machine>\<share>\<rest>`, then by the address facing the server.
    #[test]
    fn a_folder_inside_a_local_share_is_reachable_by_name_and_address() {
        let Some((share, root)) = windows_shares::list().into_iter().next() else {
            eprintln!("this machine shares nothing; nothing to check");
            return;
        };
        let inside = format!("{}\\day 1\\cam A", root.trim_end_matches('\\'));
        let answer = resolve(&inside, Some("http://127.0.0.1:8420"));
        let computer = std::env::var("COMPUTERNAME").unwrap();
        assert_eq!(answer.candidates.first(), Some(&format!("\\\\{computer}\\{share}\\day 1\\cam A")));
        // Loopback is not an address another machine could use, so it is left out.
        assert_eq!(answer.candidates.len(), 1, "{:?}", answer.candidates);
        eprintln!("{inside} -> {:?}", answer.candidates);

        let elsewhere = resolve("Q:\\nowhere\\at all", None);
        assert!(elsewhere.candidates.is_empty());
        assert!(elsewhere.how_to_share.contains("Sharing"));
    }
}
