//! Golden segments written by earlier builds. `fixtures/README.md` names the
//! commit each one came from; the files are never edited. A line with a
//! *newer* `v` than this build's being a defect is already pinned in
//! `segment.rs`'s own tests — these pin the older direction.

use conbus_core::event::{Body, Event};
use conbus_core::record::Record;
use conbus_core::segment::{Scan, SegmentLog};

const V1: &str = include_str!("fixtures/segment-v1.jsonl");
const V2: &str = include_str!("fixtures/segment-v2.jsonl");
const V3: &str = include_str!("fixtures/segment-v3.jsonl");

fn scan(lines: &str) -> Scan {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("segment-00000000.jsonl"), lines).expect("write");
    SegmentLog::scan(dir.path()).expect("scan")
}

/// What every golden segment has in common: no defects, the eighteen records
/// the old daemon wrote, in order, each stamped from its own line.
fn common(scan: &Scan, v: u32) -> &[Record] {
    assert!(scan.defects.is_empty(), "defects: {:?}", scan.defects);
    assert_eq!(scan.records.len(), 18);
    for (seq, record) in scan.records.iter().enumerate() {
        assert_eq!(record.schema_version, v);
        assert_eq!(record.seq, seq as u64);
    }
    &scan.records
}

fn prompt_text(record: &Record) -> &str {
    match &record.event {
        Event::TurnStarted {
            prompt: Body::Kept { text },
            ..
        } => text,
        other => panic!("expected a kept TurnStarted, got {other:?}"),
    }
}

#[test]
fn a_v1_segment_reads_with_zero_defects() {
    let scan = scan(V1);
    let records = common(&scan, 1);

    // The session stamp line: seq, ts, bus_id, harness and the event kind all
    // survive. `cwd` sat inside this event back then; it is not on the stamp
    // today, so the field v1 never wrote reads as its default.
    let stamp = &records[0];
    assert_eq!(stamp.ts.to_string(), "2026-10-31T23:55:30.644Z");
    assert_eq!(
        stamp.bus_id,
        "01a19463-cbd4-7b50-b819-1c9ecf85ef18".parse().expect("id")
    );
    assert_eq!(stamp.source.harness.as_str(), "claude_code");
    assert_eq!(
        stamp.event,
        Event::SessionCandidate {
            trigger: Some("startup".into())
        }
    );
    assert!(stamp.source.workspace.is_none());
    assert!(stamp.source.entrypoint.is_none());
    assert_eq!(
        stamp.source.vendor_session_id.as_deref(),
        Some("e5fc2c6a-94d2-49eb-8cfc-00abe7087bb5")
    );

    assert_eq!(
        prompt_text(&records[1]),
        "Read note.txt and reply with only the single word it names."
    );
}

#[test]
fn a_v2_segment_reads_with_zero_defects() {
    let scan = scan(V2);
    let records = common(&scan, 2);

    // v2 moved the checkout onto the stamp; it reads as `workspace`.
    let stamp = &records[0];
    assert_eq!(
        stamp.source.workspace.as_deref(),
        Some("/private/tmp/conbus-probe.TRKEF4")
    );
    assert!(stamp.source.entrypoint.is_none());
    assert_eq!(
        prompt_text(&records[1]),
        "Read note.txt and reply with only the single word it names."
    );
}

#[test]
fn a_v3_segment_reads_with_zero_defects() {
    let scan = scan(V3);
    let records = common(&scan, 3);

    // v3 added `entrypoint`; the claude records carry it and codex, which sets
    // no equivalent, honestly does not.
    let stamp = &records[0];
    assert_eq!(
        stamp.source.workspace.as_deref(),
        Some("/private/tmp/conbus-probe.TRKEF4")
    );
    assert_eq!(
        stamp.source.entrypoint.as_ref().map(|e| e.as_str()),
        Some("cli")
    );
    assert_eq!(
        prompt_text(&records[1]),
        "Read note.txt and reply with only the single word it names."
    );
    let codex = records
        .iter()
        .find(|record| record.source.harness.as_str() == "codex")
        .expect("a codex record");
    assert!(codex.source.entrypoint.is_none());
}
