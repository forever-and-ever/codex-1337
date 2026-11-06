use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;

use pretty_assertions::assert_eq;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;

use super::FramedReader;
use super::FramedWriter;
use super::HINT_LEN;
use super::HINT_TRAILER_FIELD;
use super::quarantine::StructureScan;
use super::read_exact_at;
use super::to_index;
use super::write_all_at;

fn open_for_append(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .expect("open rollout")
}

fn random_line(rng: &mut StdRng) -> Vec<u8> {
    // Occasional lines span several 128 KiB zstd blocks.
    let len = if rng.random_bool(0.04) {
        rng.random_range(130_000..300_000)
    } else {
        rng.random_range(1..3_000)
    };
    let words = [
        &b"{\"type\":"[..],
        b"\"message\"",
        b",\"text\":",
        b"rollout ",
        b"0123",
        b"\\u00e9",
    ];
    let mut line = Vec::with_capacity(len + 1);
    while line.len() < len {
        if rng.random_bool(0.3) {
            line.push(rng.random_range(b' '..=b'~'));
        } else {
            line.extend_from_slice(words[rng.random_range(0..words.len())]);
        }
    }
    line.truncate(len);
    line.push(b'\n');
    line
}

/// The bytes sealing moved into the directory's one quarantine sidecar, if any.
fn quarantined(directory: &Path) -> Vec<u8> {
    let sidecars: Vec<_> = std::fs::read_dir(directory)
        .expect("list")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.to_string_lossy().contains(".torn-"))
        .collect();
    assert!(sidecars.len() <= 1, "one seal makes at most one sidecar");
    sidecars.first().map_or_else(Vec::new, |sidecar| {
        std::fs::read(sidecar).expect("read sidecar")
    })
}

fn read_all(path: &Path) -> Vec<u8> {
    let mut content = Vec::new();
    FramedReader::open(File::open(path).expect("open"))
        .expect("survey")
        .read_to_end(&mut content)
        .expect("read");
    content
}

#[test]
fn sealing_after_any_crash_keeps_the_acknowledged_lines_and_quarantines_the_rest() {
    let mut rng = StdRng::seed_from_u64(0x5EA1);
    for _ in 0..64 {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollout.jsonl.zst");
        // Frames close only where the test says, so it knows which bytes each step wrote.
        let mut writer = FramedWriter::open(&path, open_for_append(&path))
            .expect("create")
            .with_frame_logical_bytes(u64::MAX);
        let mut acknowledged = Vec::new();
        let lines = rng.random_range(1..48);
        let crash_at = rng.random_range(0..lines);
        for index in 0..lines {
            let line = random_line(&mut rng);
            let closes = rng.random_bool(0.15);
            let before = std::fs::metadata(&path).expect("len").len();
            let mut hint = [0; HINT_LEN];
            read_exact_at(writer.file(), &mut hint, 0).expect("hint");
            writer.append(&line, /*flush*/ true).expect("append");
            // A returned append is acknowledged; the close after it may still be torn.
            let appended = std::fs::metadata(&path).expect("len").len();
            if closes {
                writer.close_frame().expect("close");
            }
            if index < crash_at {
                acknowledged.extend_from_slice(&line);
                continue;
            }
            // The process dies while this line, or the frame close after it, is being written.
            let after = std::fs::metadata(&path).expect("len").len();
            let cut = rng.random_range(before..=after);
            let file = open_for_append(&path);
            file.set_len(cut).expect("truncate");
            if cut < after {
                // The hint is rewritten only after its trailer is durable.
                write_all_at(&file, &hint, 0).expect("restore hint");
            }
            if cut >= appended {
                acknowledged.extend_from_slice(&line);
            }
            break;
        }
        drop(writer);
        // Some cases also corrupt a closed frame's trailer: sealing keeps the frames up to that
        // one and quarantines the corrupted trailer and everything after it as corruption.
        let spans = FramedReader::open(File::open(&path).expect("open"))
            .expect("survey")
            .spans;
        let corrupted = (!spans.is_empty() && rng.random_bool(0.25)).then(|| {
            let span = spans[rng.random_range(0..spans.len())];
            let file = open_for_append(&path);
            let mut previous = [0; 1];
            read_exact_at(&file, &mut previous, span.end + 32).expect("read trailer");
            write_all_at(&file, &[previous[0] ^ 0x40], span.end + 32).expect("corrupt trailer");
            span
        });
        let crashed = std::fs::read(&path).expect("read crashed rollout");

        let mut writer = FramedWriter::open(&path, open_for_append(&path)).expect("seal");
        let dropped = quarantined(dir.path());
        let cut = crashed.len() - dropped.len();
        assert_eq!(
            &crashed[cut..],
            &dropped[..],
            "the sidecar holds exactly the cut bytes"
        );
        let sealed = std::fs::read(&path).expect("read sealed rollout");
        // Before the cut, sealing rewrites only the hint's trailer field.
        let field = to_index(HINT_TRAILER_FIELD).expect("index");
        let hint_field = field..field + 8;
        assert_eq!(&sealed[..hint_field.start], &crashed[..hint_field.start]);
        assert_eq!(&sealed[hint_field.end..cut], &crashed[hint_field.end..cut]);
        let mut scan = StructureScan::default();
        scan.feed(&dropped);
        assert_eq!(scan.found(), corrupted.is_some());
        if let Some(span) = corrupted {
            acknowledged.truncate(to_index(span.logical_end()).expect("index"));
        }
        assert_eq!(writer.logical_len(), acknowledged.len() as u64);
        for _ in 0..rng.random_range(0..4) {
            let line = random_line(&mut rng);
            writer
                .append(&line, /*flush*/ true)
                .expect("append after seal");
            acknowledged.extend_from_slice(&line);
        }
        drop(writer);
        assert_eq!(read_all(&path), acknowledged);

        // Sealed, the whole file is one standard zstd stream with valid checksums.
        drop(FramedWriter::open(&path, open_for_append(&path)).expect("reseal"));
        let decoded = zstd::stream::decode_all(File::open(&path).expect("open")).expect("decode");
        assert_eq!(decoded, acknowledged);
    }
}

