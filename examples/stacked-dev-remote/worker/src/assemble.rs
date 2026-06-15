//! The `assemble_wave` handler (BD-006), mirroring
//! `../../src/stacked_dev/assemble.gleam` decision for decision — same ledger
//! reads, same resolution, same ordering, same whole-wave refusals with the
//! same message content — so a deployed run and a local run never diverge. A
//! refusal or can't-execute condition is a terminal [`ActivityFailure`] (CN5);
//! the coverage and landed-dependency checks are native, never shelling out to
//! check-coverage.py or validate.py.

use std::collections::BTreeSet;

use aion_worker::ActivityFailure;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::types::{
    AssembleInput, AssembledWave, BriefDocument, ExecutionStatus, ResolvedAdr, ResolvedContext,
    ResolvedItem, ResolvedProvenance, WaveEntry,
};

/// One roadmap item reduced to its provenance projection.
#[derive(Deserialize)]
struct RoadmapItem {
    links: RoadmapLinks,
    provenance: RoadmapProvenance,
}

#[derive(Deserialize)]
struct RoadmapLinks {
    cluster: String,
}

#[derive(Deserialize)]
struct RoadmapProvenance {
    requested_by: String,
    quote: String,
}

#[derive(Deserialize)]
struct DecisionsFile {
    decisions: Vec<ResolvedAdr>,
}

#[derive(Deserialize)]
struct RoadmapFile {
    items: Vec<RoadmapItem>,
}

#[derive(Deserialize)]
struct ChecklistFile {
    sections: Vec<ChecklistSection>,
}

#[derive(Deserialize)]
struct ChecklistSection {
    items: Vec<ResolvedItem>,
}

#[derive(Deserialize)]
struct StoriesFile {
    personas: Vec<StoriesPersona>,
}

#[derive(Deserialize)]
struct StoriesPersona {
    stories: Vec<ResolvedItem>,
}

#[derive(Deserialize)]
struct DesignFile {
    intention: String,
    constraints: Vec<ResolvedItem>,
}

/// One located, decoded brief with its cluster's resolved documents.
struct Loaded {
    id: String,
    cluster: String,
    document: BriefDocument,
    checklist_items: Vec<ResolvedItem>,
    story_items: Vec<ResolvedItem>,
    intention: String,
    constraints: Vec<ResolvedItem>,
}

/// The on-disk landed state of an out-of-wave `depends_on` brief.
enum DepStatus {
    Landed,
    NotLanded,
    NotFound,
}

/// `assemble_wave`: resolve, order, and refuse a wave, mirroring
/// `assemble.run`. A refusal or can't-execute condition is a terminal
/// activity failure (CN5).
///
/// # Errors
///
/// Terminal [`ActivityFailure`] when a ledger or brief cannot be read or
/// decoded, or when the wave is dependency-blocked, coverage-broken, or
/// cyclic.
pub fn assemble_wave(input: AssembleInput) -> Result<AssembledWave, ActivityFailure> {
    run(input).map_err(ActivityFailure::terminal)
}

fn run(input: AssembleInput) -> Result<AssembledWave, String> {
    let AssembleInput { design_dir, wave } = input;
    let decisions: Vec<ResolvedAdr> =
        read_json::<DecisionsFile>(&format!("{design_dir}/decisions.json"))?.decisions;
    let roadmap: Vec<RoadmapItem> =
        read_json::<RoadmapFile>(&format!("{design_dir}/roadmap.json"))?.items;

    let mut loaded = Vec::with_capacity(wave.len());
    for id in &wave {
        loaded.push(load_one(&design_dir, id)?);
    }
    let wave_ids: Vec<String> = loaded.iter().map(|entry| entry.id.clone()).collect();

    let mut reasons: Vec<String> = Vec::new();
    for entry in &loaded {
        reasons.extend(brief_reasons(
            &design_dir,
            entry,
            &decisions,
            &roadmap,
            &wave_ids,
        ));
    }

    match order(&loaded, &wave_ids) {
        Ok(ordered) => {
            if reasons.is_empty() {
                let entries = ordered
                    .iter()
                    .map(|entry| build_entry(entry, &decisions, &roadmap, &design_dir))
                    .collect();
                Ok(AssembledWave { entries })
            } else {
                Err(refusal_message(&reasons))
            }
        }
        Err(cyclic_ids) => {
            reasons.push(format!(
                "dependency cycle among wave briefs: {}",
                cyclic_ids.join(", ")
            ));
            Err(refusal_message(&reasons))
        }
    }
}

