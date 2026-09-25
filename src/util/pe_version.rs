//! Reads version strings out of a Windows PE file's `VS_VERSIONINFO` resource.
//!
//! The launcher needs the version of `d3d8.dll` (GenTool) and `d3d8x.dll`
//! (the Vulkan layer). The C# build called `version.dll`/`FileVersionInfo`;
//! parsing the resource ourselves keeps the check working on Linux too, where
//! those DLLs sit next to a Wine-hosted game.

use std::path::Path;

const RT_VERSION: u32 = 16;

/// `ProductVersion` from the file's version resource.
pub fn product_version(path: &Path) -> Option<String> {
    read_version_string(path, "ProductVersion")
}

/// `FileVersion` from the file's version resource.
pub fn file_version(path: &Path) -> Option<String> {
    read_version_string(path, "FileVersion")
}

fn read_version_string(path: &Path, key: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let blob = version_resource(&bytes).unwrap_or(&bytes);
    find_string_value(blob, key)
        // Some builds pad the value; a stray NUL or space is not part of it.
        .map(|v| v.trim().trim_end_matches('\0').to_owned())
        .filter(|v| !v.is_empty())
}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

/// Locate the RT_VERSION resource and return its raw bytes.
fn version_resource(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.get(..2)? != b"MZ" {
        return None;
    }
    let pe_off = u32_at(bytes, 0x3C)? as usize;
    if bytes.get(pe_off..pe_off + 4)? != b"PE\0\0" {
        return None;
    }

    let coff = pe_off + 4;
    let number_of_sections = u16_at(bytes, coff + 2)? as usize;
    let size_of_optional = u16_at(bytes, coff + 16)? as usize;
    let optional = coff + 20;

    let magic = u16_at(bytes, optional)?;
    let data_dir_off = match magic {
        0x10b => optional + 96,  // PE32
        0x20b => optional + 112, // PE32+
        _ => return None,
    };

    // Data directory entry 2 is the resource table.
    let resource_rva = u32_at(bytes, data_dir_off + 2 * 8)?;
    if resource_rva == 0 {
        return None;
    }

    let sections = optional + size_of_optional;
    let rva_to_offset = |rva: u32| -> Option<usize> {
        for i in 0..number_of_sections {
            let s = sections + i * 40;
            let va = u32_at(bytes, s + 12)?;
            let raw_size = u32_at(bytes, s + 16)?;
            let raw_ptr = u32_at(bytes, s + 20)?;
            if rva >= va && rva < va + raw_size.max(1) {
                return Some((raw_ptr + (rva - va)) as usize);
            }
        }
        None
    };

    let root = rva_to_offset(resource_rva)?;

    // Walk type -> name -> language, taking the first entry at each level.
    let type_entry = find_directory_entry(bytes, root, root, Some(RT_VERSION))?;
    let name_dir = type_entry.checked_sub(0)?;
    let name_entry = find_directory_entry(bytes, root, name_dir, None)?;
    let lang_entry = find_directory_entry(bytes, root, name_entry, None)?;

    // `lang_entry` is a leaf: IMAGE_RESOURCE_DATA_ENTRY.
    let data_rva = u32_at(bytes, lang_entry)?;
    let size = u32_at(bytes, lang_entry + 4)? as usize;
    let data_off = rva_to_offset(data_rva)?;
    bytes.get(data_off..data_off + size)
}

/// Returns the offset of the matching child (a subdirectory, or a leaf when the
/// high bit is clear). `id = None` takes the first child.
fn find_directory_entry(
    bytes: &[u8],
    resource_root: usize,
    dir: usize,
    id: Option<u32>,
) -> Option<usize> {
    let named = u16_at(bytes, dir + 12)? as usize;
    let ids = u16_at(bytes, dir + 14)? as usize;
    let first = dir + 16;

    for i in 0..(named + ids) {
        let entry = first + i * 8;
        let name = u32_at(bytes, entry)?;
        let offset = u32_at(bytes, entry + 4)?;

        // Named entries have the high bit set in `name`; we only match numeric ids.
        let matches = match id {
            None => true,
            Some(want) => name & 0x8000_0000 == 0 && name == want,
        };
        if !matches {
            continue;
        }

        return Some(resource_root + (offset & 0x7FFF_FFFF) as usize);
    }
    None
}

/// Find a `StringFileInfo` entry by key inside a version resource blob.
///
/// The `String` struct is `wLength, wValueLength, wType, szKey (UTF-16, NUL
/// terminated), padding to a 4-byte boundary, Value (UTF-16)`.
fn find_string_value(blob: &[u8], key: &str) -> Option<String> {
    let needle: Vec<u8> = key
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .chain([0, 0])
        .collect();

    let mut search_from = 0usize;
    while let Some(rel) = find_subslice(&blob[search_from..], &needle) {
        let key_start = search_from + rel;
        search_from = key_start + 2;

        // The key must start on a 4-byte boundary within the resource.
        let after_key = key_start + needle.len();
        let value_start = (after_key + 3) & !3;

        if let Some(value) = read_utf16_z(blob, value_start) {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn read_utf16_z(blob: &[u8], start: usize) -> Option<String> {
    let mut units = Vec::new();
    let mut off = start;
    // Bound the scan so a malformed resource cannot run away.
    while off + 1 < blob.len() && units.len() < 512 {
        let u = u16::from_le_bytes([blob[off], blob[off + 1]]);
        if u == 0 {
            break;
        }
        units.push(u);
        off += 2;
    }
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_string_pair_in_a_synthetic_blob() {
        // wLength, wValueLength, wType, "FileVersion\0", pad, "7.4.2.0\0"
        let mut blob = vec![0u8; 6];
        blob.extend("FileVersion".encode_utf16().flat_map(u16::to_le_bytes));
        blob.extend([0, 0]);
        while blob.len() % 4 != 0 {
            blob.push(0);
        }
        blob.extend("7.4.2.0".encode_utf16().flat_map(u16::to_le_bytes));
        blob.extend([0, 0]);

        assert_eq!(find_string_value(&blob, "FileVersion").as_deref(), Some("7.4.2.0"));
    }
}
