use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

const NAME: &[u8] = b"ppt/media/image1.bin";
const PAYLOAD: usize = 11 * 1024 * 1024;

pub(crate) struct Fixture {
    root: PathBuf,
    pub(crate) original: PathBuf,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "twodrive-package-{}",
            crate::cache_io::unique_suffix()
        ));
        fs::create_dir(&root).unwrap();
        let original = root.join("original");
        let mut f = File::create(&original).unwrap();
        let mut hdr = [0u8; 30];
        hdr[..4].copy_from_slice(b"PK\x03\x04");
        hdr[26..28].copy_from_slice(&(NAME.len() as u16).to_le_bytes());
        f.write_all(&hdr).unwrap();
        f.write_all(NAME).unwrap();
        let data = (0..64 * 1024)
            .map(|i| ((i * 113 + i / 7) % 251) as u8)
            .collect::<Vec<_>>();
        for _ in 0..PAYLOAD / data.len() {
            f.write_all(&data).unwrap();
        }
        let central = f.stream_position().unwrap() as u32;
        let mut entry = [0; 46];
        entry[..4].copy_from_slice(b"PK\x01\x02");
        entry[20..24].copy_from_slice(&(PAYLOAD as u32).to_le_bytes());
        entry[28..30].copy_from_slice(&(NAME.len() as u16).to_le_bytes());
        f.write_all(&entry).unwrap();
        f.write_all(NAME).unwrap();
        let mut end = [0; 22];
        end[..4].copy_from_slice(b"PK\x05\x06");
        end[8] = 1;
        end[10] = 1;
        end[12..16].copy_from_slice(&((46 + NAME.len()) as u32).to_le_bytes());
        end[16..20].copy_from_slice(&central.to_le_bytes());
        f.write_all(&end).unwrap();
        Self { root, original }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn package_replace_reuses_shifted_media_and_reopens_exact_latest_and_frozen_views() {
    let fixture = Fixture::new();
    let mut original = Office::enroll(&fixture.original, &fixture.original, true)
        .unwrap()
        .unwrap();
    original.freeze().unwrap();
    let target = fixture.root.join("temp");
    File::create(&target).unwrap();
    let mut o = Office::enroll(&target, &fixture.original, false)
        .unwrap()
        .unwrap();
    let before = fs::read(&fixture.original).unwrap();
    let mut after = b"one extra byte offset".to_vec();
    after.extend_from_slice(&before);
    // Deliberately split the first signature and the name across requests.
    let mut pos = 0;
    for end in [23, 32, 64] {
        o.write(pos as u64, &after[pos..end]).unwrap();
        pos = end;
    }
    for chunk in after[pos..].chunks(37 * 1024) {
        o.write(pos as u64, chunk).unwrap();
        pos += chunk.len();
    }
    let stored: u64 =
        o.db.query_row(
            "SELECT COALESCE(sum(length(data)),0) FROM literals",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(stored < 4096, "stored {stored} literal bytes");
    o.freeze().unwrap();
    let frozen = after.clone();
    let at = 2 * 1024 * 1024;
    o.write(at as u64, b"updated bytes").unwrap();
    after[at..at + 13].copy_from_slice(b"updated bytes");
    o.truncate((at + 18) as u64).unwrap();
    after.truncate(at + 18);
    o.write((at + 350) as u64, b"end").unwrap();
    after.resize(at + 350, 0);
    after.extend_from_slice(b"end");
    o.sync().unwrap();
    drop(o);
    let mut reopened = Office::open(&target).unwrap().unwrap();
    let mut actual = vec![0; frozen.len()];
    assert_eq!(reopened.read(1, 0, &mut actual).unwrap(), frozen.len());
    assert_eq!(actual, frozen);
    actual.fill(0xaa);
    assert_eq!(reopened.read(LIVE, 0, &mut actual).unwrap(), after.len());
    assert_eq!(&actual[..after.len()], after);
    assert_eq!(fs::read(&fixture.original).unwrap(), before);
    reopened.thaw().unwrap();
    drop(reopened);
    original.thaw().unwrap();
    drop(original);
    fs::remove_file(&fixture.original).unwrap();
    fs::remove_dir_all(dir(&fixture.original)).unwrap();
    // Reopen after every merge step. Even partially folded bytes remain readable.
    for _ in 0..300 {
        let more = merge::advance(&target).unwrap();
        if let Some(o) = Office::open(&target).unwrap() {
            assert_eq!(o.read(LIVE, 0, &mut actual).unwrap(), after.len());
            assert_eq!(&actual[..after.len()], after);
        }
        if !more {
            break;
        }
    }
    assert!(!dir(&target).exists());
    assert_eq!(fs::read(target).unwrap(), after);
}

#[test]
fn malformed_zip_and_unrelated_equal_length_bytes_are_preserved() {
    let f = Fixture::new();
    let mut raw = fs::read(&f.original).unwrap();
    let mut base = Office::enroll(&f.original, &f.original, true)
        .unwrap()
        .unwrap();
    base.freeze().unwrap();
    let target = f.root.join("new");
    File::create(&target).unwrap();
    let mut o = Office::enroll(&target, &f.original, false)
        .unwrap()
        .unwrap();
    let at = 30 + NAME.len() + 123;
    raw[at] ^= 0xff;
    for (i, b) in raw.chunks(LITERAL).enumerate() {
        o.write((i * LITERAL) as u64, b).unwrap();
    }
    let mut actual = vec![0; raw.len()];
    o.read(LIVE, 0, &mut actual).unwrap();
    assert_eq!(raw, actual);
    let malformed = f.root.join("invalid");
    let mut file = File::create(&malformed).unwrap();
    file.seek(SeekFrom::Start(PAYLOAD as u64)).unwrap();
    file.write_all(b"PK\x05\x06").unwrap();
    assert!(
        Office::enroll(&malformed, &malformed, true)
            .unwrap()
            .is_none()
    );
}

#[test]
fn rejected_write_keeps_the_previous_complete_save() {
    let f = Fixture::new();
    let mut o = Office::enroll(&f.original, &f.original, true)
        .unwrap()
        .unwrap();
    o.freeze().unwrap();
    o.db.execute_batch("CREATE TEMP TRIGGER disk_full BEFORE INSERT ON literals BEGIN SELECT RAISE(ABORT,'disk full'); END;").unwrap();
    assert!(o.write(3, b"replace earlier bytes").is_err());
    let mut got = [0; 32];
    o.read(LIVE, 0, &mut got).unwrap();
    let mut want = [0; 32];
    File::open(&f.original)
        .unwrap()
        .read_exact(&mut want)
        .unwrap();
    assert_eq!(got, want);
}
