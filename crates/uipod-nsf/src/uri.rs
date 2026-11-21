//! Virtual subtrack URI parsing and formatting (`/Music/NSF/Contra.nsf#3`).
//!
//! Only a trailing `#<digits>` immediately following `.nsf` or `.nsfe`
//! (case-insensitive) is treated as a 0-based subtrack fragment so `#`
//! characters inside directory or file names (e.g. `/Music/#1 Hits/Rock #2.nsf`)
//! are preserved verbatim.

/// Canonical virtual URI representation: `/Music/NSF/Contra.nsf#3` (0-based subtrack index).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtrackUri<'a> {
    pub file_path: &'a str,
    pub subtrack_index: Option<u16>,
}

impl<'a> SubtrackUri<'a> {
    /// Parse a raw path or virtual subtrack URI into its base file path and optional 0-based subtrack index.
    #[inline]
    pub fn parse(uri: &'a str) -> Self {
        parse_subtrack_uri(uri)
    }
}

/// Returns `true` if `path` ends with `.nsf` or `.nsfe` (case-insensitive), ignoring any valid `#<idx>` suffix.
#[inline]
pub fn is_nsf_path(path: &str) -> bool {
    let base = parse_subtrack_uri(path).file_path;
    has_nsf_extension(base)
}

#[inline]
pub(crate) fn has_nsf_extension(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() >= 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".nsf") {
        // Ensure there is at least a filename stem before `.nsf` (or bare `.nsf` is rejected if empty stem after `/`)
        let stem = &path[..bytes.len() - 4];
        return !stem.is_empty() && !stem.ends_with('/') && !stem.ends_with('\\');
    }
    if bytes.len() >= 5 && bytes[bytes.len() - 5..].eq_ignore_ascii_case(b".nsfe") {
        let stem = &path[..bytes.len() - 5];
        return !stem.is_empty() && !stem.ends_with('/') && !stem.ends_with('\\');
    }
    false
}

/// Parse `/path/to/Mega Man 2.nsf#12` into `SubtrackUri { file_path: "/path/to/Mega Man 2.nsf", subtrack_index: Some(12) }`.
///
/// Only treats a trailing `#<digits>` immediately following `.nsf` or `.nsfe` (case-insensitive)
/// as a subtrack fragment.
pub fn parse_subtrack_uri(uri: &str) -> SubtrackUri<'_> {
    if let Some(hash_pos) = uri.rfind('#') {
        let base = &uri[..hash_pos];
        let frag = &uri[hash_pos + 1..];
        if !frag.is_empty()
            && frag.bytes().all(|b| b.is_ascii_digit())
            && has_nsf_extension(base)
        {
            if let Ok(idx) = frag.parse::<u16>() {
                return SubtrackUri {
                    file_path: base,
                    subtrack_index: Some(idx),
                };
            }
        }
    }
    SubtrackUri {
        file_path: uri,
        subtrack_index: None,
    }
}

/// Format a canonical virtual subtrack URI (`<base_path>#<index>`), stripping any existing
/// `#<digits>` subtrack suffix on `.nsf`/`.nsfe` paths first so formatting is idempotent.
pub fn format_subtrack_uri(path: &str, index: u16) -> String {
    let base = parse_subtrack_uri(path).file_path;
    let mut out = String::with_capacity(base.len() + 7);
    out.push_str(base);
    out.push('#');
    out.push_str(&index.to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_nsf_and_nsfe_subtrack_uris() {
        assert_eq!(
            SubtrackUri::parse("/path/to/Mega Man 2.nsf#12"),
            SubtrackUri {
                file_path: "/path/to/Mega Man 2.nsf",
                subtrack_index: Some(12),
            }
        );
        assert_eq!(
            SubtrackUri::parse("/Music/NSF/Akumajou Densetsu.NSFE#0"),
            SubtrackUri {
                file_path: "/Music/NSF/Akumajou Densetsu.NSFE",
                subtrack_index: Some(0),
            }
        );
        assert_eq!(
            SubtrackUri::parse("Contra.Nsf#255"),
            SubtrackUri {
                file_path: "Contra.Nsf",
                subtrack_index: Some(255),
            }
        );
    }

    #[test]
    fn preserves_hashes_in_directories_and_filenames() {
        assert_eq!(
            SubtrackUri::parse("/Music/#Chiptunes/Game #2.nsf"),
            SubtrackUri {
                file_path: "/Music/#Chiptunes/Game #2.nsf",
                subtrack_index: None,
            }
        );
        assert_eq!(
            SubtrackUri::parse("/Music/#Chiptunes/Game #2.nsf#7"),
            SubtrackUri {
                file_path: "/Music/#Chiptunes/Game #2.nsf",
                subtrack_index: Some(7),
            }
        );
        assert_eq!(
            SubtrackUri::parse("/Music/#1/Song.mp3#3"),
            SubtrackUri {
                file_path: "/Music/#1/Song.mp3#3",
                subtrack_index: None,
            }
        );
        assert_eq!(
            SubtrackUri::parse("/Music/Game.nsf#"),
            SubtrackUri {
                file_path: "/Music/Game.nsf#",
                subtrack_index: None,
            }
        );
        assert_eq!(
            SubtrackUri::parse("/Music/Game.nsf#abc"),
            SubtrackUri {
                file_path: "/Music/Game.nsf#abc",
                subtrack_index: None,
            }
        );
        assert_eq!(
            SubtrackUri::parse("/Music/Game.nsf#99999"),
            SubtrackUri {
                file_path: "/Music/Game.nsf#99999",
                subtrack_index: None,
            }
        );
    }

    #[test]
    fn formats_subtrack_uri_idempotently() {
        assert_eq!(
            format_subtrack_uri("/Music/NSF/Contra.nsf", 3),
            "/Music/NSF/Contra.nsf#3"
        );
        assert_eq!(
            format_subtrack_uri("/Music/#Favs/Contra #1.nsf#0", 14),
            "/Music/#Favs/Contra #1.nsf#14"
        );
        assert!(is_nsf_path("/Music/Contra.nsf#3"));
        assert!(is_nsf_path("/Music/Contra.NSFE"));
        assert!(!is_nsf_path("/Music/Contra.flac"));
    }
}
