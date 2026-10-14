//! The local PreToolUse policy: `policy.json` beside the log.
//!
//! An operator's rules that turn a matching `PreToolUse` payload into a deny
//! or ask reply, in the vendor's own hook dialect. Evaluation happens in the
//! hook process from the payload alone, before the daemon is ever asked, so
//! a down or slow recorder cannot lift a deny — the reply is decided either
//! way and the record is still sent exactly as today.
//!
//! The policy only tightens. There is no `allow` and there never will be: a
//! vendor prompt the file cannot lift stays the person's own decision, and a
//! file that does not parse decides nothing rather than guessing.

use std::io;
use std::path::Path;

use conbus_core::event::Harness;
use conbus_core::paths::Paths;
use serde::Deserialize;

/// What a rule can decide. `ask` keeps the vendor's own prompt in front of
/// the person; `deny` refuses the call outright. There is deliberately no
/// `allow` — policy can only narrow what runs, never widen it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Deny,
    Ask,
}

impl Decision {
    fn as_str(self) -> &'static str {
        match self {
            Decision::Deny => "deny",
            Decision::Ask => "ask",
        }
    }
}

/// The harnesses whose `PreToolUse` contract takes a decision reply.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PolicyHarness {
    ClaudeCode,
    Codex,
}

impl PolicyHarness {
    fn as_str(self) -> &'static str {
        match self {
            PolicyHarness::ClaudeCode => "claude_code",
            PolicyHarness::Codex => "codex",
        }
    }
}

/// One rule, exactly as the file holds it. `deny_unknown_fields` is the
/// contract: a misspelled key must read as an invalid file, not as a rule
/// that silently matches everything it was never asked to match.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    /// Which harness the rule applies to. Absent means every harness that
    /// can take a decision at all.
    harness: Option<PolicyHarness>,
    /// The payload's `tool_name`, or `"*"` for every tool.
    tool: String,
    /// Any one of these substrings inside the compact JSON of `tool_input`
    /// matches. Absent matches every input.
    input_contains: Option<Vec<String>>,
    decision: Decision,
    /// Shown to the person verbatim, so an empty one is a broken rule.
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    rules: Vec<Rule>,
}

/// A policy file and whether it can be trusted to decide.
#[derive(Debug)]
pub struct Policy {
    rules: Vec<Rule>,
}

/// What the file on disk turned out to be. `Absent` and `Invalid` decide
/// identically — nothing — but they are different facts and `doctor` and
/// `policy check` say which.
pub enum Load {
    /// No `policy.json` — policy is off, every call passes through.
    Absent,
    /// A file exists and it cannot be a policy: unparseable, wrong version,
    /// unknown field. Fail-open at runtime, loud in `doctor`.
    Invalid(String),
    Ready(Policy),
}

pub fn load(path: &Path) -> Load {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Load::Absent,
        Err(err) => return Load::Invalid(format!("cannot read {}: {err}", path.display())),
    };
    match parse(&text) {
        Ok(policy) => Load::Ready(policy),
        Err(err) => Load::Invalid(format!("invalid {}: {err}", path.display())),
    }
}

fn parse(text: &str) -> Result<Policy, String> {
    let file: File = serde_json::from_str(text).map_err(|err| err.to_string())?;
    if file.version != 1 {
        return Err(format!(
            "unsupported version {}; this build reads version 1",
            file.version
        ));
    }
    for (i, rule) in file.rules.iter().enumerate() {
        if rule.reason.trim().is_empty() {
            return Err(format!("rule {} has an empty reason", i + 1));
        }
    }
    Ok(Policy { rules: file.rules })
}

impl Policy {
    /// How many rules the file holds — for `doctor`, which reports the count
    /// rather than the rules themselves.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// The first matching rule of the winning decision: any matching deny
    /// beats every matching ask, and among equals the file's order stands.
    fn decide(&self, harness: &str, tool: &str, tool_input: &serde_json::Value) -> Option<&Rule> {
        let input = serde_json::to_string(tool_input).unwrap_or_else(|_| "null".to_string());
        let mut ask: Option<&Rule> = None;
        for rule in &self.rules {
            if rule.harness.is_some_and(|h| h.as_str() != harness) {
                continue;
            }
            if rule.tool != "*" && rule.tool != tool {
                continue;
            }
            if let Some(needles) = &rule.input_contains
                && !needles.iter().any(|needle| input.contains(needle))
            {
                continue;
            }
            match rule.decision {
                Decision::Deny => return Some(rule),
                Decision::Ask if ask.is_none() => ask = Some(rule),
                Decision::Ask => {}
            }
        }
        ask
    }
}

