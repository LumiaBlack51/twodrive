//! Format-independent content-defined delta v1. Memory is bounded by a chunk and
//! a capped signature table; file contents and patches are streamed to disk.
use anyhow::ensure;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom, Write},
};

pub const MEDIA_TYPE: &str = "application/vnd.twodrive.delta-v1";
pub const SIGNATURE_METHOD: &str = "TD-SIGNATURE";
pub const MAX_SIGNATURE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CHUNKS: usize = 32768;
const MAX_CHUNK: usize = 4 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
#[error("delta base digest mismatch")]
pub(crate) struct BaseMismatch;
const MAGIC: &[u8; 8] = b"TDDELTA1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub length: u32,
    pub hash: [u8; 32],
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub version: u8,
    pub size: u64,
    pub hash: [u8; 32],
    pub min_chunk: usize,
    pub chunks: Vec<Chunk>,
}
impl Signature {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.version == 1 && self.chunks.len() <= MAX_CHUNKS,
            "unsupported signature"
        );
        ensure!(
            self.min_chunk.is_power_of_two() && (8192..=MAX_CHUNK / 4).contains(&self.min_chunk),
            "invalid chunk parameters"
        );
        let mut size = 0u64;
        for (index, chunk) in self.chunks.iter().enumerate() {
            let length = chunk.length as usize;
            ensure!(
                length > 0 && length <= self.min_chunk * 4,
                "invalid chunk length"
            );
            ensure!(
                index + 1 == self.chunks.len() || length >= self.min_chunk,
                "short interior chunk"
            );
            size = size
                .checked_add(length as u64)
                .ok_or_else(|| anyhow::anyhow!("signature overflow"))?;
        }
        ensure!(size == self.size, "signature size mismatch");
        Ok(())
    }
}

// Fixed deterministic gear table, independent of platform, paths or file type.
fn gear(byte: u8) -> u64 {
    let mut x = (byte as u64).wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}
fn chunks(
    source: &mut impl Read,
    minimum: usize,
    mut consume: impl FnMut(&[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let table: [u64; 256] = std::array::from_fn(|n| gear(n as u8));
    let mut buffer = [0; 64 * 1024];
    let mut chunk = Vec::with_capacity(minimum * 4);
    let mut rolling = 0u64;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            chunk.push(*byte);
            rolling = rolling.wrapping_shl(1).wrapping_add(table[*byte as usize]);
            if chunk.len() >= minimum
                && (rolling & (minimum as u64 * 2 - 1) == 0 || chunk.len() == minimum * 4)
            {
                consume(&chunk)?;
                chunk.clear();
                rolling = 0;
            }
        }
    }
    if !chunk.is_empty() {
        consume(&chunk)?;
    }
    Ok(())
}

pub fn signature(source: &mut impl Read, size: u64) -> anyhow::Result<Signature> {
    let mut minimum = 8192usize;
    while size.div_ceil(minimum as u64) > MAX_CHUNKS as u64 && minimum < MAX_CHUNK / 4 {
        minimum *= 2;
    }
    ensure!(
        size.div_ceil(minimum as u64) <= MAX_CHUNKS as u64,
        "base too large for delta v1"
    );
    let mut result = Signature {
        version: 1,
        size,
        hash: [0; 32],
        min_chunk: minimum,
        chunks: Vec::new(),
    };
    let mut hash = Sha256::new();
    chunks(source, minimum, |data| {
        ensure!(
            result.chunks.len() < MAX_CHUNKS,
            "base changed while signing"
        );
        hash.update(data);
        result.chunks.push(Chunk {
            length: data.len() as u32,
            hash: Sha256::digest(data).into(),
        });
        Ok(())
    })?;
    result.hash = hash.finalize().into();
    result.validate()?;
    Ok(result)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub size: u64,
    pub reused: u64,
    pub literal: u64,
    pub wire: u64,
}
/// Construct a complete patch before choosing it over a full upload. Cancellation
/// is checked during hashing as well as network transfer by the caller's reader.
pub fn encode(
    source: &mut impl Read,
    base: &Signature,
    patch: &mut (impl Write + Seek),
) -> anyhow::Result<Stats> {
    base.validate()?;
    let mut index = HashMap::new();
    let mut offset = 0u64;
    for chunk in &base.chunks {
        index.entry((chunk.hash, chunk.length)).or_insert(offset);
        offset += chunk.length as u64;
    }
    patch.write_all(MAGIC)?;
    patch.write_all(&base.hash)?;
    patch.write_all(&[0; 40])?; // target length and SHA-256 filled after streaming
    let mut stats = Stats {
        wire: 81,
        ..Stats::default()
    };
    let mut hash = Sha256::new();
    chunks(source, base.min_chunk, |data| {
        hash.update(data);
        stats.size = stats
            .size
            .checked_add(data.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("target overflow"))?;
        let length = data.len() as u32;
        let digest: [u8; 32] = Sha256::digest(data).into();
        if let Some(offset) = index.get(&(digest, length)) {
            patch.write_all(&[1])?;
            patch.write_all(&offset.to_be_bytes())?;
            patch.write_all(&length.to_be_bytes())?;
            stats.reused += length as u64;
            stats.wire += 13;
        } else {
            patch.write_all(&[2])?;
            patch.write_all(&length.to_be_bytes())?;
            patch.write_all(data)?;
            stats.literal += length as u64;
            stats.wire += 5 + length as u64;
        }
        Ok(())
    })?;
    patch.write_all(&[0])?;
    patch.seek(SeekFrom::Start(40))?;
    patch.write_all(&stats.size.to_be_bytes())?;
    patch.write_all(&hash.finalize())?;
    patch.rewind()?;
    Ok(stats)
}

