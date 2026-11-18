//! Index raw ZIP entry payloads without inflating them. Unsupported layouts simply
//! use the ordinary byte store. Index limits bound work on untrusted packages.
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

const MAX_DIRECTORY: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 8192;
const MAX_NAME: usize = 1024;
const MAX_HEADER: usize = 4096;

fn u16_at(data: &[u8], pos: usize) -> usize {
    u16::from_le_bytes([data[pos], data[pos + 1]]) as usize
}

fn u32_at(data: &[u8], pos: usize) -> u64 {
    u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as u64
}

pub struct Package {
    entries: HashMap<Vec<u8>, (u64, u64)>,
}

impl Package {
    pub fn open(file: &File) -> io::Result<Option<Self>> {
        let size = file.metadata()?.len();
        if size < 22 {
            return Ok(None);
        }
        let len = size.min(65557) as usize;
        let mut tail = vec![0; len];
        file.read_exact_at(&mut tail, size - len as u64)?;
        let Some(pos) = memchr::memmem::rfind(&tail, b"PK\x05\x06") else {
            return Ok(None);
        };
        let end = &tail[pos..];
        if end.len() < 22
            || end.len() != 22 + u16_at(end, 20)
            || u32_at(end, 4) != 0
            || u16_at(end, 8) != u16_at(end, 10)
        {
            return Ok(None);
        }
        let count = u16_at(end, 10);
        let bytes = u32_at(end, 12) as usize;
        let start = u32_at(end, 16);
        if count > MAX_ENTRIES || bytes > MAX_DIRECTORY || start + bytes as u64 > size {
            return Ok(None);
        }
        let mut directory = vec![0; bytes];
        file.read_exact_at(&mut directory, start)?;
        let mut dir = directory.as_slice();
        let mut entries = HashMap::new();
        for _ in 0..count {
            if dir.len() < 46 || &dir[..4] != b"PK\x01\x02" {
                return Ok(None);
            }
            let name_len = u16_at(dir, 28);
            let next = 46 + name_len + u16_at(dir, 30) + u16_at(dir, 32);
            if next > dir.len() {
                return Ok(None);
            }
            let payload_len = u32_at(dir, 20);
            if name_len <= MAX_NAME && payload_len >= 4096 && u16_at(dir, 8) & 1 == 0 {
                let local = u32_at(dir, 42);
                let mut header = [0; 30];
                if local + 30 <= start {
                    file.read_exact_at(&mut header, local)?;
                    let payload =
                        local + 30 + u16_at(&header, 26) as u64 + u16_at(&header, 28) as u64;
                    if &header[..4] == b"PK\x03\x04" && payload + payload_len <= start {
                        entries.insert(dir[46..46 + name_len].to_vec(), (payload, payload_len));
                    }
                }
            }
            dir = &dir[next..];
        }
        Ok((!entries.is_empty()).then_some(Self { entries }))
    }
}

#[derive(Default)]
pub struct Scan {
    pending: Vec<u8>,
    next: u64,
    // Absolute destination start/end and source start. Only a hint: bytes must match.
    entry: Option<(u64, u64, u64)>,
}

impl Scan {
    pub fn candidates(
        &mut self,
        index: &Package,
        offset: u64,
        bytes: &[u8],
    ) -> Vec<(usize, usize, u64)> {
        if self.next != offset {
            self.entry = None;
            self.pending.clear();
        }
        self.next = offset + bytes.len() as u64;
        let prefix = self.pending.len();
        let stitched;
        let input = if prefix == 0 {
            bytes
        } else {
            stitched = {
                let mut v = std::mem::take(&mut self.pending);
                v.extend_from_slice(bytes);
                v
            };
            &stitched
        };
        let origin = offset - prefix as u64;
        let mut spans = Vec::new();
        let mut pos = 0;
        while pos < input.len() {
            let at = origin + pos as u64;
            if let Some((start, end, source)) = self.entry.take()
                && (start..end).contains(&at)
            {
                let n = (end - at).min((input.len() - pos) as u64) as usize;
                let first = pos.max(prefix);
                if pos + n > first {
                    spans.push((
                        first - prefix,
                        pos + n - prefix,
                        source + (origin + first as u64 - start),
                    ));
                }
                pos += n;
                if at + (n as u64) < end {
                    self.entry = Some((start, end, source));
                }
                continue;
            }
            let Some(found) = memchr::memmem::find(&input[pos..], b"PK\x03\x04") else {
                // Carry only a suffix that can actually begin the signature.
                let rest = &input[pos..];
                let sig = b"PK\x03\x04";
                for n in (1..=3.min(rest.len())).rev() {
                    if rest.ends_with(&sig[..n]) {
                        self.pending.extend_from_slice(&sig[..n]);
                        break;
                    }
                }
                break;
            };
            pos += found;
            let hdr = &input[pos..];
            if hdr.len() < 30 {
                self.pending.extend_from_slice(hdr);
                break;
            }
            let name = u16_at(hdr, 26);
            let len = 30 + name + u16_at(hdr, 28);
            if len > MAX_HEADER || name > MAX_NAME {
                pos += 4;
                continue;
            }
            if hdr.len() < len {
                self.pending.extend_from_slice(hdr);
                break;
            }
            pos += len;
            if let Some(&(source, length)) = index.entries.get(&hdr[30..30 + name]) {
                let start = origin + pos as u64;
                self.entry = Some((start, start.saturating_add(length), source));
            }
        }
        spans
    }
}
