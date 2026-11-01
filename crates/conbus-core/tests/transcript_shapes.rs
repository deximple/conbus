//! The line shapes a real Claude Code transcript holds, pinned against what the
//! parser makes of them.
//!
//! `fixtures/claude_transcript_2.1.316_shapes.jsonl` is built from one real line
//! of each shape: its keys and flags are the vendor's, and every value is
//! redacted. Which shapes are turns was measured against the live log, joined by
//! `promptId` over 2,094 transcripts: of the turns the parser finds where the
//! recorder was listening, 4,553 are ones the prompt hook fired for and 9 are not
//! in the log.

use conbus_core::backfill::parse_claude_transcript;
use conbus_core::event::{Body, Event, VendorTurnId};
use conbus_core::ids::BusId;

const SHAPES: &str = include_str!("fixtures/claude_transcript_2.1.316_shapes.jsonl");
const SESSION: &str = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";

fn kept(body: &Body) -> &str {
    match body {
        Body::Kept { text } => text,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_turns_of_a_real_transcripts_shapes_are_the_ones_the_hook_fires_for() {
    let out = parse_claude_transcript(SHAPES, SESSION, BusId::mint());

    let turns: Vec<(&str, &str)> = out
        .entries
        .iter()
        .filter_map(|entry| match &entry.event {
            Event::TurnStarted { prompt, .. } => Some((
                entry
                    .vendor_turn_id
                    .as_ref()
                    .map(VendorTurnId::as_str)
                    .expect("every prompt line has an id"),
                kept(prompt),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        turns,
        vec![
            ("P1", "a prompt a person typed"),
            // P2 is a local command: caveat, name, output.
            ("P3", "a prompt through a front end"),
            (
                "P4",
                "<command-message>go</command-message>\n<command-name>/go</command-name>"
            ),
            ("P5", ""),
            (
                "P6",
                "<system-reminder>context</system-reminder>\nwords in front of a reminder"
            ),
            (
                "P7",
                "<task-notification>a background task finished</task-notification>"
            ),
            // P8 was written for the transcript only.
            ("P9", "a prompt the system sent"),
            ("P10", "a turn the person stopped"),
            ("P11", "a turn the vendor could not finish"),
            ("P12", "a turn the recorder went down in"),
            ("P13", "the last turn"),
            ("P14", "a turn a tool ended"),
        ]
    );
}

#[test]
fn only_a_turn_the_model_finished_has_an_ending() {
    let out = parse_claude_transcript(SHAPES, SESSION, BusId::mint());

    let endings: Vec<&str> = out
        .entries
        .iter()
        .filter_map(|entry| match &entry.event {
            Event::TurnEnded {
                assistant_message: Some(said),
                ..
            } => Some(kept(said)),
            _ => None,
        })
        .collect();
    assert_eq!(
        endings,
        vec![
            "first answer",
            "third answer",
            "fourth answer",
            "fifth answer",
            "sixth answer",
            "seventh answer",
            "ninth answer",
            "the last answer",
            "the closing words",
        ],
        "not the stopped turn, the failed one, or the one the recorder went down in"
    );
}

#[test]
fn what_is_not_a_turn_is_counted_under_its_own_name() {
    let out = parse_claude_transcript(SHAPES, SESSION, BusId::mint());

    let skipped = |why: &str| out.skipped.get(why).copied().unwrap_or(0);
    assert_eq!(skipped("meta line"), 4, "caveat, skill, body, image marker");
    assert_eq!(skipped("compaction summary"), 1);
    assert_eq!(skipped("transcript-only line"), 1);
    assert_eq!(skipped("interrupt line"), 2);
    assert_eq!(skipped("local command output"), 1);
    assert_eq!(skipped("assistant line the vendor wrote"), 2);
    assert_eq!(skipped("user line with no text"), 3, "three tool results");
}
