//! What the agents did, counted: the activity ledger.
//!
//! One row per UTC day, harness, workspace and attendance, holding counts and
//! nothing else — no prompt, no reply, no tool body, no error text. The row is
//! the finest grain `conbus usage` prints from, and it is deliberately the
//! shape a sync would ship: a ledger that can only say how many, never what.
//!
//! A conversation is attributed once. Its workspace is the first one it
//! reported and its attendance is the first entrypoint it carried, so an agent
//! that `cd`s into a subdirectory stays one conversation in one place, and the
//! records written before a harness reported its `cwd` are not a second,
//! placeless conversation.
//!
//! Tokens and cost are not here because they are not in the log. No hook
//! payload carries them; a reader that wants them needs the vendor's own
//! telemetry, and this module says so rather than inventing a number.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use serde::Serialize;

use crate::clock::Timestamp;
use crate::event::{Entrypoint, Event, Harness, ToolOutcome, TurnState};
use crate::ids::BusId;
use crate::record::Record;

const DAY_SECS: i64 = 24 * 60 * 60;

/// How close a second delivery of one report arrives. The widest seen on the
/// live machine is ~330 ms, from Grok, every one of whose events reached the log
/// twice; a person typing the same thing again takes longer than this.
const TWIN_WINDOW_SECS: i64 = 2;

/// A limit stop this long after the harness's previous one starts a new
/// episode. Retries inside a blocked window are one event for whoever has to
/// deal with it; the vendors' windows are hours long.
const EPISODE_GAP_SECS: i64 = 60 * 60;

/// The harness names a shared row may carry. Anything else is `other`.
const KNOWN_HARNESSES: &[&str] = &[
    Harness::CLAUDE_CODE,
    Harness::CODEX,
    Harness::CURSOR,
    Harness::GROK,
    Harness::OPENCODE,
    Harness::AGY,
    Harness::CLAUDE_CHAT,
];

/// Entrypoints that mean a person is at the other end.
///
/// Named, not inferred. `Entrypoint::is_unattended` is the reliable direction
/// and an unrecognised value is evidence of nothing, so it gets its own
/// bucket rather than being counted as a person — the opposite of the choice
/// the briefing gate makes, and for the same reason: there, silence on a
/// person is the expensive mistake; here, a report that overstates how much a
/// person did is.
const ATTENDED: &[&str] = &["cli", "claude-desktop", "vscode", "jetbrains", "remote"];

/// Whether a person was there.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attendance {
    Attended,
    Unattended,
    /// No entrypoint, or one this build does not recognise. Nearly every
    /// record from a harness other than Claude Code is here: only Claude Code
    /// says.
    Unknown,
}

impl Attendance {
    pub fn of(entrypoint: Option<&Entrypoint>) -> Self {
        match entrypoint {
            Some(entrypoint) if entrypoint.is_unattended() => Attendance::Unattended,
            Some(entrypoint) if ATTENDED.contains(&entrypoint.as_str()) => Attendance::Attended,
            _ => Attendance::Unknown,
        }
    }
}

/// How many of each thing happened. Additive: two of these sum.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
pub struct Counts {
    /// Prompts submitted.
    ///
    /// There is no "completed" beside it. A clean end is the one thing a
    /// vendor reports more than once by design — Cursor writes it from two
    /// hooks — so a count of them measures the adapter, not the work.
    pub turns: u64,
    /// Turns stopped by a usage limit, whatever the vendor calls it.
    pub limit_stops: u64,
    /// Runs of limit stops: a new one after an hour without any.
    pub limit_episodes: u64,
    /// Turns stopped by the account: authentication, billing, a hold.
    pub account_stops: u64,
    /// Turns the vendor failed: overloaded, a server error.
    pub vendor_errors: u64,
    /// Turns the request itself could not be served: invalid, too long, no
    /// such model.
    pub request_faults: u64,
    /// Stops whose reason this build does not recognise. Unclassified, not
    /// guessed.
    pub other_stops: u64,
    /// Turns a person interrupted.
    pub user_stops: u64,
    /// Subagent runs that finished. Not turns: the vendor opens a subagent
    /// with no prompt, so it has an end and no start.
    pub subagents: u64,
    /// Tool calls that ran.
    pub tool_calls: u64,
    /// Calls that ran and failed on their own.
    pub tool_failures: u64,
    /// Calls a person stopped. Not a failure of the tool.
    pub tool_interrupts: u64,
    /// Calls the harness refused to run.
    pub tool_denials: u64,
    /// Times a person was asked whether a tool may run.
    pub permission_asks: u64,
    /// Summed tool durations, where the vendor measured them.
    pub tool_ms: u64,
    /// Briefings handed to a session.
    pub briefings: u64,
    /// `routed` records, whoever decided them.
    pub hops: u64,
    /// Second deliveries of a report already counted here — which harness
    /// double-delivers, and when.
    pub repeated: u64,
}

impl Counts {
    /// Every field by name, so that a new count cannot be left out of a sum.
    fn add(&mut self, other: &Counts) {
        let Counts {
            turns,
            limit_stops,
            limit_episodes,
            account_stops,
            vendor_errors,
            request_faults,
            other_stops,
            user_stops,
            subagents,
            tool_calls,
            tool_failures,
            tool_interrupts,
            tool_denials,
            permission_asks,
            tool_ms,
            briefings,
            hops,
            repeated,
        } = other;
        self.turns += turns;
        self.limit_stops += limit_stops;
        self.limit_episodes += limit_episodes;
        self.account_stops += account_stops;
        self.vendor_errors += vendor_errors;
        self.request_faults += request_faults;
        self.other_stops += other_stops;
        self.user_stops += user_stops;
        self.subagents += subagents;
        self.tool_calls += tool_calls;
        self.tool_failures += tool_failures;
        self.tool_interrupts += tool_interrupts;
        self.tool_denials += tool_denials;
        self.permission_asks += permission_asks;
        self.tool_ms += tool_ms;
        self.briefings += briefings;
        self.hops += hops;
        self.repeated += repeated;
    }

