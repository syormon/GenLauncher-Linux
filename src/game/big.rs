//! Minimal reader for the Westwood BIG archive format, enough to recognise one
//! and to patch `MaxCameraHeight` inside `Data\INI\GameData.ini`.
//! Port of `BigHandler`.

use anyhow::Result;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// True for a real BIG archive, and for the 24-byte placeholder archives the
/// launcher uses to mask a stock game file without shipping a replacement.
pub fn is_big_archive(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else { return false };

    let mut head = [0u8; 4];
    match file.read(&mut head) {
        Ok(n) if n < 4 => {
            // Special case for empty/placeholder archives written as "??".
            n >= 2 && head[0] == b'?' && head[1] == b'?'
        }
        Ok(_) => matches!(&head, b"BIGF" | b"BIG4"),
        Err(_) => false,
    }
}

/// One file inside a BIG archive.
#[derive(Debug, Clone)]
pub struct BigEntry {
    pub offset: u32,
    pub length: u32,
    pub name: String,
}

/// Read the archive's file table.
pub fn read_entries(path: &Path) -> Result<Vec<BigEntry>> {
    let mut file = File::open(path)?;

    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    let _archive_size = read_u32_le(&mut file)?;
    let entry_count = read_u32_be(&mut file)?;
    let _header_size = read_u32_be(&mut file)?;

    // A corrupt header could claim a huge count; cap it at something sane.
    let entry_count = entry_count.min(200_000);

    let mut entries = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        let offset = read_u32_be(&mut file)?;
        let length = read_u32_be(&mut file)?;
        let name = read_cstring(&mut file)?;
        entries.push(BigEntry { offset, length, name });
    }
    Ok(entries)
}

/// True when the archive contains a `GameData.ini` — the file holding the
/// camera height cap.
pub fn file_contains_game_data_ini(path: &Path) -> bool {
    match read_entries(path) {
        Ok(entries) => entries.iter().any(|e| e.name.to_ascii_lowercase().contains("gamedata.ini")),
        Err(_) => false,
    }
}

/// Overwrite the `MaxCameraHeight` value inside the archive's `GameData.ini`.
///
/// The value is patched in place so the surrounding bytes, and therefore every
/// offset in the archive, stay exactly where they were.
pub fn set_camera_height(path: &Path, height: i32) -> Result<()> {
    let entries = read_entries(path)?;
    let Some(entry) = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(r"Data\INI\GameData.ini"))
    else {
        return Ok(());
    };

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(entry.offset as u64))?;
    let mut buf = vec![0u8; entry.length as usize];
    file.read_exact(&mut buf)?;
    drop(file);

    let text = String::from_utf8_lossy(&buf).into_owned();

    // The original skips the first 3000 characters to reach the gameplay block.
    let search_start = 3000.min(text.len());
    let Some(key_at) = text[search_start..].find("MaxCameraHeight").map(|i| i + search_start)
    else {
        return Ok(());
    };
    let Some(eq_at) = text[key_at + 14..].find('=').map(|i| i + key_at + 14) else {
        return Ok(());
    };

    let value_at = eq_at + 1;
    let replacement = format!("{height}.00");

    if value_at + replacement.len() > buf.len() {
        return Ok(());
    }

    let mut out = OpenOptions::new().write(true).open(path)?;
    out.seek(SeekFrom::Start(entry.offset as u64 + value_at as u64))?;
    out.write_all(replacement.as_bytes())?;
    out.flush()?;
    Ok(())
}

fn read_u32_le(file: &mut File) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    file.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u32_be(file: &mut File) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    file.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_cstring(file: &mut File) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        file.read_exact(&mut byte)?;
        if byte[0] == 0 {
            break;
        }
        bytes.push(byte[0]);
        if bytes.len() > 4096 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Every BIG archive under `folder`, recursively.
pub fn big_files_in_folder(folder: &Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    crate::util::fs::visit_files(folder, &mut |path| {
        if is_big_archive(path) {
            result.push(path.to_path_buf());
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn recognises_bigf_and_placeholder_archives() {
        let dir = std::env::temp_dir().join("gl-big-test");
        std::fs::create_dir_all(&dir).unwrap();

        let real = dir.join("real.big");
        std::fs::File::create(&real).unwrap().write_all(b"BIGF\0\0\0\0").unwrap();
        assert!(is_big_archive(&real));

        let placeholder = dir.join("ph.big");
        std::fs::File::create(&placeholder).unwrap().write_all(b"??").unwrap();
        assert!(is_big_archive(&placeholder));

        let other = dir.join("other.txt");
        std::fs::File::create(&other).unwrap().write_all(b"hello world").unwrap();
        assert!(!is_big_archive(&other));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
