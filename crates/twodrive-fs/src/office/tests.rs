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

#[test]
fn small_office_writes_checkpoint_within_budget_and_sync_on_both_page_layouts() {
    let fixture = Fixture::new();
    let map = dir(&fixture.original);
    let old = fs::read(&fixture.original).unwrap();
    let mut expected = old.clone();
    drop(
        Office::enroll(&fixture.original, &fixture.original, true)
            .unwrap()
            .unwrap(),
    );

    for page_size in [1024, 4096] {
        if page_size == 4096 {
            // Recreate the layout of an in-flight map from 0.2.26.
            let db = Connection::open(map.join("map")).unwrap();
            db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA page_size=4096; VACUUM; PRAGMA journal_mode=WAL;").unwrap();
        }
        let mut writer = Office::open(&fixture.original).unwrap().unwrap();
        let actual: u32 = writer
            .db
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .unwrap();
        assert_eq!(actual, page_size);
        let pages: u32 = writer
            .db
            .pragma_query_value(None, "wal_autocheckpoint", |row| row.get(0))
            .unwrap();
        assert_eq!(pages * actual, 256 * 1024);
        writer.freeze().unwrap();
        let frozen = expected.clone();
        for i in 0..19 {
            let off = 4096 * i;
            let value = (i + page_size as usize / 1024) as u8;
            expected[off..off + 4096].fill(value);
            writer
                .write(off as u64, &expected[off..off + 4096])
                .unwrap();
        }
        writer.sync().unwrap();
        // Inspect a copy of the checkpointed database alone while the writer
        // remains open. No close-time checkpoint or original WAL can help it.
        let copy = fixture.root.join(format!("checkpoint-{page_size}"));
        fs::create_dir(&copy).unwrap();
        fs::copy(map.join("map"), copy.join("map")).unwrap();
        fs::hard_link(map.join("base"), copy.join("base")).unwrap();
        let saved = Office::at(&copy).unwrap();
        let mut got = vec![0; old.len()];
        assert_eq!(saved.read(LIVE, 0, &mut got).unwrap(), got.len());
        assert_eq!(got, expected);
        assert_eq!(saved.read(1, 0, &mut got).unwrap(), got.len());
        assert_eq!(got, frozen);
        writer.thaw().unwrap();
    }
}

#[test]
fn changed_media_appends_preserve_sparse_bytes_and_the_frozen_view() {
    let f = Fixture::new();
    let target = f.root.join("temp");
    File::create(&target).unwrap();
    let _basis = Office::enroll(&f.original, &f.original, true)
        .unwrap()
        .unwrap();
    let mut writer = Office::enroll(&target, &f.original, false)
        .unwrap()
        .unwrap();
    let mut expected = vec![0; 17];
    for i in 0..64 {
        let bytes = [i; 4096];
        writer.write(expected.len() as u64, &bytes).unwrap();
        expected.extend_from_slice(&bytes);
    }
    let pages: u32 = writer
        .db
        .pragma_query_value(None, "wal_autocheckpoint", |r| r.get(0))
        .unwrap();
    assert_eq!(pages * 1024, 4 * 1024 * 1024);
    writer.freeze().unwrap();
    writer.write(24, b"changed").unwrap();
    writer.sync().unwrap();
    drop(writer);
    let reopened = Office::open(&target).unwrap().unwrap();
    let mut got = vec![0; expected.len()];
    assert_eq!(reopened.read(1, 0, &mut got).unwrap(), expected.len());
    assert_eq!(got, expected);
    expected[24..31].copy_from_slice(b"changed");
    assert_eq!(reopened.read(LIVE, 0, &mut got).unwrap(), expected.len());
    assert_eq!(got, expected);
}

#[test]
#[ignore = "isolated Office write-path timing"]
fn time_small_writes_to_replacement_package() {
    use std::time::Instant;
    let f = Fixture::new();
    let old = fs::read(&f.original).unwrap();
    let mut base = Office::enroll(&f.original, &f.original, true)
        .unwrap()
        .unwrap();
    base.freeze().unwrap();
    for changed in [false, true] {
        let target = f.root.join(format!("replacement-{changed}"));
        File::create(&target).unwrap();
        let mut writer = Office::enroll(&target, &f.original, false)
            .unwrap()
            .unwrap();
        let mut next = old.clone();
        if changed {
            for b in &mut next[30 + NAME.len()..30 + NAME.len() + PAYLOAD] {
                *b ^= 0xff;
            }
        }
        let start = Instant::now();
        for (i, chunk) in next.chunks(4096).enumerate() {
            writer.write((i * 4096) as u64, chunk).unwrap();
        }
        writer.sync().unwrap();
        eprintln!(
            "changed={changed} bytes={} foreground={:?}",
            next.len(),
            start.elapsed()
        );
        let mut actual = vec![0; next.len()];
        writer.read(LIVE, 0, &mut actual).unwrap();
        assert_eq!(actual, next);
        base.read(1, 0, &mut actual).unwrap();
        assert_eq!(actual, old);
    }
}

#[test]
fn background_checkpoint_can_flush_references_while_the_writer_stays_open() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let mut writer = Office::enroll(&f.original, &f.original, true)
        .unwrap()
        .unwrap();
    writer.truncate(0).unwrap();
    writer.sync().unwrap();
    // Prevent both SQLite's automatic checkpoint and a last-connection-close
    // checkpoint from satisfying the assertion on behalf of the worker.
    writer
        .db
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    let source = fs::read(&f.original).unwrap();
    let directory = dir(&f.original);
    let copied = f.root.join("checkpoint-copy");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut end = 0;
    loop {
        // The single process-wide worker can drop a hint if another file is busy.
        for _ in 0..checkpoint::HINT_WRITES {
            writer.write(end as u64, &source[end..end + 4096]).unwrap();
            end += 4096;
        }
        std::thread::sleep(Duration::from_millis(15));
        let guard = File::open(&directory).unwrap();
        guard.lock().unwrap();
        fs::copy(directory.join("map"), &copied).unwrap();
        let main_db = Connection::open(&copied).unwrap();
        let checkpointed: usize = main_db
            .query_row("SELECT size FROM views WHERE id=0", [], |r| r.get(0))
            .unwrap();
        if checkpointed > 0 {
            assert!(checkpointed <= end);
            break;
        }
        assert!(
            Instant::now() < deadline
                && end + checkpoint::HINT_WRITES as usize * 4096 <= source.len()
        );
    }
}