    /// Exhaustive on purpose: a new event has to be a decision here, not a
    /// silent zero.
    fn count(&mut self, record: &Record) {
        let subagent = record.vendor_agent_id.is_some();
        match &record.event {
            // A subagent's prompt is the parent's tool call, not a person's.
            Event::TurnStarted { .. } if subagent => {}
            Event::TurnStarted { .. } => self.turns += 1,
            Event::TurnEnded { .. } if subagent => self.subagents += 1,
            Event::TurnEnded { state, .. } => match Stop::of(state) {
                None => {}
                Some(Stop::Limit) => self.limit_stops += 1,
                Some(Stop::Account) => self.account_stops += 1,
                Some(Stop::Vendor) => self.vendor_errors += 1,
                Some(Stop::Request) => self.request_faults += 1,
                Some(Stop::Other) => self.other_stops += 1,
                Some(Stop::Person) => self.user_stops += 1,
            },
            Event::ToolUse {
                duration_ms,
                outcome,
                ..
            } => {
                self.tool_calls += 1;
                self.tool_ms += duration_ms.unwrap_or(0);
                match outcome {
                    ToolOutcome::Succeeded { .. } => {}
                    ToolOutcome::Failed {
                        interrupted: true, ..
                    } => self.tool_interrupts += 1,
                    ToolOutcome::Failed { .. } => self.tool_failures += 1,
                }
            }
            Event::ToolDenied { .. } => self.tool_denials += 1,
            Event::PermissionAsked { .. } => self.permission_asks += 1,
            Event::Briefed { .. } => self.briefings += 1,
            Event::Routed { .. } => self.hops += 1,
            Event::SessionCandidate { .. }
            | Event::ToolRequested { .. }
            | Event::ToolBatchEnded { .. }
            | Event::Notified { .. }
            | Event::Continued { .. }
            | Event::Summarised { .. }
            | Event::Leased { .. }
            | Event::Aliased { .. }
            | Event::SessionEnded { .. } => {}
        }
    }
}

/// What an operator would do about a stop.
///
/// The adapters split stops for the router — capacity or not — and keep the
/// vendor's words. This reads those words again for a different question.
/// Claude Code's reasons are the vendor's enum; Cursor, OpenCode and Grok
/// report their own, and anything not named here is `Other`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stop {
    Limit,
    Account,
    Vendor,
    Request,
    Other,
    Person,
}

impl Stop {
    fn of(state: &TurnState) -> Option<Self> {
        let first = |reason: &str| {
            reason
                .split(':')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        match state {
            TurnState::Complete => None,
            TurnState::InterruptedUser => Some(Stop::Person),
            // The router treats an overloaded vendor as capacity, rightly for
            // a hop; for whoever is paying, it is the vendor's fault, not a
            // limit they hit.
            TurnState::InterruptedQuota { reason } => Some(match first(reason).as_str() {
                "overloaded" => Stop::Vendor,
                _ => Stop::Limit,
            }),
            TurnState::InterruptedPolicy { reason } => Some(match first(reason).as_str() {
                "authentication_failed"
                | "oauth_org_not_allowed"
                | "account_on_hold"
                | "billing_error"
                | "cloud_credential_error" => Stop::Account,
                "server_error" | "overloaded" => Stop::Vendor,
                "invalid_request" | "model_not_found" | "max_output_tokens" => Stop::Request,
                _ => Stop::Other,
            }),
        }
    }
}

/// One day of one harness in one workspace, attended or not.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Row {
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    pub harness: String,
    /// The path the conversation first reported. Local-only: a sync must not
    /// ship it as written.
    pub workspace: Option<String>,
    pub attendance: Attendance,
    /// Conversations with a record on this day. Not additive across days — a
    /// conversation that runs past midnight is in two rows.
    pub conversations: u64,
    #[serde(flatten)]
    pub counts: Counts,
}

/// A row as a sync would send it: `docs/rfd/0002-metadata-sync.md` with
/// workspaces off. There is no workspace field at all — absent, not empty.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct SharedRow {
    pub day: String,
    pub harness: String,
    pub attendance: Attendance,
    pub conversations: u64,
    #[serde(flatten)]
    pub counts: Counts,
}

/// One harness across the whole window.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HarnessTotal {
    pub harness: String,
    /// Distinct over the window, unlike the rows.
    pub conversations: u64,
    pub attended: u64,
    pub unattended: u64,
    pub unknown: u64,
    pub counts: Counts,
}

/// The window, folded.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Ledger {
    /// Live records inside the window.
    pub records: u64,
    /// Recovered history, which is not activity this recorder saw.
    pub backfilled: u64,
    pub before_window: u64,
    /// Second deliveries of a report already counted: the same event, naming
    /// the same vendor call or turn, inside the twin window.
    pub repeated: u64,
    pub rows: Vec<Row>,
    /// Most conversations first.
    pub harnesses: Vec<HarnessTotal>,
    /// Tools by failures, most first. Failures only: an interrupt is a person.
    pub failing_tools: Vec<(String, u64)>,
    /// Workspaces by turns, most first.
    pub workspaces: Vec<(String, u64)>,
}

impl Ledger {
    /// Everything at or after `since`, compared as text — a day like
    /// `2026-09-17` works, because the timestamp format is fixed-width.
    pub fn fold(records: &[Record], since: &str) -> Self {
        let places = attribute(records);
        let mut tally = Tally::default();
        for record in records {
            tally.add(record, since, &places);
        }
        tally.finish()
    }

    /// The rows RFD 0002 lets a sync send, with workspaces off.
    ///
    /// Rows that differed only by folder merge, and their conversation counts
    /// add up rather than overlap: a conversation is attributed to one folder,
    /// the first it reported, so it is in exactly one of the rows merged.
    pub fn shareable(&self) -> Vec<SharedRow> {
        let mut merged: BTreeMap<(&str, &str, Attendance), SharedRow> = BTreeMap::new();
        for row in &self.rows {
            let shared = merged
                .entry((&row.day, shared_harness(&row.harness), row.attendance))
                .or_insert_with(|| SharedRow {
                    day: row.day.clone(),
                    harness: shared_harness(&row.harness).to_string(),
                    attendance: row.attendance,
                    conversations: 0,
                    counts: Counts::default(),
                });
            shared.conversations += row.conversations;
            shared.counts.add(&row.counts);
        }
        merged.into_values().collect()
    }
}