/// The reply a decision prints on stdout: the `PreToolUse` decision object in
/// the dialect both vendors parse. Never `continue`/`suppressOutput` — Codex
/// fails a PreToolUse reply that carries them (see `silent_reply`), and the
/// decision must survive every vendor's parser.
fn decision_reply(decision: Decision, reason: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision.as_str(),
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

/// The rule a harness actually hears. Codex's `PreToolUse` parser rejects
/// `permissionDecision: ask` outright — the reply fails the hook rather than
/// asking — so for Codex an `ask` rule is no decision at all and the
/// vendor's own prompt still stands in front of the call.
fn effective<'a>(harness: &str, rule: &'a Rule) -> Option<&'a Rule> {
    if harness == Harness::CODEX && rule.decision == Decision::Ask {
        return None;
    }
    Some(rule)
}

/// What this payload earns from the policy, as the exact reply to print.
///
/// `harness` is who invoked the hook, because the reply goes back to that
/// vendor and must be in its dialect. Payloads identified as Cursor's or as
/// an Inference frame are never decided — their reply contract is a
/// different one, whatever the fields happen to name.
pub fn decide(paths: Option<&Paths>, harness: &str, payload: &str) -> Option<String> {
    if !matches!(harness, Harness::CLAUDE_CODE | Harness::CODEX) {
        return None;
    }
    if crate::hook::cursor::looks_like(payload) || crate::hook::inference::looks_like(payload) {
        return None;
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(payload) else {
        return None;
    };
    if payload.get("hook_event_name").and_then(|v| v.as_str()) != Some("PreToolUse") {
        return None;
    }
    let tool = payload
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let input = payload
        .get("tool_input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let Load::Ready(policy) = load(&paths?.policy()) else {
        return None;
    };
    policy
        .decide(harness, tool, &input)
        .and_then(|rule| effective(harness, rule))
        .map(|rule| decision_reply(rule.decision, &rule.reason))
}

/// `conbus policy check`: the effective rules, or the reason there are none.
pub fn check(paths: &Paths) -> io::Result<()> {
    match load(&paths.policy()) {
        Load::Absent => println!(
            "policy    off — no {} (policy only ever tightens; absent means pass-through)",
            paths.policy().display()
        ),
        Load::Invalid(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
        Load::Ready(policy) => {
            println!(
                "policy    {} rule(s) in {}",
                policy.rules.len(),
                paths.policy().display()
            );
            for (i, rule) in policy.rules.iter().enumerate() {
                println!(
                    "          {}. {} {} {} → {} — {}",
                    i + 1,
                    rule.harness
                        .map_or("*".to_string(), |h| h.as_str().to_string()),
                    rule.tool,
                    rule.input_contains
                        .as_ref()
                        .map(|needles| format!("input_contains {needles:?}"))
                        .unwrap_or_else(|| "(any input)".to_string()),
                    rule.decision.as_str(),
                    rule.reason,
                );
            }
        }
    }
    Ok(())
}

/// `conbus policy test`: what the file would decide for one call.
pub fn test(paths: &Paths, harness: &str, tool: &str, input: &str) -> io::Result<()> {
    if harness.is_empty() || tool.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "policy test needs --harness and --tool",
        ));
    }
    let input: serde_json::Value = if input.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(input).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("--input is not JSON: {err}"),
            )
        })?
    };
    match load(&paths.policy()) {
        Load::Absent => println!("none"),
        Load::Invalid(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
        Load::Ready(policy) => match policy
            .decide(harness, tool, &input)
            .and_then(|rule| effective(harness, rule))
        {
            Some(rule) => println!("{}: {}", rule.decision.as_str(), rule.reason),
            None => println!("none"),
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(json: serde_json::Value) -> Policy {
        match parse(&json.to_string()) {
            Ok(policy) => policy,
            Err(err) => panic!("policy must parse: {err}"),
        }
    }

    fn rules(rules: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"version": 1, "rules": rules})
    }

    fn apply(json: serde_json::Value, tool: &str, input: serde_json::Value) -> Option<String> {
        policy(json)
            .decide("claude_code", tool, &input)
            .map(|rule| format!("{}: {}", rule.decision.as_str(), rule.reason))
    }

    #[test]
    fn a_valid_file_parses_and_decides() {
        let decided = apply(
            rules(serde_json::json!([{
                "tool": "Bash",
                "input_contains": ["rm -rf /"],
                "decision": "deny",
                "reason": "no wiping the disk"
            }])),
            "Bash",
            serde_json::json!({"command": "rm -rf /"}),
        );
        assert_eq!(decided.as_deref(), Some("deny: no wiping the disk"));
    }

    #[test]
    fn an_unknown_field_invalidates_the_file() {
        // A misspelled key is a broken file, not a silently different rule.
        let err = parse(
            &rules(serde_json::json!([{
                "tool": "Bash",
                "decision": "deny",
                "reason": "x",
                "tool_name": "Bash"
            }]))
            .to_string(),
        )
        .expect_err("unknown field must fail");
        assert!(err.contains("tool_name"), "{err}");
    }

    #[test]
    fn the_wrong_version_invalidates_the_file() {
        let err = parse(&serde_json::json!({"version": 2, "rules": []}).to_string())
            .expect_err("version 2 must fail");
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn an_empty_reason_invalidates_the_file() {
        let err = parse(
            &rules(serde_json::json!([{
                "tool": "Bash",
                "decision": "deny",
                "reason": "   "
            }]))
            .to_string(),
        )
        .expect_err("empty reason must fail");
        assert!(err.contains("reason"), "{err}");
    }

    #[test]
    fn allow_is_not_a_decision_this_file_can_hold() {
        // Policy tightens; a rule that lifts a vendor prompt would invert it.
        parse(
            &rules(serde_json::json!([{
                "tool": "Bash",
                "decision": "allow",
                "reason": "x"
            }]))
            .to_string(),
        )
        .expect_err("allow must fail");
    }

    #[test]
    fn a_wildcard_tool_matches_every_tool() {
        let decided = apply(
            rules(serde_json::json!([{
                "tool": "*",
                "decision": "deny",
                "reason": "nothing runs"
            }])),
            "Edit",
            serde_json::json!({}),
        );
        assert_eq!(decided.as_deref(), Some("deny: nothing runs"));
    }

    #[test]
    fn input_contains_matches_any_substring_of_the_compact_input() {
        let decided = apply(
            rules(serde_json::json!([{
                "tool": "Bash",
                "input_contains": ["never-this", "rm -rf"],
                "decision": "deny",
                "reason": "matched the second needle"
            }])),
            "Bash",
            serde_json::json!({"command": "sudo rm -rf /", "timeout": 10}),
        );
        assert_eq!(decided.as_deref(), Some("deny: matched the second needle"));
    }

    #[test]
    fn a_rule_without_input_contains_matches_every_input() {
        let decided = apply(
            rules(serde_json::json!([{
                "tool": "Bash",
                "decision": "deny",
                "reason": "all Bash"
            }])),
            "Bash",
            serde_json::json!({"command": "ls"}),
        );
        assert_eq!(decided.as_deref(), Some("deny: all Bash"));
    }

    #[test]
    fn a_harness_rule_leaves_other_harnesses_alone() {
        let file = rules(serde_json::json!([{
            "harness": "codex",
            "tool": "Bash",
            "decision": "deny",
            "reason": "codex only"
        }]));
        assert_eq!(
            apply(file.clone(), "Bash", serde_json::json!({})),
            None,
            "claude_code must not see a codex rule"
        );
        let decided = policy(file)
            .decide("codex", "Bash", &serde_json::json!({}))
            .map(|rule| rule.decision.as_str());
        assert_eq!(decided, Some("deny"));
    }

    #[test]
    fn deny_beats_ask_and_the_first_matching_reason_wins() {
        let decided = apply(
            rules(serde_json::json!([
                {"tool": "Bash", "decision": "ask", "reason": "first ask"},
                {"tool": "Bash", "decision": "deny", "reason": "first deny"},
                {"tool": "Bash", "decision": "deny", "reason": "second deny"},
                {"tool": "Bash", "decision": "ask", "reason": "second ask"},
            ])),
            "Bash",
            serde_json::json!({}),
        );
        assert_eq!(decided.as_deref(), Some("deny: first deny"));

        let decided = apply(
            rules(serde_json::json!([
                {"tool": "Bash", "decision": "ask", "reason": "first ask"},
                {"tool": "Bash", "decision": "ask", "reason": "second ask"},
            ])),
            "Bash",
            serde_json::json!({}),
        );
        assert_eq!(decided.as_deref(), Some("ask: first ask"));
    }

    #[test]
    fn nothing_matching_means_no_decision() {
        let decided = apply(
            rules(serde_json::json!([{
                "tool": "Bash",
                "decision": "deny",
                "reason": "x"
            }])),
            "Read",
            serde_json::json!({}),
        );
        assert_eq!(decided, None);
    }

    #[test]
    fn a_missing_file_is_absent_not_invalid() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            load(&dir.path().join("policy.json")),
            Load::Absent
        ));
    }

    #[test]
    fn the_decision_reply_is_exactly_the_vendor_object() {
        // The two vendors parse this shape; adding a control field would fail
        // Codex, removing one would fail the match the reply depends on.
        let reply = decision_reply(Decision::Deny, "no");
        assert_eq!(
            reply,
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"no"}}"#
        );
    }

    #[test]
    fn decide_skips_everything_but_a_decision_harness_pretooluse() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("policy.json"),
            rules(serde_json::json!([{
                "tool": "*",
                "decision": "deny",
                "reason": "everything"
            }]))
            .to_string(),
        )
        .expect("write");
        let paths =
            Paths::from_env(Some(dir.path().to_str().expect("utf-8")), None, None).expect("paths");

        let pretool = r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{}}"#;
        assert!(decide(Some(&paths), "claude_code", pretool).is_some());
        // A different event, a different harness, and an unreadable payload
        // are all the same answer: nothing.
        assert!(
            decide(
                Some(&paths),
                "claude_code",
                r#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Bash"}"#
            )
            .is_none()
        );
        assert!(decide(Some(&paths), "grok", pretool).is_none());
        assert!(decide(Some(&paths), "claude_code", "{ not json").is_none());
        // Cursor's payload shapes never reach a decision even under a
        // claude_code registration.
        assert!(decide(
            Some(&paths),
            "claude_code",
            r#"{"conversation_id":"c","cursor_version":"3.19.13","hook_event_name":"PreToolUse","tool_name":"Bash"}"#
        )
        .is_none());
    }

    #[test]
    fn codex_never_hears_an_ask() {
        // Codex fails a PreToolUse reply that says `ask`; for it an ask rule
        // is no decision — the vendor's own prompt still stands in front.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("policy.json"),
            rules(serde_json::json!([
                {"tool": "Bash", "decision": "ask", "reason": "check first"},
                {"tool": "Read", "decision": "deny", "reason": "no reads"}
            ]))
            .to_string(),
        )
        .expect("write");
        let paths =
            Paths::from_env(Some(dir.path().to_str().expect("utf-8")), None, None).expect("paths");

        let ask_payload = r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{}}"#;
        assert!(decide(Some(&paths), "codex", ask_payload).is_none());
        assert!(
            decide(Some(&paths), "claude_code", ask_payload)
                .expect("ask must reach claude")
                .contains(r#""permissionDecision":"ask""#)
        );
        // A deny still reaches Codex even when an ask matched first.
        let deny_payload = r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{}}"#;
        assert!(
            decide(Some(&paths), "codex", deny_payload)
                .expect("deny must reach codex")
                .contains(r#""permissionDecision":"deny""#)
        );
    }
}
