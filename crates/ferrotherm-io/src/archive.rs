// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! File sets from a folder, .zip, .tgz/.tar.gz or .tar archive, plus Unix
//! `compress` (.Z, LZW) decoding used by some ODB++ files.

use std::io::Read;
use std::path::Path;

/// (relative path with '/' separators, contents)
pub type Files = Vec<(String, Vec<u8>)>;

const MAX_TOTAL: u64 = 2 << 30; // 2 GiB safety limit

pub fn load(path: &Path) -> Result<Files, String> {
    let name = path.to_string_lossy().to_ascii_lowercase();
    if path.is_dir() {
        let mut out = Vec::new();
        walk(path, path, &mut out)?;
        return Ok(out);
    }
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if name.ends_with(".zip") {
        unzip(&data)
    } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        let gz = flate2::read::GzDecoder::new(&data[..]);
        untar(gz)
    } else if name.ends_with(".tar") {
        untar(&data[..])
    } else {
        Ok(vec![(
            path.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            data,
        )])
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Files) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out)?;
        } else if let Ok(d) = std::fs::read(&p) {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, d));
        }
    }
    Ok(())
}

pub fn unzip(data: &[u8]) -> Result<Files, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| e.to_string())?;
        if f.is_dir() {
            continue;
        }
        total += f.size();
        if total > MAX_TOTAL {
            return Err("archive too large".into());
        }
        let mut v = Vec::new();
        f.read_to_end(&mut v).map_err(|e| e.to_string())?;
        out.push((f.name().replace('\\', "/"), v));
    }
    Ok(out)
}

pub fn untar<R: Read>(r: R) -> Result<Files, String> {
    let mut a = tar::Archive::new(r);
    let mut out = Vec::new();
    let mut total = 0u64;
    for e in a.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| e.to_string())?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        total += e.size();
        if total > MAX_TOTAL {
            return Err("archive too large".into());
        }
        let p = e
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let mut v = Vec::new();
        e.read_to_end(&mut v).map_err(|e| e.to_string())?;
        out.push((p, v));
    }
    Ok(out)
}

/// Decode Unix `compress` (.Z) data (LZW, magic 1F 9D).
pub fn uncompress_z(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 3 || data[0] != 0x1f || data[1] != 0x9d {
        return Err("not a .Z file".into());
    }
    let maxbits = (data[2] & 0x1f) as u32;
    let block = data[2] & 0x80 != 0;
    if !(9..=16).contains(&maxbits) {
        return Err("bad .Z header".into());
    }
    let mut prefix: Vec<u32> = vec![0; 1 << maxbits];
    let mut suffix: Vec<u8> = vec![0; 1 << maxbits];
    for i in 0..256 {
        suffix[i] = i as u8;
    }
    let first_free = if block { 257 } else { 256 };
    let mut free = first_free as u32;
    let mut bits = 9u32;
    let mut out = Vec::new();
    let body = &data[3..];
    let mut bitpos: usize = 0;
    let mut old: Option<u32> = None;
    let mut fin = 0u8;
    let mut stack = Vec::new();
    // codes are read in groups of `bits` codes; a width change discards the
    // rest of the current group (classic compress quirk)
    let mut group_start = 0usize;
    loop {
        if bitpos + bits as usize > body.len() * 8 {
            break;
        }
        let mut code = 0u32;
        for k in 0..bits as usize {
            let b = bitpos + k;
            if body[b / 8] >> (b % 8) & 1 == 1 {
                code |= 1 << k;
            }
        }
        bitpos += bits as usize;
        if block && code == 256 {
            free = first_free as u32;
            // skip to the end of the group
            let gbits = (bits as usize) * 8;
            let used = bitpos - group_start;
            bitpos += (gbits - used % gbits) % gbits;
            bits = 9;
            group_start = bitpos;
            old = None;
            continue;
        }
        let Some(o) = old else {
            if code > 255 {
                return Err("corrupt .Z data".into());
            }
            out.push(code as u8);
            fin = code as u8;
            old = Some(code);
            continue;
        };
        let incode = code;
        stack.clear();
        let mut c = code;
        if c >= free {
            if c > free {
                return Err("corrupt .Z data".into());
            }
            stack.push(fin);
            c = o;
        }
        while c >= 256 {
            stack.push(suffix[c as usize]);
            c = prefix[c as usize];
        }
        fin = c as u8;
        stack.push(fin);
        out.extend(stack.iter().rev());
        if free < (1 << maxbits) {
            prefix[free as usize] = o;
            suffix[free as usize] = fin;
            free += 1;
        }
        old = Some(incode);
        if free >= (1 << bits) && bits < maxbits {
            let gbits = (bits as usize) * 8;
            let used = bitpos - group_start;
            bitpos += (gbits - used % gbits) % gbits;
            bits += 1;
            group_start = bitpos;
        }
    }
    Ok(out)
}

/// Text of a file, decompressing `.z` variants.
pub fn text_of(name: &str, data: &[u8]) -> String {
    if name.to_ascii_lowercase().ends_with(".z") {
        if let Ok(d) = uncompress_z(data) {
            return String::from_utf8_lossy(&d).to_string();
        }
    }
    String::from_utf8_lossy(data).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode files produced by ncompress (set FT_ZTEST to the folder with
    /// zt.txt.Z + the original under testdata name).
    #[test]
    fn lzw_matches_compress() {
        let Some(d) = std::env::var_os("FT_ZTEST").map(std::path::PathBuf::from) else {
            return;
        };
        for (z, orig) in [
            ("zt.txt.Z", "dl/pic_programmer_v7.kicad_pcb"),
            ("rnd2.bin.Z", "rnd.bin"),
        ] {
            let zd = std::fs::read(d.join(z)).unwrap();
            let o = std::fs::read(d.join(orig)).unwrap();
            let u = uncompress_z(&zd).unwrap();
            assert_eq!(u.len(), o.len(), "{z}");
            assert!(u == o, "{z} content differs");
        }
    }
}
