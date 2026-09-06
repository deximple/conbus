//! Workspace facts, derived from the log and confirmed here.
//!
//! A busy agent does not write these. The hook does not propose them. A person
//! (or a later auto-accept policy) confirms a claim that already has a source
//! turn. Linked workspaces see the claim on their next first prompt.

use std::fs;
use std::io::{self, Write};

use conbus_core::clock::Timestamp;
use conbus_core::ids::BusId;
use conbus_core::paths::Paths;
use serde::{Deserialize, Serialize};

const TYPES: &[&str] = &["decision", "constraint", "state", "pointer", "open"];

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub workspace: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub claim: String,
    pub lifecycle: String,
    pub anchors: Vec<String>,
    pub bus_id: String,
    pub seq: u64,
    pub verified_at: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Link {
    pub from: String,
    pub to: String,
}

fn canon(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.trim_end_matches('/').to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn accept(
    paths: &Paths,
    workspace: &str,
    kind: &str,
    claim: &str,
    anchor: &str,
    lifecycle: &str,
    bus_id: &str,
    seq: u64,
) -> io::Result<Fact> {
    if !TYPES.contains(&kind) {
        return Err(io::Error::other(format!(
            "unknown fact type {kind:?}; expected one of {}",
            TYPES.join(", ")
        )));
    }
    if claim.trim().is_empty() {
        return Err(io::Error::other("a fact needs a claim"));
    }
    if kind != "open" && anchor.trim().is_empty() {
        return Err(io::Error::other("a fact that is not open needs an anchor"));
    }
    let now = Timestamp::now().to_string();
    let fact = Fact {
        id: BusId::mint().to_string(),
        workspace: canon(workspace),
        kind: kind.into(),
        claim: claim.trim().into(),
        lifecycle: lifecycle.into(),
        anchors: if anchor.trim().is_empty() {
            Vec::new()
        } else {
            vec![anchor.trim().into()]
        },
        bus_id: bus_id.into(),
        seq,
        verified_at: now,
    };
    if let Some(parent) = paths.facts().parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.facts())?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&fact).map_err(io::Error::other)?
    )?;
    Ok(fact)
}

pub fn link(paths: &Paths, from: &str, to: &str) -> io::Result<Link> {
    let link = Link {
        from: canon(from),
        to: canon(to),
    };
    if link.from == link.to {
        return Err(io::Error::other("a workspace cannot link to itself"));
    }
    if let Some(parent) = paths.links().parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.links())?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&link).map_err(io::Error::other)?
    )?;
    Ok(link)
}

/// Facts a session in `workspace` should see: its own, plus those of workspaces
/// linked *to* it.
pub fn visible(paths: &Paths, workspace: Option<&str>) -> io::Result<Vec<Fact>> {
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let here = canon(workspace);
    let links = read_links(paths)?;
    let sources: Vec<String> = std::iter::once(here.clone())
        .chain(links.into_iter().filter(|l| l.to == here).map(|l| l.from))
        .collect();
    let facts = read_facts(paths)?;
    Ok(facts
        .into_iter()
        .filter(|f| sources.iter().any(|s| s == &f.workspace))
        .collect())
}

pub fn render(facts: &[Fact]) -> Vec<String> {
    facts
        .iter()
        .take(3)
        .map(|fact| {
            let anchor = fact.anchors.first().map(String::as_str).unwrap_or("—");
            format!(
                "\n  arena         {} · {}\n  claim         {}\n  anchor        {}\n",
                fact.kind, fact.workspace, fact.claim, anchor
            )
        })
        .collect()
}

fn read_facts(paths: &Paths) -> io::Result<Vec<Fact>> {
    read_jsonl(&paths.facts())
}

fn read_links(paths: &Paths) -> io::Result<Vec<Link>> {
    read_jsonl(&paths.links())
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &std::path::Path) -> io::Result<Vec<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(item) = serde_json::from_str(line) {
            out.push(item);
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    paths: &Paths,
    action: &str,
    workspace: Option<String>,
    kind: Option<String>,
    claim: Option<String>,
    anchor: Option<String>,
    from: Option<String>,
    to: Option<String>,
    bus_id: Option<String>,
    seq: Option<u64>,
) -> io::Result<()> {
    match action {
        "fact" => {
            let fact = accept(
                paths,
                workspace
                    .as_deref()
                    .ok_or_else(|| io::Error::other("--workspace is required"))?,
                kind.as_deref().unwrap_or("decision"),
                claim
                    .as_deref()
                    .ok_or_else(|| io::Error::other("--claim is required"))?,
                anchor.as_deref().unwrap_or(""),
                "until_task",
                bus_id.as_deref().unwrap_or(""),
                seq.unwrap_or(0),
            )?;
            println!("accepted  {} · {}", fact.id, fact.claim);
        }
        "link" => {
            let link = link(
                paths,
                from.as_deref()
                    .ok_or_else(|| io::Error::other("--from is required"))?,
                to.as_deref()
                    .ok_or_else(|| io::Error::other("--to is required"))?,
            )?;
            println!("linked    {} → {}", link.from, link.to);
        }
        other => return Err(io::Error::other(format!("unknown arena action {other:?}"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::paths::Paths;

    #[test]
    fn a_linked_workspace_sees_the_other_side_s_fact() {
        let root = tempfile::tempdir().expect("tempdir");
        let a = root.path().join("a");
        let b = root.path().join("b");
        fs::create_dir_all(&a).expect("a");
        fs::create_dir_all(&b).expect("b");
        let paths =
            Paths::from_env(root.path().join("conbus").to_str(), None, None).expect("paths");

        accept(
            &paths,
            a.to_str().unwrap(),
            "decision",
            "bus_id is minted on the first prompt",
            "crates/conbus-core/src/ids.rs",
            "permanent",
            "01a0611a-cfef-75b2-9c50-8a485ed71792",
            1,
        )
        .expect("accept");
        link(&paths, a.to_str().unwrap(), b.to_str().unwrap()).expect("link");

        let seen = visible(&paths, b.to_str()).expect("visible");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].claim, "bus_id is minted on the first prompt");
        assert!(visible(&paths, a.to_str()).expect("own").len() == 1);
    }
}