fn load_one(design_dir: &str, id: &str) -> Result<Loaded, String> {
    let (cluster, document) = locate(design_dir, id)?;
    let cluster_dir = format!("{design_dir}/{cluster}");
    let checklist = read_json::<ChecklistFile>(&format!("{cluster_dir}/checklist.json"))?;
    let checklist_items = checklist
        .sections
        .into_iter()
        .flat_map(|section| section.items)
        .collect();
    let stories = read_json::<StoriesFile>(&format!("{cluster_dir}/stories.json"))?;
    let story_items = stories
        .personas
        .into_iter()
        .flat_map(|persona| persona.stories)
        .collect();
    let design = read_json::<DesignFile>(&format!("{cluster_dir}/design.json"))?;
    Ok(Loaded {
        id: id.to_owned(),
        cluster,
        document,
        checklist_items,
        story_items,
        intention: design.intention,
        constraints: design.constraints,
    })
}

/// Locate one brief by scanning the cluster directories under `design_dir`.
/// Exactly one match must exist.
fn locate(design_dir: &str, id: &str) -> Result<(String, BriefDocument), String> {
    let entries = std::fs::read_dir(design_dir)
        .map_err(|source| format!("assemble_wave: cannot list {design_dir}: {source}"))?;
    let mut candidates: Vec<(String, String)> = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|source| format!("assemble_wave: cannot list {design_dir}: {source}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{design_dir}/{name}/briefs/{id}.json");
        if let Ok(raw) = std::fs::read_to_string(&path) {
            candidates.push((name, raw));
        }
    }
    match candidates.as_slice() {
        [(cluster, raw)] => serde_json::from_str::<BriefDocument>(raw)
            .map(|document| (cluster.clone(), document))
            .map_err(|_| {
                format!(
                    "assemble_wave: brief {id} at {design_dir}/{cluster}/briefs/{id}.json \
                     failed to decode"
                )
            }),
        [] => Err(format!(
            "assemble_wave: brief {id} not found under {design_dir}"
        )),
        multiple => Err(format!(
            "assemble_wave: brief {id} matched multiple clusters: {}",
            multiple
                .iter()
                .map(|(cluster, _)| cluster.clone())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn brief_reasons(
    design_dir: &str,
    loaded: &Loaded,
    decisions: &[ResolvedAdr],
    roadmap: &[RoadmapItem],
    wave_ids: &[String],
) -> Vec<String> {
    let document = &loaded.document;
    let id = &document.id;
    let mut reasons = coverage_reasons(loaded);
    let decision_ids: Vec<String> = decisions.iter().map(|adr| adr.id.clone()).collect();
    for adr in &document.design_anchor {
        if !decision_ids.contains(adr) {
            reasons.push(format!(
                "{id}: design_anchor {adr} not found in decisions.json"
            ));
        }
    }
    if !roadmap
        .iter()
        .any(|item| item.links.cluster == loaded.cluster)
    {
        reasons.push(format!(
            "{id}: no roadmap item links cluster {}",
            loaded.cluster
        ));
    }
    for dep in &document.depends_on {
        if let Some(reason) = dep_reason(design_dir, id, dep, wave_ids) {
            reasons.push(reason);
        }
    }
    reasons
}

/// The per-brief coverage refusals (mirroring check-coverage.py's per-brief
/// checks): brief-level checklist/stories must equal the union of the per-R#
/// arrays in both directions, and every referenced id must exist in the
/// cluster documents.
fn coverage_reasons(loaded: &Loaded) -> Vec<String> {
    let document = &loaded.document;
    let id = &document.id;
    let union_checklist = unique(
        document
            .requirements
            .iter()
            .flat_map(|req| req.checklist.iter().cloned()),
    );
    let union_stories = unique(
        document
            .requirements
            .iter()
            .flat_map(|req| req.stories.iter().cloned()),
    );
    let checklist_ids: Vec<String> = loaded
        .checklist_items
        .iter()
        .map(|item| item.id.clone())
        .collect();
    let story_ids: Vec<String> = loaded
        .story_items
        .iter()
        .map(|item| item.id.clone())
        .collect();

    let mut reasons = Vec::new();
    for cid in &document.checklist {
        if !union_checklist.contains(cid) {
            reasons.push(format!(
                "{id}: brief-level checklist {cid} not covered by any R#"
            ));
        }
    }
    for cid in &union_checklist {
        if !document.checklist.contains(cid) {
            reasons.push(format!(
                "{id}: checklist {cid} cited by {} but missing from the brief-level array",
                citing(document, cid, true)
            ));
        }
    }
    for sid in &document.stories {
        if !union_stories.contains(sid) {
            reasons.push(format!(
                "{id}: brief-level story {sid} not covered by any R#"
            ));
        }
    }
    for sid in &union_stories {
        if !document.stories.contains(sid) {
            reasons.push(format!(
                "{id}: story {sid} cited by {} but missing from the brief-level array",
                citing(document, sid, false)
            ));
        }
    }
    for cid in unique(
        document
            .checklist
            .iter()
            .cloned()
            .chain(union_checklist.iter().cloned()),
    ) {
        if !checklist_ids.contains(&cid) {
            reasons.push(format!(
                "{id}: checklist id {cid} not found in the cluster checklist"
            ));
        }
    }
    for sid in unique(
        document
            .stories
            .iter()
            .cloned()
            .chain(union_stories.iter().cloned()),
    ) {
        if !story_ids.contains(&sid) {
            reasons.push(format!(
                "{id}: story id {sid} not found in the cluster stories"
            ));
        }
    }
    reasons
}

/// The comma-joined R# ids whose checklist (or stories) array cites `id`.
fn citing(document: &BriefDocument, id: &str, is_checklist: bool) -> String {
    document
        .requirements
        .iter()
        .filter(|req| {
            if is_checklist {
                req.checklist.iter().any(|c| c == id)
            } else {
                req.stories.iter().any(|s| s == id)
            }
        })
        .map(|req| req.id.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn dep_reason(design_dir: &str, id: &str, dep: &str, wave_ids: &[String]) -> Option<String> {
    if wave_ids.iter().any(|w| w == dep) {
        return None;
    }
    match dep_status(design_dir, dep) {
        DepStatus::Landed => None,
        DepStatus::NotFound => Some(format!(
            "{id} depends on {dep}, which is not in the wave and was not found on disk"
        )),
        DepStatus::NotLanded => Some(format!(
            "{id} depends on {dep}, which is not in the wave and is not landed on disk \
             (no execution block with status landed)"
        )),
    }
}

fn dep_status(design_dir: &str, dep: &str) -> DepStatus {
    match locate(design_dir, dep) {
        Err(_) => DepStatus::NotFound,
        Ok((_, document)) => match document.execution {
            Some(block) if block.status == ExecutionStatus::Landed => DepStatus::Landed,
            _ => DepStatus::NotLanded,
        },
    }
}

fn refusal_message(reasons: &[String]) -> String {
    format!("assemble_wave refused the wave: {}", reasons.join("; "))
}

/// Stable topological order: every within-wave `depends_on` precedes its
/// dependent, the caller's order preserved among independents. `Err` carries
/// the ids that could not be placed — a cycle.
fn order<'a>(loaded: &'a [Loaded], wave_ids: &[String]) -> Result<Vec<&'a Loaded>, Vec<String>> {
    let mut remaining: Vec<&Loaded> = loaded.iter().collect();
    let mut placed: BTreeSet<String> = BTreeSet::new();
    let mut ordered: Vec<&Loaded> = Vec::with_capacity(loaded.len());

    while !remaining.is_empty() {
        let pick = remaining.iter().position(|entry| {
            entry
                .document
                .depends_on
                .iter()
                .filter(|dep| wave_ids.iter().any(|w| w == *dep))
                .all(|dep| placed.contains(dep))
        });
        match pick {
            Some(index) => {
                let chosen = remaining.remove(index);
                placed.insert(chosen.id.clone());
                ordered.push(chosen);
            }
            None => return Err(remaining.iter().map(|entry| entry.id.clone()).collect()),
        }
    }
    Ok(ordered)
}

fn build_entry(
    loaded: &Loaded,
    decisions: &[ResolvedAdr],
    roadmap: &[RoadmapItem],
    design_dir: &str,
) -> WaveEntry {
    let document = &loaded.document;
    let adrs = document
        .design_anchor
        .iter()
        .filter_map(|adr_id| decisions.iter().find(|adr| &adr.id == adr_id).cloned())
        .collect();
    let checklist = document
        .checklist
        .iter()
        .filter_map(|cid| {
            loaded
                .checklist_items
                .iter()
                .find(|item| &item.id == cid)
                .cloned()
        })
        .collect();
    let stories = document
        .stories
        .iter()
        .filter_map(|sid| {
            loaded
                .story_items
                .iter()
                .find(|item| &item.id == sid)
                .cloned()
        })
        .collect();
    let provenance = roadmap
        .iter()
        .find(|item| item.links.cluster == loaded.cluster)
        .map_or_else(
            || ResolvedProvenance {
                requested_by: String::new(),
                quote: String::new(),
            },
            |item| ResolvedProvenance {
                requested_by: item.provenance.requested_by.clone(),
                quote: item.provenance.quote.clone(),
            },
        );
    WaveEntry {
        brief_document: document.clone(),
        resolved_context: ResolvedContext {
            adrs,
            checklist,
            stories,
            constraints: loaded.constraints.clone(),
            intention: loaded.intention.clone(),
            design_path: format!("{design_dir}/{}/design.json", loaded.cluster),
            provenance,
        },
    }
}

/// First-seen-preserving deduplication, matching `gleam/list.unique`.
fn unique(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
}

fn read_json<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|source| format!("assemble_wave: cannot read {path}: {source}"))?;
    serde_json::from_str(&raw).map_err(|_| format!("assemble_wave: cannot parse {path} as JSON"))
}
