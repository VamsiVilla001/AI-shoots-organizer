//! Turning player names into folder names that are safe on every target
//! filesystem.
//!
//! Player handles are not filenames. They contain colons, slashes, emoji and
//! trailing dots, and Windows additionally reserves a set of device names that
//! cannot be used at all. Getting this wrong means an export that silently
//! fails partway through, so it is handled explicitly.

/// Characters no Windows path may contain. This is the strictest of the target
/// platforms, so applying it everywhere keeps exports portable between a
/// Windows edit bay and a macOS one.
const ILLEGAL: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Device names Windows refuses regardless of extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Long enough for any real player name, short enough to leave room for the
/// rest of the path under Windows' traditional 260-character limit.
const MAX_COMPONENT: usize = 80;

/// Converts arbitrary text into a single safe path component.
pub fn sanitise_component(input: &str) -> String {
    let mut out: String = input
        .chars()
        .map(|c| {
            if ILLEGAL.contains(&c) || (c as u32) < 0x20 {
                '_'
            } else {
                c
            }
        })
        .collect();

    // Windows strips trailing dots and spaces, which would turn "Player." into
    // a name that does not match what we recorded.
    out = out.trim().trim_end_matches(['.', ' ']).trim().to_string();

    if out.chars().count() > MAX_COMPONENT {
        out = out.chars().take(MAX_COMPONENT).collect::<String>().trim_end().to_string();
    }

    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out = format!("{out}_");
    }

    if out.is_empty() {
        out = "Unnamed".to_string();
    }
    out
}

/// Picks a filename that does not collide with `taken`, appending ` (2)`,
/// ` (3)` and so on before the extension — the convention every operating
/// system's file manager uses, so it reads as expected to an editor.
pub fn deduplicate(filename: &str, taken: &mut std::collections::HashSet<String>) -> String {
    let key = filename.to_lowercase();
    if taken.insert(key) {
        return filename.to_string();
    }

    let (stem, extension) = match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (filename, None),
    };

    for n in 2..10_000 {
        let candidate = match extension {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        if taken.insert(candidate.to_lowercase()) {
            return candidate;
        }
    }

    // Unreachable in practice; better than looping forever.
    format!("{stem}-duplicate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ordinary_names_pass_through_untouched() {
        assert_eq!(sanitise_component("Jonathan"), "Jonathan");
        assert_eq!(sanitise_component("Gods Reign"), "Gods Reign");
        assert_eq!(sanitise_component("MaVi_07"), "MaVi_07");
    }

    #[test]
    fn illegal_characters_are_replaced() {
        assert_eq!(sanitise_component("Team/Player"), "Team_Player");
        assert_eq!(sanitise_component("A:B*C?D"), "A_B_C_D");
        assert_eq!(sanitise_component("back\\slash"), "back_slash");
    }

    #[test]
    fn trailing_dots_and_spaces_are_trimmed() {
        assert_eq!(sanitise_component("Player."), "Player");
        assert_eq!(sanitise_component("  Player  "), "Player");
        assert_eq!(sanitise_component("Player..."), "Player");
    }

    #[test]
    fn windows_device_names_are_escaped() {
        assert_eq!(sanitise_component("CON"), "CON_");
        assert_eq!(sanitise_component("nul"), "nul_");
        assert_eq!(sanitise_component("COM1"), "COM1_");
        // A name that merely contains a device name is fine.
        assert_eq!(sanitise_component("CONRAD"), "CONRAD");
    }

    #[test]
    fn empty_and_whitespace_names_get_a_placeholder() {
        assert_eq!(sanitise_component(""), "Unnamed");
        assert_eq!(sanitise_component("   "), "Unnamed");
        assert_eq!(sanitise_component("///"), "___");
    }

    #[test]
    fn very_long_names_are_truncated() {
        let long = "a".repeat(500);
        assert_eq!(sanitise_component(&long).chars().count(), MAX_COMPONENT);
    }

    #[test]
    fn non_ascii_names_survive() {
        assert_eq!(sanitise_component("Jonáthan"), "Jonáthan");
        assert_eq!(sanitise_component("최상혁"), "최상혁");
    }

    #[test]
    fn duplicate_filenames_get_a_suffix() {
        let mut taken = HashSet::new();
        assert_eq!(deduplicate("IMG_0231.JPG", &mut taken), "IMG_0231.JPG");
        assert_eq!(deduplicate("IMG_0231.JPG", &mut taken), "IMG_0231 (2).JPG");
        assert_eq!(deduplicate("IMG_0231.JPG", &mut taken), "IMG_0231 (3).JPG");
    }

    #[test]
    fn deduplication_is_case_insensitive() {
        // Windows and macOS both treat these as the same file.
        let mut taken = HashSet::new();
        deduplicate("Shot.jpg", &mut taken);
        assert_eq!(deduplicate("SHOT.JPG", &mut taken), "SHOT (2).JPG");
    }

    #[test]
    fn extensionless_files_are_handled() {
        let mut taken = HashSet::new();
        deduplicate("README", &mut taken);
        assert_eq!(deduplicate("README", &mut taken), "README (2)");
    }
}
