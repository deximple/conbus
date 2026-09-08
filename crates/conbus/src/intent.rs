//! Chat orders: resume, handoff, transfer.
//!
//! Only a prompt that *is* the order counts. A sentence that mentions a
//! harness while doing other work is not a hop.

use conbus_core::attach;
use conbus_core::event::Harness;

/// What the person asked the bus to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Order {
    pub kind: Kind,
    pub to: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Resume,
    Handoff,
    Transfer,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Resume => "resume",
            Kind::Handoff => "handoff",
            Kind::Transfer => "transfer",
        }
    }
}

/// Names people type. Longest first so "claude code" wins over "claude".
const NAMES: &[(&str, &str)] = &[
    ("claude code", Harness::CLAUDE_CODE),
    ("claude_code", Harness::CLAUDE_CODE),
    ("claude-code", Harness::CLAUDE_CODE),
    ("클로드 코드", Harness::CLAUDE_CODE),
    ("antigravity", Harness::AGY),
    ("open code", Harness::OPENCODE),
    ("opencode", Harness::OPENCODE),
    ("오픈코드", Harness::OPENCODE),
    ("claude", Harness::CLAUDE_CODE),
    ("클로드", Harness::CLAUDE_CODE),
    ("codex", Harness::CODEX),
    ("코덱스", Harness::CODEX),
    ("cursor", Harness::CURSOR),
    ("커서", Harness::CURSOR),
    ("grok", Harness::GROK),
    ("그록", Harness::GROK),
    ("chat", Harness::CLAUDE_CHAT),
    ("챗", Harness::CLAUDE_CHAT),
    ("agy", Harness::AGY),
];

const FRAMES: &[(&str, Kind)] = &[
    ("이제 {dest}에서 작업할거야", Kind::Handoff),
    ("이제 {dest}에서 작업할게", Kind::Handoff),
    ("이제 {dest}에서 작업할께", Kind::Handoff),
    ("이제 {dest}에서 작업할거예요", Kind::Handoff),
    ("이제 {dest}로 작업할거야", Kind::Handoff),
    ("이제 {dest}로 작업할게", Kind::Handoff),
    ("이제 {dest}에서 할거야", Kind::Handoff),
    ("이제 {dest}로 할거야", Kind::Handoff),
    ("이제 {dest}에서 할께", Kind::Handoff),
    ("이제 {dest}로", Kind::Handoff),
    ("{dest}에서 작업할거야", Kind::Handoff),
    ("{dest}로 작업할거야", Kind::Handoff),
    ("{dest}로 넘겨", Kind::Handoff),
    ("{dest}로 넘어가", Kind::Handoff),
    ("{dest}로 핸드오프", Kind::Handoff),
    ("{dest}로 handoff", Kind::Handoff),
    ("{dest}로 이관", Kind::Transfer),
    ("{dest}로 재개", Kind::Resume),
    ("{dest}에서 재개", Kind::Resume),
    ("switch to {dest}", Kind::Handoff),
    ("hop to {dest}", Kind::Handoff),
    ("hand off to {dest}", Kind::Handoff),
    ("handoff to {dest}", Kind::Handoff),
    ("transfer to {dest}", Kind::Transfer),
    ("resume {dest}", Kind::Resume),
    ("continue on {dest}", Kind::Resume),
    ("continue in {dest}", Kind::Resume),
    ("go to {dest}", Kind::Handoff),
    ("open {dest}", Kind::Handoff),
];

/// Parse a user prompt. `None` unless the whole line is an order.
pub fn parse(text: &str) -> Option<Order> {
    let line = normalize(text)?;
    let (raw, slug) = dest_in(&line)?;
    let frame = line.replacen(raw, "{dest}", 1);
    let kind = FRAMES
        .iter()
        .find_map(|(pat, kind)| (*pat == frame).then_some(*kind))?;
    Some(Order {
        kind,
        to: slug.to_string(),
    })
}

fn normalize(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 80 {
        return None;
    }
    if trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
        > 1
    {
        return None;
    }
    let mut out = String::new();
    let mut gap = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push(' ');
        }
        gap = false;
        out.push(ch);
    }
    while out.ends_with(['.', '!', '?', '。', '！', '？']) {
        out.pop();
    }
    if out.is_empty() {
        return None;
    }
    Some(out.to_ascii_lowercase())
}

fn dest_in(line: &str) -> Option<(&str, &'static str)> {
    NAMES.iter().find_map(|(name, slug)| {
        line.find(name)
            .map(|at| (&line[at..at + name.len()], *slug))
            .filter(|_| attach::resolve(slug) == Some(*slug))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_korean_handoff_names_the_catalog_leg() {
        let order = parse("이제 커서로 작업할거야").expect("order");
        assert_eq!(order.to, Harness::CURSOR);
        assert_eq!(order.kind, Kind::Handoff);
        let order = parse("이제 클로드에서 작업할거야.").expect("order");
        assert_eq!(order.to, Harness::CLAUDE_CODE);
        let order = parse("코덱스로 넘겨").expect("order");
        assert_eq!(order.to, Harness::CODEX);
    }

    #[test]
    fn resume_and_transfer_are_distinct_kinds() {
        assert_eq!(parse("커서로 재개").expect("r").kind, Kind::Resume);
        assert_eq!(parse("opencode로 이관").expect("t").kind, Kind::Transfer);
        assert_eq!(parse("resume cursor").expect("en").to, Harness::CURSOR);
        assert_eq!(parse("switch to grok").expect("en").kind, Kind::Handoff);
    }

    #[test]
    fn work_that_mentions_a_harness_is_not_an_order() {
        assert!(parse("hand this off").is_none());
        assert!(parse("keep going").is_none());
        assert!(parse("cursor hooks.json을 고친다").is_none());
        assert!(parse("이제 커서로 작업할거야. 그리고 테스트도 돌려").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn an_unknown_name_is_not_a_catalog_leg() {
        assert!(parse("이제 윈드서프에서 작업할거야").is_none());
        assert!(parse("switch to notepad").is_none());
    }
}
