pub mod archive;
pub mod fs;
pub mod links;
pub mod ntp;
pub mod pe_version;

use md5::{Digest, Md5};
use std::io::Read;
use std::path::Path;

/// Upper-case hex MD5 of a file, the format the S3 manifests use for ETags.
pub fn md5_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode_upper(hasher.finalize()))
}

/// Decode `%XX` escapes. Used for S3 keys, which come back percent-encoded,
/// and for `filename*=` in a Content-Disposition header.
///
/// `+` is left alone: S3 encodes a space as `%20`, not `+`.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Open a URL in the user's browser, or a path in their file manager.
pub fn open_external(target: &str) {
    if let Err(e) = open::that_detached(target) {
        log::warn!("could not open {target}: {e}");
    }
}

/// Digits of a version string compared numerically, as the launcher's
/// self-update and Vulkan/Gentool checks do.
pub fn version_is_older(current: &str, latest: &str) -> bool {
    crate::model::compare_versions(current, latest) == std::cmp::Ordering::Less
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_launcher_versions_digitwise() {
        assert!(version_is_older("1.0.1.1", "1.0.2.0"));
        assert!(!version_is_older("1.0.1.1", "1.0.1.1"));
        assert!(!version_is_older("1.0.2.0", "1.0.1.1"));
    }

    #[test]
    fn decodes_percent_escapes_but_leaves_plus_alone() {
        assert_eq!(percent_decode("%21%21OFS_ALPHA_Window_16_9.big"), "!!OFS_ALPHA_Window_16_9.big");
        assert_eq!(percent_decode("Contra%20009.zip"), "Contra 009.zip");
        assert_eq!(percent_decode("a+b.big"), "a+b.big");
        assert_eq!(percent_decode("plain.big"), "plain.big");
        // A stray, non-hex '%' must survive untouched.
        assert_eq!(percent_decode("100%done"), "100%done");
    }
}