/// The fold's working state: sets where the ledger has counts.
#[derive(Default)]
struct Tally {
    records: u64,
    backfilled: u64,
    before_window: u64,
    repeated: u64,
    /// When each report that names a vendor thing was last counted.
    delivered: HashMap<(BusId, u64), Timestamp>,
    /// Each harness's last limit stop, for telling episodes apart.
    last_limit: HashMap<String, Timestamp>,
    rows: BTreeMap<RowKey, (HashSet<BusId>, Counts)>,
    harnesses: BTreeMap<String, (HashMap<BusId, Attendance>, Counts)>,
    failing: BTreeMap<String, u64>,
    workspaces: BTreeMap<String, u64>,
}

impl Tally {
    fn add(&mut self, record: &Record, since: &str, places: &HashMap<BusId, Place>) {
        if record.source.backfilled_from.is_some() {
            self.backfilled += 1;
            return;
        }
        let ts = record.ts.to_string();
        if ts.as_str() < since {
            self.before_window += 1;
            return;
        }
        self.records += 1;
        let place = places.get(&record.bus_id);
        let attendance = Attendance::of(place.and_then(|p| p.entrypoint.as_ref()));
        let workspace = place.and_then(|p| p.workspace.clone());
        let harness = record.source.harness.as_str().to_string();
        let key = RowKey {
            day: ts.get(..10).unwrap_or(&ts).to_string(),
            harness: harness.clone(),
            workspace: workspace.clone(),
            attendance,
        };
        if self.is_twin(record) {
            self.repeated += 1;
            self.rows.entry(key).or_default().1.repeated += 1;
            self.harnesses.entry(harness).or_default().1.repeated += 1;
            return;
        }
        let episode = u64::from(self.starts_episode(record, &harness));
        let (seen, counts) = self.rows.entry(key).or_default();
        seen.insert(record.bus_id);
        counts.count(record);
        counts.limit_episodes += episode;
        let (seen, counts) = self.harnesses.entry(harness).or_default();
        seen.insert(record.bus_id, attendance);
        counts.count(record);
        counts.limit_episodes += episode;
        self.note_hotspots(record, workspace);
    }

    /// A second delivery of a report already counted, inside the twin window.
    fn is_twin(&mut self, record: &Record) -> bool {
        let Some(fingerprint) = fingerprint(record) else {
            return false;
        };
        let key = (record.bus_id, fingerprint);
        let twin = self
            .delivered
            .get(&key)
            .is_some_and(|first| record.ts.seconds_since(*first) < TWIN_WINDOW_SECS);
        if !twin {
            self.delivered.insert(key, record.ts);
        }
        twin
    }

    /// Whether this record is a limit stop that begins a new episode.
    fn starts_episode(&mut self, record: &Record, harness: &str) -> bool {
        let Event::TurnEnded { state, .. } = &record.event else {
            return false;
        };
        if record.vendor_agent_id.is_some() || Stop::of(state) != Some(Stop::Limit) {
            return false;
        }
        let previous = self.last_limit.insert(harness.to_string(), record.ts);
        previous.is_none_or(|last| record.ts.seconds_since(last) > EPISODE_GAP_SECS)
    }

    /// The tools that fail and the folders that are busy.
    fn note_hotspots(&mut self, record: &Record, workspace: Option<String>) {
        if let Event::ToolUse {
            tool,
            outcome: ToolOutcome::Failed {
                interrupted: false, ..
            },
            ..
        } = &record.event
        {
            *self.failing.entry(tool.clone()).or_default() += 1;
        }
        if let (Event::TurnStarted { .. }, None, Some(workspace)) =
            (&record.event, &record.vendor_agent_id, workspace)
        {
            *self.workspaces.entry(workspace).or_default() += 1;
        }
    }