#[test]
fn seeks_cross_closed_frames_and_the_open_frame() {
    let mut rng = StdRng::seed_from_u64(0x5EE4);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rollout.jsonl.zst");
    let mut writer = FramedWriter::open(&path, open_for_append(&path))
        .expect("create")
        .with_frame_logical_bytes(8 << 10);
    let mut expected = Vec::new();
    for _ in 0..300 {
        let line = random_line(&mut rng);
        writer.append(&line, /*flush*/ true).expect("append");
        expected.extend_from_slice(&line);
    }
    // The newest lines stay in the open frame.
    let mut reader = FramedReader::open(File::open(&path).expect("open")).expect("survey");
    assert!(!reader.tail.is_empty() && reader.spans.len() > 10);
    let len = expected.len() as u64;
    assert_eq!(reader.seek(SeekFrom::End(0)).expect("seek"), len);
    let mut positions = (0..200)
        .map(|_| rng.random_range(0..len))
        .collect::<Vec<_>>();
    positions.sort_unstable_by(|left, right| right.cmp(left));
    positions.push(reader.tail_start - 1);
    for start in positions {
        let count = rng.random_range(1..40_000u64);
        reader.seek(SeekFrom::Start(start)).expect("seek");
        let mut read = Vec::new();
        (&mut reader)
            .take(count)
            .read_to_end(&mut read)
            .expect("read");
        let end = to_index((start + count).min(len)).expect("index");
        assert_eq!(read, &expected[to_index(start).expect("index")..end]);
    }
    reader.seek(SeekFrom::End(-5)).expect("seek from end");
    let mut last = Vec::new();
    reader.read_to_end(&mut last).expect("read tail");
    assert_eq!(last, &expected[expected.len() - 5..]);
}

#[test]
fn readers_see_a_prefix_of_lines_while_a_writer_appends() {
    let mut rng = StdRng::seed_from_u64(0xC0C0);
    let lines = (0..1_500)
        .map(|_| random_line(&mut rng))
        .collect::<Vec<_>>();
    let expected = lines.concat();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rollout.jsonl.zst");
    let mut writer = FramedWriter::open(&path, open_for_append(&path))
        .expect("create")
        .with_frame_logical_bytes(16 << 10);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let appender = {
        let done = std::sync::Arc::clone(&done);
        std::thread::spawn(move || {
            for (index, line) in lines.iter().enumerate() {
                writer.append(line, /*flush*/ true).expect("append");
                if index % 97 == 0 {
                    writer.close_frame().expect("close");
                }
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    };
    let mut seen = 0;
    let mut snapshots = 0;
    loop {
        let finished = done.load(std::sync::atomic::Ordering::SeqCst);
        let content = read_all(&path);
        assert!(content.len() >= seen, "a later snapshot lost lines");
        assert_eq!(content, &expected[..content.len()]);
        assert!(content.is_empty() || content.ends_with(b"\n"));
        seen = content.len();
        snapshots += 1;
        if finished {
            break;
        }
    }
    appender.join().expect("writer thread");
    assert_eq!(seen, expected.len());
    assert!(snapshots > 1);
}