fn read_array<const N: usize>(reader: &mut impl Read) -> anyhow::Result<[u8; N]> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}
/// The destination is always a temporary file. No caller may commit it on error.
pub fn apply(
    base: &mut (impl Read + Seek),
    patch: &mut impl Read,
    destination: &mut impl Write,
    max_size: u64,
) -> anyhow::Result<Stats> {
    ensure!(
        &read_array::<8>(patch)? == MAGIC,
        "unsupported delta format"
    );
    let expected_base = read_array::<32>(patch)?;
    let size = u64::from_be_bytes(read_array(patch)?);
    ensure!(size <= max_size, "delta result exceeds upload limit");
    let expected_target = read_array::<32>(patch)?;
    let base_size = base.seek(SeekFrom::End(0))?;
    base.rewind()?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = base.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if <[u8; 32]>::from(hash.finalize()) != expected_base {
        return Err(BaseMismatch.into());
    }
    let mut hash = Sha256::new();
    let mut stats = Stats::default();
    loop {
        let tag = read_array::<1>(patch)?[0];
        if tag == 0 {
            break;
        }
        ensure!(tag == 1 || tag == 2, "invalid delta operation");
        let offset = if tag == 1 {
            u64::from_be_bytes(read_array(patch)?)
        } else {
            0
        };
        let length = u32::from_be_bytes(read_array(patch)?) as u64;
        ensure!(
            length > 0 && length <= MAX_CHUNK as u64 && length <= size.saturating_sub(stats.size),
            "invalid delta length"
        );
        if tag == 1 {
            ensure!(
                offset <= base_size && length <= base_size - offset,
                "delta copy out of range"
            );
            base.seek(SeekFrom::Start(offset))?;
            stats.reused += length;
        } else {
            stats.literal += length;
        }
        let mut remaining = length;
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            if tag == 1 {
                base.read_exact(&mut buffer[..count])?;
            } else {
                patch.read_exact(&mut buffer[..count])?;
            }
            destination.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
            remaining -= count as u64;
        }
        stats.size += length;
    }
    ensure!(patch.read(&mut [0])? == 0, "trailing delta bytes");
    ensure!(
        stats.size == size && <[u8; 32]>::from(hash.finalize()) == expected_target,
        "delta result digest mismatch"
    );
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn data() -> Vec<u8> {
        let mut x = 0x123456789abcdefu64;
        (0..2 * 1024 * 1024)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }
    #[test]
    fn insert_delete_modify_and_truncate_reuse_binary_content() {
        let original = data();
        let signature = signature(&mut original.as_slice(), original.len() as u64).unwrap();
        let mut edited = original.clone();
        edited.splice(
            17000..17000,
            b"inserted document annotation".iter().copied(),
        );
        edited.drain(400000..410000);
        edited[900000..900100].fill(77);
        edited.truncate(1800000);
        for target in [&edited[..], &original[..], &[][..]] {
            let mut patch = Cursor::new(Vec::new());
            let stats = encode(&mut &target[..], &signature, &mut patch).unwrap();
            let mut output = Vec::new();
            apply(
                &mut Cursor::new(&original),
                &mut patch,
                &mut output,
                original.len() as u64,
            )
            .unwrap();
            assert_eq!(output, target);
            assert_eq!(stats.wire, patch.get_ref().len() as u64);
            if !target.is_empty() {
                assert!(stats.reused > target.len() as u64 * 9 / 10);
            }
        }
    }
    #[test]
    fn malformed_patch_wrong_base_and_limits_never_validate() {
        let original = data();
        let signature = signature(&mut original.as_slice(), original.len() as u64).unwrap();
        let mut patch = Cursor::new(Vec::new());
        encode(&mut original.as_slice(), &signature, &mut patch).unwrap();
        let bytes = patch.into_inner();
        for length in [0, 8, 40, 80, bytes.len() - 1] {
            assert!(
                apply(
                    &mut Cursor::new(&original),
                    &mut &bytes[..length],
                    &mut Vec::new(),
                    u64::MAX
                )
                .is_err()
            );
        }
        let mut wrong = original.clone();
        wrong[0] ^= 1;
        assert!(
            apply(
                &mut Cursor::new(wrong),
                &mut bytes.as_slice(),
                &mut Vec::new(),
                u64::MAX
            )
            .is_err()
        );
        assert!(
            apply(
                &mut Cursor::new(&original),
                &mut bytes.as_slice(),
                &mut Vec::new(),
                1
            )
            .is_err()
        );
        let mut corrupt = bytes.clone();
        corrupt[48] ^= 1;
        assert!(
            apply(
                &mut Cursor::new(&original),
                &mut corrupt.as_slice(),
                &mut Vec::new(),
                u64::MAX
            )
            .is_err()
        );
        let mut corrupt = bytes.clone();
        corrupt[81..89].fill(255);
        assert!(
            apply(
                &mut Cursor::new(&original),
                &mut corrupt.as_slice(),
                &mut Vec::new(),
                u64::MAX
            )
            .is_err()
        );
        let mut extra = bytes;
        extra.push(0);
        assert!(
            apply(
                &mut Cursor::new(&original),
                &mut extra.as_slice(),
                &mut Vec::new(),
                u64::MAX
            )
            .is_err()
        );
    }
}