    fn finish(self) -> Ledger {
        let rows = self
            .rows
            .into_iter()
            .map(|(key, (seen, counts))| Row {
                day: key.day,
                harness: key.harness,
                workspace: key.workspace,
                attendance: key.attendance,
                conversations: seen.len() as u64,
                counts,
            })
            .collect();
        let mut harnesses: Vec<HarnessTotal> = self
            .harnesses
            .into_iter()
            .map(|(harness, (seen, counts))| {
                let with = |wanted| seen.values().filter(|a| **a == wanted).count() as u64;
                HarnessTotal {
                    harness,
                    conversations: seen.len() as u64,
                    attended: with(Attendance::Attended),
                    unattended: with(Attendance::Unattended),
                    unknown: with(Attendance::Unknown),
                    counts,
                }
            })
            .collect();
        // Stable over the map's name order, so ties print by name.
        harnesses.sort_by(|a, b| b.conversations.cmp(&a.conversations));
        Ledger {
            records: self.records,
            backfilled: self.backfilled,
            before_window: self.before_window,
            repeated: self.repeated,
            rows,
            harnesses,
            failing_tools: most_first(self.failing),
            workspaces: most_first(self.workspaces),
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RowKey {
    day: String,
    harness: String,
    workspace: Option<String>,
    attendance: Attendance,
}

/// What a second delivery of this record would look like.
///
/// Only for a record that names a vendor thing — a call, a turn, a subagent.
/// An id alone is not enough: Claude Code's `prompt_id` is a process-global
/// slot and Codex keeps one turn id while a person steers it, so two different
/// prompts under one id are two submissions. The whole event has to match,
/// inside the twin window. A record that names nothing cannot be told apart
/// from a real second call, and is always counted. Sealed bodies carry a fresh
/// nonce, so their twins are counted twice.
fn fingerprint(record: &Record) -> Option<u64> {
    let names_a_call = matches!(
        &record.event,
        Event::ToolUse {
            tool_use_id: Some(_),
            ..
        } | Event::ToolRequested {
            tool_use_id: Some(_),
            ..
        } | Event::ToolDenied {
            tool_use_id: Some(_),
            ..
        }
    );
    if !names_a_call && record.vendor_turn_id.is_none() && record.vendor_agent_id.is_none() {
        return None;
    }
    let mut hasher = DefaultHasher::new();
    record.vendor_turn_id.hash(&mut hasher);
    record.vendor_agent_id.hash(&mut hasher);
    record.event.hash(&mut hasher);
    Some(hasher.finish())
}

/// Where a conversation is, and who is there, as it first said.
#[derive(Default)]
struct Place {
    workspace: Option<String>,
    entrypoint: Option<Entrypoint>,
}

/// Every live record, not only the window's: a conversation that began before
/// the window keeps the place and the person it began with.
fn attribute(records: &[Record]) -> HashMap<BusId, Place> {
    let mut places: HashMap<BusId, Place> = HashMap::new();
    for record in records {
        if record.source.backfilled_from.is_some() {
            continue;
        }
        let place = places.entry(record.bus_id).or_default();
        if place.workspace.is_none() {
            place.workspace.clone_from(&record.source.workspace);
        }
        if place.entrypoint.is_none() {
            place.entrypoint.clone_from(&record.source.entrypoint);
        }
    }
    places
}

/// A harness name as a shared row may carry it.
fn shared_harness(harness: &str) -> &str {
    if KNOWN_HARNESSES.contains(&harness) {
        harness
    } else {
        "other"
    }
}

/// Largest first; ties by name, so the same log prints the same report.
fn most_first(counts: BTreeMap<String, u64>) -> Vec<(String, u64)> {
    let mut sorted: Vec<(String, u64)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

/// The first day of a window of `days` UTC days that ends today.
pub fn first_day(now: Timestamp, days: u32) -> String {
    let back = i64::from(days.saturating_sub(1)) * DAY_SECS;
    Timestamp::from_unix_seconds(0)
        .and_then(|epoch| Timestamp::from_unix_seconds(now.seconds_since(epoch) - back))
        .map_or_else(|| day_of(now), day_of)
}

fn day_of(ts: Timestamp) -> String {
    let text = ts.to_string();
    text.get(..10).unwrap_or(&text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Body, Harness, Source};
    use crate::record::SCHEMA_VERSION;

    const ONE: &str = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e";
    const TWO: &str = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3f";

    fn source(harness: &str) -> Source {
        Source {
            harness: Harness::new(harness),
            harness_version: None,
            vendor_session_id: None,
            workspace: None,
            entrypoint: None,
            backfilled_from: None,
        }
    }

    fn at(ts: &str, bus_id: &str, source: Source, event: Event) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 1,
            ts: ts.parse().expect("ts"),
            bus_id: bus_id.parse().expect("id"),
            vendor_turn_id: None,
            vendor_agent_id: None,
            source,
            event,
        }
    }

    fn claude_in(workspace: Option<&str>, entrypoint: Option<&str>) -> Source {
        Source {
            workspace: workspace.map(str::to_string),
            entrypoint: entrypoint.map(Entrypoint::new),
            ..source(Harness::CLAUDE_CODE)
        }
    }

    fn prompt(text: &str) -> Event {
        Event::TurnStarted {
            author: None,
            prompt: Body::kept(text),
        }
    }

    fn tool(name: &str, outcome: ToolOutcome, duration_ms: Option<u64>) -> Event {
        Event::ToolUse {
            tool: name.into(),
            tool_use_id: None,
            duration_ms,
            input: Body::kept("cargo test"),
            outcome,
        }
    }

    fn ended(state: TurnState) -> Event {
        Event::TurnEnded {
            state,
            assistant_message: None,
        }
    }

    const DAY: &str = "2026-09-22";

    fn on_day(time: &str) -> String {
        format!("{DAY}T{time}.000Z")
    }

    #[test]
    fn a_conversation_is_one_conversation_however_many_records_it_writes() {
        let s = || claude_in(Some("/w"), Some("cli"));
        let ledger = Ledger::fold(
            &[
                at(&on_day("09:00:00"), ONE, s(), prompt("ship it")),
                at(
                    &on_day("09:00:01"),
                    ONE,
                    s(),
                    tool(
                        "Bash",
                        ToolOutcome::Succeeded {
                            output: Body::kept("ok"),
                        },
                        Some(40),
                    ),
                ),
                at(&on_day("09:00:02"), ONE, s(), ended(TurnState::Complete)),
            ],
            DAY,
        );

        assert_eq!(ledger.rows.len(), 1, "{:?}", ledger.rows);
        let row = &ledger.rows[0];
        assert_eq!(row.day, DAY);
        assert_eq!(row.harness, "claude_code");
        assert_eq!(row.workspace.as_deref(), Some("/w"));
        assert_eq!(row.attendance, Attendance::Attended);
        assert_eq!(row.conversations, 1);
        assert_eq!(row.counts.turns, 1);
        assert_eq!(row.counts.tool_calls, 1);
        assert_eq!(row.counts.tool_ms, 40);
        assert_eq!(ledger.records, 3);
    }

    #[test]
    fn a_conversation_stays_in_the_workspace_it_first_reported() {
        // Records written before a harness reported its `cwd` carry none, and
        // an agent that `cd`s reports a subdirectory. Neither is a second
        // conversation somewhere else.
        let ledger = Ledger::fold(
            &[
                at(&on_day("09:00:00"), ONE, claude_in(None, None), prompt("a")),
                at(
                    &on_day("09:00:01"),
                    ONE,
                    claude_in(Some("/w"), Some("cli")),
                    prompt("b"),
                ),
                at(
                    &on_day("09:00:02"),
                    ONE,
                    claude_in(Some("/w/crates"), Some("cli")),
                    prompt("c"),
                ),
            ],
            DAY,
        );

        assert_eq!(ledger.rows.len(), 1, "{:?}", ledger.rows);
        assert_eq!(ledger.rows[0].workspace.as_deref(), Some("/w"));
        assert_eq!(ledger.rows[0].attendance, Attendance::Attended);
        assert_eq!(ledger.rows[0].counts.turns, 3);
        assert_eq!(ledger.workspaces, vec![("/w".to_string(), 3)]);
    }

    #[test]
    fn attendance_is_the_entrypoint_and_an_unrecognised_one_is_unknown() {
        assert_eq!(
            Attendance::of(Some(&Entrypoint::new("claude-desktop"))),
            Attendance::Attended
        );
        assert_eq!(
            Attendance::of(Some(&Entrypoint::new("sdk-cli"))),
            Attendance::Unattended
        );
        assert_eq!(
            Attendance::of(Some(&Entrypoint::new("github-action"))),
            Attendance::Unattended
        );
        assert_eq!(
            Attendance::of(Some(&Entrypoint::new("remote"))),
            Attendance::Attended,
            "somebody is at the other end of a remote session"
        );
        assert_eq!(
            Attendance::of(Some(&Entrypoint::new("something-new"))),
            Attendance::Unknown
        );
        assert_eq!(Attendance::of(None), Attendance::Unknown);
    }

    #[test]
    fn a_stopped_turn_says_who_stopped_it() {
        let s = || claude_in(Some("/w"), Some("sdk-cli"));
        let ledger = Ledger::fold(
            &[
                at(&on_day("09:00:00"), ONE, s(), ended(TurnState::Complete)),
                at(
                    &on_day("09:00:01"),
                    ONE,
                    s(),
                    ended(TurnState::InterruptedQuota {
                        reason: "rate_limit".into(),
                    }),
                ),
                at(
                    &on_day("09:00:02"),
                    ONE,
                    s(),
                    ended(TurnState::InterruptedPolicy {
                        reason: "billing_error".into(),
                    }),
                ),
                at(
                    &on_day("09:00:03"),
                    ONE,
                    s(),
                    ended(TurnState::InterruptedUser),
                ),
            ],
            DAY,
        );

        let counts = &ledger.rows[0].counts;
        assert_eq!(counts.limit_stops, 1);
        assert_eq!(counts.account_stops, 1);
        assert_eq!(counts.user_stops, 1);
        assert_eq!(ledger.rows[0].attendance, Attendance::Unattended);
    }

    #[test]
    fn a_stop_is_filed_under_what_an_operator_would_do_about_it() {
        // The adapters split stops for the router: capacity or not. An
        // operator needs a different cut — wait, fix the account, page the
        // vendor, fix the request. On the machine that built this, the
        // "policy" bucket was 111 authentication failures over 34 hours, a
        // generic Cursor `error` and three server errors.
        let quota = |reason: &str| {
            ended(TurnState::InterruptedQuota {
                reason: reason.into(),
            })
        };
        let policy = |reason: &str| {
            ended(TurnState::InterruptedPolicy {
                reason: reason.into(),
            })
        };
        let stops = [
            quota("rate_limit"),
            quota("rate_limit: capacity"),
            quota("request failed: PRO_USER_USAGE_LIMIT"),
            quota("GoUsageLimitError"),
            quota("overloaded"),
            policy("server_error"),
            policy("authentication_failed"),
            policy("billing_error: card declined"),
            policy("account_on_hold"),
            policy("invalid_request"),
            policy("max_output_tokens"),
            policy("unknown"),
            policy("error"),
            policy("APIError"),
            ended(TurnState::InterruptedUser),
        ];
        let records: Vec<Record> = stops
            .into_iter()
            .enumerate()
            .map(|(i, event)| {
                at(
                    &on_day(&format!("09:00:{i:02}")),
                    ONE,
                    source("cursor"),
                    event,
                )
            })
            .collect();
        let counts = &Ledger::fold(&records, DAY).rows[0].counts;

        assert_eq!(counts.limit_stops, 4, "quota, whatever the vendor calls it");
        assert_eq!(counts.vendor_errors, 2, "overloaded is the vendor, not you");
        assert_eq!(counts.account_stops, 3);
        assert_eq!(counts.request_faults, 2);
        assert_eq!(counts.other_stops, 3, "unclassified, not guessed");
        assert_eq!(counts.user_stops, 1);
    }

    #[test]
    fn limit_stops_close_together_are_one_episode() {
        // 200 unattended sessions that each hit the limit in one hour are one
        // event for whoever has to deal with it, not two hundred.
        let limit = || {
            ended(TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            })
        };
        let ledger = Ledger::fold(
            &[
                at(&on_day("03:00:00"), ONE, source("claude_code"), limit()),
                at(&on_day("03:20:00"), TWO, source("claude_code"), limit()),
                at(&on_day("03:59:00"), ONE, source("claude_code"), limit()),
                at(&on_day("03:59:30"), ONE, source("codex"), limit()),
                at(&on_day("06:00:00"), TWO, source("claude_code"), limit()),
            ],
            DAY,
        );

        let claude = &ledger.harnesses[0];
        assert_eq!(claude.harness, "claude_code");
        assert_eq!(claude.counts.limit_stops, 4);
        assert_eq!(
            claude.counts.limit_episodes, 2,
            "a gap of two hours is a new one"
        );
        let codex = &ledger.harnesses[1];
        assert_eq!(codex.counts.limit_episodes, 1, "each harness has its own");
    }

    #[test]
    fn a_second_delivery_is_counted_in_its_row() {
        // The per-row count is a data-quality signal: which harness on which
        // day double-delivers.
        let failed = || ToolOutcome::Failed {
            error: Body::kept("exit 1"),
            interrupted: false,
        };
        let ledger = Ledger::fold(
            &[
                at(
                    &on_day("09:00:00"),
                    ONE,
                    source("grok"),
                    tool_with_id("c1", failed()),
                ),
                at(
                    &on_day("09:00:00"),
                    ONE,
                    source("grok"),
                    tool_with_id("c1", failed()),
                ),
            ],
            DAY,
        );

        assert_eq!(ledger.rows[0].counts.repeated, 1);
        assert_eq!(ledger.rows[0].counts.tool_calls, 1);
        assert_eq!(ledger.harnesses[0].counts.repeated, 1);
    }

    #[test]
    fn a_harness_this_build_does_not_know_is_shared_as_other() {
        // Kept by name on the machine; a sync sends only names it knows.
        let ledger = Ledger::fold(
            &[at(
                &on_day("09:00:00"),
                ONE,
                source("frobnicator"),
                prompt("a"),
            )],
            DAY,
        );

        assert_eq!(ledger.rows[0].harness, "frobnicator");
        assert_eq!(ledger.shareable()[0].harness, "other");
    }

    #[test]
    fn a_tool_a_person_stopped_is_not_a_tool_that_failed() {
        let s = || claude_in(Some("/w"), Some("cli"));
        let failed = |interrupted| ToolOutcome::Failed {
            error: Body::kept("exit 101"),
            interrupted,
        };
        let ledger = Ledger::fold(
            &[
                at(
                    &on_day("09:00:00"),
                    ONE,
                    s(),
                    tool("Bash", failed(false), Some(5)),
                ),
                at(
                    &on_day("09:00:01"),
                    ONE,
                    s(),
                    tool("Bash", failed(false), None),
                ),
                at(
                    &on_day("09:00:02"),
                    ONE,
                    s(),
                    tool("Edit", failed(false), None),
                ),
                at(
                    &on_day("09:00:03"),
                    ONE,
                    s(),
                    tool("Bash", failed(true), None),
                ),
                at(
                    &on_day("09:00:04"),
                    ONE,
                    s(),
                    Event::ToolDenied {
                        tool: "Bash".into(),
                        tool_use_id: None,
                        input: Body::kept("rm -rf /"),
                        reason: "classifier".into(),
                    },
                ),
                at(
                    &on_day("09:00:05"),
                    ONE,
                    s(),
                    Event::PermissionAsked {
                        tool: "Bash".into(),
                        input: Body::kept("git push"),
                    },
                ),
            ],
            DAY,
        );

        let counts = &ledger.rows[0].counts;
        assert_eq!(counts.tool_calls, 4, "a denied call never ran");
        assert_eq!(counts.tool_failures, 3);
        assert_eq!(counts.tool_interrupts, 1);
        assert_eq!(counts.tool_denials, 1);
        assert_eq!(counts.permission_asks, 1);
        assert_eq!(counts.tool_ms, 5);
        assert_eq!(
            ledger.failing_tools,
            vec![("Bash".to_string(), 2), ("Edit".to_string(), 1)]
        );
    }

    #[test]
    fn backfilled_history_and_records_before_the_window_are_counted_not_folded() {
        let recovered = Source {
            backfilled_from: Some("claude_transcript".into()),
            ..claude_in(Some("/w"), Some("cli"))
        };
        let ledger = Ledger::fold(
            &[
                at(&on_day("09:00:00"), ONE, recovered, prompt("old")),
                at(
                    "2026-09-21T23:59:59.999Z",
                    TWO,
                    claude_in(Some("/w"), Some("cli")),
                    prompt("yesterday"),
                ),
                at(
                    &on_day("00:00:00"),
                    TWO,
                    claude_in(Some("/w"), Some("cli")),
                    prompt("today"),
                ),
            ],
            DAY,
        );

        assert_eq!(ledger.backfilled, 1);
        assert_eq!(ledger.before_window, 1);
        assert_eq!(ledger.records, 1);
        assert_eq!(ledger.rows.len(), 1);
        assert_eq!(ledger.rows[0].counts.turns, 1);
    }

    #[test]
    fn a_conversation_before_the_window_keeps_its_workspace_inside_it() {
        // The workspace is reported early, often on the first prompt. A
        // window that opens after that must not strand the rest of the
        // conversation without a place.
        let ledger = Ledger::fold(
            &[
                at(
                    "2026-09-21T20:00:00.000Z",
                    ONE,
                    claude_in(Some("/w"), Some("cli")),
                    prompt("yesterday"),
                ),
                at(
                    &on_day("09:00:00"),
                    ONE,
                    claude_in(None, None),
                    prompt("today"),
                ),
            ],
            DAY,
        );

        assert_eq!(ledger.rows.len(), 1);
        assert_eq!(ledger.rows[0].workspace.as_deref(), Some("/w"));
        assert_eq!(ledger.rows[0].attendance, Attendance::Attended);
    }

    #[test]
    fn a_conversation_across_midnight_is_two_rows_and_one_conversation() {
        let s = || claude_in(Some("/w"), Some("cli"));
        let ledger = Ledger::fold(
            &[
                at("2026-09-22T23:59:00.000Z", ONE, s(), prompt("late")),
                at("2026-09-23T00:01:00.000Z", ONE, s(), prompt("later")),
                at(
                    "2026-09-23T00:02:00.000Z",
                    TWO,
                    Source {
                        workspace: Some("/v".into()),
                        ..source("codex")
                    },
                    prompt("other"),
                ),
            ],
            DAY,
        );

        let days: Vec<&str> = ledger.rows.iter().map(|row| row.day.as_str()).collect();
        assert_eq!(days, vec!["2026-09-22", "2026-09-23", "2026-09-23"]);
        assert!(ledger.rows.iter().all(|row| row.conversations == 1));

        let claude = &ledger.harnesses[0];
        assert_eq!(claude.harness, "claude_code");
        assert_eq!(claude.conversations, 1, "distinct over the window");
        assert_eq!(claude.attended, 1);
        assert_eq!(claude.counts.turns, 2);
        let codex = &ledger.harnesses[1];
        assert_eq!(codex.harness, "codex");
        assert_eq!(codex.unknown, 1, "only Claude Code says who is there");
    }

    #[test]
    fn a_row_says_how_many_and_never_what() {
        // The row is the shape a sync would ship. Every key is named here, so
        // a new field has to be argued for rather than serialised by default.
        let s = || claude_in(Some("/w"), Some("cli"));
        let ledger = Ledger::fold(
            &[
                at(&on_day("09:00:00"), ONE, s(), prompt("the secret plan")),
                at(
                    &on_day("09:00:01"),
                    ONE,
                    s(),
                    tool(
                        "Bash",
                        ToolOutcome::Failed {
                            error: Body::kept("password=hunter2"),
                            interrupted: false,
                        },
                        None,
                    ),
                ),
                at(
                    &on_day("09:00:02"),
                    ONE,
                    s(),
                    Event::TurnEnded {
                        state: TurnState::Complete,
                        assistant_message: Some(Body::kept("here is the plan")),
                    },
                ),
            ],
            DAY,
        );

        let json = serde_json::to_value(&ledger.rows[0]).expect("json");
        let text = json.to_string();
        for leaked in ["secret plan", "hunter2", "here is the plan", "cargo test"] {
            assert!(!text.contains(leaked), "{leaked} reached a row: {text}");
        }
        let mut keys: Vec<&str> = json
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "account_stops",
                "attendance",
                "briefings",
                "conversations",
                "day",
                "harness",
                "hops",
                "limit_episodes",
                "limit_stops",
                "other_stops",
                "permission_asks",
                "repeated",
                "request_faults",
                "subagents",
                "tool_calls",
                "tool_denials",
                "tool_failures",
                "tool_interrupts",
                "tool_ms",
                "turns",
                "user_stops",
                "vendor_errors",
                "workspace",
            ]
        );
    }

    #[test]
    fn briefings_and_hops_are_counted_where_they_were_written() {
        let s = || claude_in(Some("/w"), Some("cli"));
        let ledger = Ledger::fold(
            &[
                at(
                    &on_day("09:00:00"),
                    ONE,
                    s(),
                    Event::Briefed {
                        about: Vec::new(),
                        units: 400,
                        dropped: 0,
                    },
                ),
                at(
                    &on_day("09:00:01"),
                    ONE,
                    s(),
                    Event::Routed {
                        next: "codex".into(),
                        hop: crate::event::Hop::Displaced,
                        reason: "rate_limit".into(),
                        code_freshness: None,
                        provenance: None,
                    },
                ),
            ],
            DAY,
        );

        assert_eq!(ledger.rows[0].counts.briefings, 1);
        assert_eq!(ledger.rows[0].counts.hops, 1);
    }

    fn tool_with_id(id: &str, outcome: ToolOutcome) -> Event {
        Event::ToolUse {
            tool: "Bash".into(),
            tool_use_id: Some(crate::event::VendorToolUseId::new(id)),
            duration_ms: None,
            input: Body::kept("cargo test"),
            outcome,
        }
    }

    fn in_turn(mut record: Record, turn: &str) -> Record {
        record.vendor_turn_id = Some(crate::event::VendorTurnId::new(turn));
        record
    }

    fn by_subagent(mut record: Record, agent: &str) -> Record {
        record.vendor_agent_id = Some(crate::event::VendorAgentId::new(agent));
        record
    }

    #[test]
    fn a_tool_call_recorded_twice_is_counted_once() {
        // On the live machine every Grok tool call since 2026-09-06 reached
        // the log twice, ~330 ms apart, under one `tool_use_id`. Counting
        // records would double every Grok figure.
        let s = || source("grok");
        let failed = || ToolOutcome::Failed {
            error: Body::kept("exit 1"),
            interrupted: false,
        };
        let ledger = Ledger::fold(
            &[
                at(
                    &on_day("09:00:00"),
                    ONE,
                    s(),
                    tool_with_id("call_1", failed()),
                ),
                at(
                    &on_day("09:00:01"),
                    ONE,
                    s(),
                    tool_with_id("call_1", failed()),
                ),
            ],
            DAY,
        );

        let counts = &ledger.rows[0].counts;
        assert_eq!(counts.tool_calls, 1);
        assert_eq!(counts.tool_failures, 1);
        assert_eq!(ledger.failing_tools, vec![("Bash".to_string(), 1)]);
        assert_eq!(ledger.records, 2, "both copies are records");
        assert_eq!(ledger.repeated, 1);
    }

    #[test]
    fn a_turn_recorded_twice_is_counted_once() {
        // Cursor has been seen submitting one prompt twice, milliseconds
        // apart, and every Grok event has reached the log twice.
        let s = || source("cursor");
        let ledger = Ledger::fold(
            &[
                in_turn(at(&on_day("09:00:00"), ONE, s(), prompt("a")), "t1"),
                in_turn(at(&on_day("09:00:00"), ONE, s(), prompt("a")), "t1"),
                in_turn(
                    at(
                        &on_day("09:00:01"),
                        ONE,
                        s(),
                        ended(TurnState::InterruptedUser),
                    ),
                    "t1",
                ),
                in_turn(
                    at(
                        &on_day("09:00:01"),
                        ONE,
                        s(),
                        ended(TurnState::InterruptedUser),
                    ),
                    "t1",
                ),
                in_turn(at(&on_day("09:00:02"), ONE, s(), prompt("b")), "t2"),
            ],
            DAY,
        );

        let counts = &ledger.rows[0].counts;
        assert_eq!(counts.turns, 2);
        assert_eq!(counts.user_stops, 1);
        assert_eq!(ledger.repeated, 2);
    }

    #[test]
    fn a_turn_id_is_not_proof_of_one_turn() {
        // Claude Code's `prompt_id` is a process-global slot, and Codex keeps
        // one turn id while a person steers it: on the live machine one
        // Claude Code id carried four different prompts over five minutes.
        // A different prompt is a different submission whatever the id says.
        let s = || claude_in(Some("/w"), Some("claude-desktop"));
        let ledger = Ledger::fold(
            &[
                in_turn(at(&on_day("09:00:00"), ONE, s(), prompt("fix it")), "slot"),
                in_turn(
                    at(&on_day("09:00:00"), ONE, s(), prompt("and the docs")),
                    "slot",
                ),
            ],
            DAY,
        );

        assert_eq!(ledger.rows[0].counts.turns, 2);
        assert_eq!(ledger.repeated, 0);
    }

    #[test]
    fn the_same_prompt_again_later_is_a_new_turn() {
        // People type "continue" more than once. A second delivery arrives
        // within milliseconds; a person takes longer than the window.
        let s = || claude_in(Some("/w"), Some("cli"));
        let ledger = Ledger::fold(
            &[
                in_turn(
                    at(&on_day("09:00:00"), ONE, s(), prompt("continue")),
                    "slot",
                ),
                in_turn(
                    at(&on_day("09:01:00"), ONE, s(), prompt("continue")),
                    "slot",
                ),
            ],
            DAY,
        );

        assert_eq!(ledger.rows[0].counts.turns, 2);
        assert_eq!(ledger.repeated, 0);
    }

    #[test]
    fn a_subagent_finishing_is_not_a_turn() {
        // `SubagentStop` is a `turn_ended` under the subagent's id. On the
        // live machine 266 of 561 Claude Code turn ends in a week were these,
        // which made "completed" nearly twice the number of prompts.
        let s = || claude_in(Some("/w"), Some("cli"));
        let ledger = Ledger::fold(
            &[
                at(&on_day("09:00:00"), ONE, s(), prompt("go")),
                by_subagent(
                    at(&on_day("09:00:01"), ONE, s(), ended(TurnState::Complete)),
                    "agent-1",
                ),
                by_subagent(
                    at(&on_day("09:00:02"), ONE, s(), ended(TurnState::Complete)),
                    "agent-2",
                ),
                at(&on_day("09:00:03"), ONE, s(), ended(TurnState::Complete)),
            ],
            DAY,
        );

        let counts = &ledger.rows[0].counts;
        assert_eq!(counts.turns, 1);
        assert_eq!(counts.subagents, 2);
    }

    #[test]
    fn records_without_an_id_cannot_be_told_apart_and_are_all_counted() {
        let s = || claude_in(Some("/w"), Some("cli"));
        let ok = || ToolOutcome::Succeeded {
            output: Body::kept("ok"),
        };
        let ledger = Ledger::fold(
            &[
                at(&on_day("09:00:00"), ONE, s(), tool("Read", ok(), None)),
                at(&on_day("09:00:00"), ONE, s(), tool("Read", ok(), None)),
            ],
            DAY,
        );

        assert_eq!(ledger.rows[0].counts.tool_calls, 2);
        assert_eq!(ledger.repeated, 0);
    }

    #[test]
    fn a_shareable_row_has_no_folder_and_folders_merge() {
        // RFD 0002 with workspaces off: the field is absent, not empty, and two
        // conversations that differed only by folder are one row. Each
        // conversation belongs to one folder, so their counts still add up.
        let ledger = Ledger::fold(
            &[
                at(
                    &on_day("09:00:00"),
                    ONE,
                    claude_in(Some("/Users/someone/secret-project"), Some("cli")),
                    prompt("a"),
                ),
                at(
                    &on_day("09:00:01"),
                    TWO,
                    claude_in(Some("/w"), Some("cli")),
                    prompt("b"),
                ),
                at(
                    &on_day("09:00:02"),
                    TWO,
                    claude_in(Some("/w"), Some("sdk-cli")),
                    prompt("c"),
                ),
            ],
            DAY,
        );

        let shared = ledger.shareable();
        assert_eq!(ledger.rows.len(), 2, "two folders locally");
        assert_eq!(shared.len(), 1, "{shared:?}");
        assert_eq!(shared[0].conversations, 2);
        assert_eq!(shared[0].counts.turns, 3);
        assert_eq!(shared[0].attendance, Attendance::Attended);
        let text = serde_json::to_string(&shared[0]).expect("json");
        assert!(!text.contains("workspace"), "{text}");
        assert!(!text.contains("secret-project"), "{text}");
    }

    #[test]
    fn a_shareable_row_names_exactly_what_the_contract_names() {
        // docs/rfd/0002-metadata-sync.md, "The unit: a row", with workspaces
        // off. A new key here is an amendment to that RFD first.
        let s = || claude_in(Some("/w"), Some("cli"));
        let ledger = Ledger::fold(
            &[
                at(&on_day("09:00:00"), ONE, s(), prompt("the secret plan")),
                at(
                    &on_day("09:00:01"),
                    ONE,
                    s(),
                    tool(
                        "Bash",
                        ToolOutcome::Failed {
                            error: Body::kept("password=hunter2"),
                            interrupted: false,
                        },
                        None,
                    ),
                ),
            ],
            DAY,
        );

        let json = serde_json::to_value(&ledger.shareable()[0]).expect("json");
        let text = json.to_string();
        for leaked in ["secret plan", "hunter2", "cargo test", "Bash", "/w"] {
            assert!(
                !text.contains(leaked),
                "{leaked} reached a shared row: {text}"
            );
        }
        let mut keys: Vec<&str> = json
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "account_stops",
                "attendance",
                "briefings",
                "conversations",
                "day",
                "harness",
                "hops",
                "limit_episodes",
                "limit_stops",
                "other_stops",
                "permission_asks",
                "repeated",
                "request_faults",
                "subagents",
                "tool_calls",
                "tool_denials",
                "tool_failures",
                "tool_interrupts",
                "tool_ms",
                "turns",
                "user_stops",
                "vendor_errors",
            ]
        );
    }

    #[test]
    fn a_window_of_seven_days_ends_today() {
        let now: Timestamp = "2026-09-23T10:00:00.000Z".parse().expect("ts");
        assert_eq!(first_day(now, 7), "2026-09-17");
        assert_eq!(first_day(now, 1), "2026-09-23");
        assert_eq!(first_day(now, 0), "2026-09-23", "zero days is today");
    }
}
