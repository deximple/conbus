//! The derived sidecar cache must be record-for-record what a plain scan
//! plus `meta` gives — order, defects and equality semantics included.

use conbus_core::clock::Timestamp;
use conbus_core::event::{Author, Body, Event, Harness, Source, ToolOutcome, VendorToolUseId};
use conbus_core::ids::BusId;
use conbus_core::record::{Entry, Record};
use conbus_core::retention::Retention;
use conbus_core::segment::{SegmentLog, meta};
use conbus_core::usage::Ledger;
use std::path::Path;

fn source() -> Source {
    Source {
        harness: Harness::new(Harness::CLAUDE_CODE),
        harness_version: Some("2.1.0".into()),
        vendor_session_id: Some("f0e1d2c3".into()),
        workspace: Some("/w".into()),
        entrypoint: None,
        backfilled_from: None,
        profile: None,
    }
}

fn entry(bus_id: BusId, event: Event) -> Entry {
    Entry {
        bus_id,
        observed_at: Some(Timestamp::now()),
        source: source(),
        vendor_turn_id: None,
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
        event,
    }
}

fn tool_use(input: Body, output: Body) -> Event {
    Event::ToolUse {
        tool: "Bash".into(),
        tool_use_id: Some(VendorToolUseId::new("toolu_01")),
        duration_ms: Some(12),
        input,
        outcome: ToolOutcome::Succeeded { output },
    }
}

/// A log mixing every body variant and a twin pair whose bodies differ and
/// one whose bodies are equal.
fn mixed_log(dir: &Path, per_segment_records: usize) {
    let mut log = SegmentLog::open_with_segment_size(dir, Retention::default(), 600).expect("open");
    let bus = BusId::mint();
    let other = BusId::mint();
    let events = [
        Event::TurnStarted {
            author: Some(Author::new("user")),
            prompt: Body::kept("a real prompt, kept verbatim"),
        },
        Event::ToolRequested {
            tool: "Bash".into(),
            tool_use_id: Some(VendorToolUseId::new("toolu_req")),
            input: Body::kept("rm -rf the kept input"),
        },
        Event::PermissionAsked {
            tool: "Edit".into(),
            input: Body::Clipped {
                text: "clipped text".into(),
                original_bytes: 4000,
            },
        },
        tool_use(
            Body::Sealed {
                alg: "xchacha20poly1305".into(),
                nonce: "a1b2c3".into(),
                ciphertext: "sealed body".into(),
                original_bytes: 999,
            },
            Body::Elided { bytes: 77 },
        ),
        Event::TurnEnded {
            state: conbus_core::event::TurnState::Complete,
            assistant_message: Some(Body::kept("the assistant said this")),
            tokens: None,
        },
        // The twin pairs: same shape, bodies that differ — and two records
        // that are wholly identical.
        tool_use(Body::kept("the first twin body"), Body::kept("out a")),
        tool_use(Body::kept("the second twin body"), Body::kept("out a")),
    ];
    for (n, event) in events.iter().enumerate() {
        let bus_id = if n % 2 == 0 { bus } else { other };
        let _ = per_segment_records;
        log.append(entry(bus_id, event.clone())).expect("append");
        // Equal twins: write the same event twice.
        if n == 0 {
            log.append(entry(bus_id, event.clone())).expect("append");
        }
    }
    drop(log);
}

#[test]
fn meta_keeps_only_what_equality_needs() {
    for body in [
        Body::kept("the same text"),
        Body::Clipped {
            text: "the same text".into(),
            original_bytes: 5000,
        },
        Body::Sealed {
            alg: "a".into(),
            nonce: "n".into(),
            ciphertext: "the same text".into(),
            original_bytes: 50,
        },
        Body::Elided { bytes: 9 },
    ] {
        let record_a = Record {
            event: tool_use(body.clone(), Body::Elided { bytes: 1 }),
            ..entry_record()
        };
        let same = meta(&record_a) == meta(&record_a);
        assert!(same);
        let record_b = Record {
            event: tool_use(Body::kept("a different text"), Body::Elided { bytes: 1 }),
            ..entry_record()
        };
        let before = record_a == record_b;
        let after = meta(&record_a) == meta(&record_b);
        assert_eq!(before, after, "equality must be preserved: {record_b:?}");
        // And a conversation body stays verbatim.
        let prompt = Record {
            event: Event::TurnStarted {
                author: None,
                prompt: body.clone(),
            },
            ..entry_record()
        };
        assert_eq!(meta(&prompt), prompt, "conversation text is not digested");
    }
    // A digest is not mistakable for text.
    let record = Record {
        event: tool_use(Body::kept("plain text"), Body::Elided { bytes: 1 }),
        ..entry_record()
    };
    let Event::ToolUse { input, .. } = &meta(&record).event else {
        panic!("tool_use");
    };
    assert!(format!("{input:?}").contains("sha256:"));
}

fn entry_record() -> Record {
    Record {
        schema_version: conbus_core::record::SCHEMA_VERSION,
        ts: Timestamp::now(),
        seq: 1,
        bus_id: BusId::mint(),
        source: source(),
        vendor_turn_id: None,
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
        event: Event::SessionCandidate { trigger: None },
    }
}

#[test]
fn a_meta_scan_is_a_full_scan_through_meta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("log");
    std::fs::create_dir(&log_dir).expect("mkdir");
    mixed_log(&log_dir, 0);
    let cache = dir.path().join("cache");

    let full = SegmentLog::scan(&log_dir).expect("scan");
    let expected: Vec<Record> = full.records.iter().map(meta).collect();

    for no_cache in [true, false] {
        if no_cache {
            // SAFETY: tests run one process; restore it after.
            unsafe { std::env::set_var("CONBUS_NO_CACHE", "1") };
        }
        let scan = SegmentLog::scan_meta(&log_dir, Some(&cache)).expect("scan_meta");
        assert_eq!(scan.records, expected, "no_cache={no_cache}");
        assert_eq!(scan.defects, full.defects, "no_cache={no_cache}");
        if no_cache {
            unsafe { std::env::remove_var("CONBUS_NO_CACHE") };
            assert!(
                !cache.join("meta").exists(),
                "NO_CACHE must not write sidecars"
            );
        } else {
            // The second read is the sidecar-served one — still identical.
            let served = SegmentLog::scan_meta(&log_dir, Some(&cache)).expect("served");
            assert_eq!(served.records, expected);
            assert_eq!(served.defects, full.defects);
            assert!(cache.join("meta").is_dir(), "a sidecar was written");
        }
    }

    // The fold answers the same over both.
    let a = Ledger::fold(&full.records, "2026-01-01");
    let b = Ledger::fold(&expected, "2026-01-01");
    assert_eq!(a.rows, b.rows);
    assert_eq!(a.harnesses, b.harnesses);
    assert_eq!(a.repeated, b.repeated);
    assert_eq!(a.shareable(), b.shareable());
}

#[test]
fn many_small_segments_keep_scan_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("log");
    std::fs::create_dir(&log_dir).expect("mkdir");
    let mut log =
        SegmentLog::open_with_segment_size(&log_dir, Retention::default(), 300).expect("open");
    let bus = BusId::mint();
    for n in 0..120u64 {
        log.append(entry(
            bus,
            Event::SessionCandidate {
                trigger: Some(n.to_string()),
            },
        ))
        .expect("append");
    }
    drop(log);
    let segments = std::fs::read_dir(&log_dir).expect("read").count();
    assert!(segments >= 40, "expected ~40 segments, found {segments}");

    let full = SegmentLog::scan(&log_dir).expect("scan");
    let scan = SegmentLog::scan_meta(&log_dir, Some(&dir.path().join("cache"))).expect("scan_meta");
    let expected: Vec<Record> = full.records.iter().map(meta).collect();
    assert_eq!(scan.records, expected);
    assert_eq!(scan.defects, full.defects);
}

#[test]
fn a_grown_segment_is_rebuilt_not_served_from_its_old_sidecar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("log");
    std::fs::create_dir(&log_dir).expect("mkdir");
    let mut log =
        SegmentLog::open_with_segment_size(&log_dir, Retention::default(), 400).expect("open");
    let bus = BusId::mint();
    for n in 0..6u64 {
        log.append(entry(
            bus,
            Event::SessionCandidate {
                trigger: Some(n.to_string()),
            },
        ))
        .expect("append");
    }
    drop(log);

    let cache = dir.path().join("cache");
    let first = SegmentLog::build_cache(&log_dir, &cache).expect("build");
    assert!(first.built >= 1);
    // One closed segment gains a line: its sidecar must not answer for it.
    let closed = std::fs::read_dir(&log_dir)
        .expect("read")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| !p.ends_with("segment-00000000.jsonl") || true)
        .min()
        .expect("a segment");
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&closed)
        .expect("open append");
    writeln!(
        file,
        "{{\"v\":3,\"ts\":\"2026-11-01T00:00:09.000Z\",\"seq\":99,\"bus_id\":\"{bus}\",\"harness\":\"claude_code\",\"event\":\"session_candidate\"}}"
    )
    .expect("append line");
    drop(file);

    let scan = SegmentLog::scan_meta(&log_dir, Some(&cache)).expect("scan_meta");
    assert!(
        scan.records.iter().any(|r| r.seq == 99),
        "the grown segment was served from a stale sidecar"
    );
}

#[test]
fn a_same_length_change_is_rebuilt_not_served_from_its_old_sidecar() {
    // Damage in place, or a good copy restored over a damaged one: the name and
    // the length both stay. Served from the sidecar, a damaged segment reads as
    // healthy, and a restored one keeps reporting damage it no longer has.
    let dir = tempfile::tempdir().expect("tempdir");
    let log_dir = dir.path().join("log");
    std::fs::create_dir(&log_dir).expect("mkdir");
    let mut log =
        SegmentLog::open_with_segment_size(&log_dir, Retention::default(), 400).expect("open");
    let bus = BusId::mint();
    for n in 0..6u64 {
        log.append(entry(
            bus,
            Event::SessionCandidate {
                trigger: Some(n.to_string()),
            },
        ))
        .expect("append");
    }
    drop(log);

    let cache = dir.path().join("cache");
    let first = SegmentLog::build_cache(&log_dir, &cache).expect("build");
    assert!(first.built >= 1);
    let again = SegmentLog::build_cache(&log_dir, &cache).expect("build again");
    assert_eq!(again.built, 0, "an unchanged segment keeps its sidecar");
    let closed = std::fs::read_dir(&log_dir)
        .expect("read")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .min()
        .expect("a segment");
    let mut bytes = std::fs::read(&closed).expect("read");
    bytes[0] = b'z';
    std::fs::write(&closed, &bytes).expect("damage");
    // Set outright: two writes can share a time on a coarse clock.
    std::fs::File::options()
        .write(true)
        .open(&closed)
        .expect("open")
        .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30))
        .expect("set time");

    let scan = SegmentLog::scan_meta(&log_dir, Some(&cache)).expect("scan_meta");
    assert!(
        scan.defects.iter().any(|defect| defect.segment == closed),
        "the changed segment was served from a stale sidecar"
    );
}
