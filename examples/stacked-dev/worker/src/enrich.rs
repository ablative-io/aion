//! Pure append-only merge of stage reports and the execution block into the
//! brief document, mirroring `../../src/stacked_dev/enrich.gleam` and the
//! authored-subset guard (CN3). Each merge replaces the targeted stage's
//! blocks WHOLESALE and copies every authored field through verbatim; the
//! `enrich_brief` handler enforces the guard before writing.

use crate::types::{
    BriefDocument, BriefRequirement, DevBlock, Enrichment, ReviewBlock, ScoutBlock,
};

/// A merge that cannot be applied: the report named a requirement the brief
/// does not carry — a contract mismatch, never silently dropped.
#[derive(Debug)]
pub struct UnknownRequirementId(pub String);

impl std::fmt::Display for UnknownRequirementId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "report enrichment names requirement {}, which the brief does not define",
            self.0
        )
    }
}

/// The first authored field that differs between the on-disk document and the
/// handed one, named in schema property order
/// (`requirements/2/spec` for a nested divergence), or `None` when the
/// authored subsets are identical. Enrichment blocks are never compared — they
/// are the pipeline's to change.
#[must_use]
pub fn authored_divergence(disk: &BriefDocument, handed: &BriefDocument) -> Option<String> {
    let scalar = [
        ("id", disk.id == handed.id),
        ("cluster", disk.cluster == handed.cluster),
        ("title", disk.title == handed.title),
        ("depends_on", disk.depends_on == handed.depends_on),
        ("blocked_by", disk.blocked_by == handed.blocked_by),
        ("checklist", disk.checklist == handed.checklist),
        ("stories", disk.stories == handed.stories),
        ("design_anchor", disk.design_anchor == handed.design_anchor),
        ("purpose", disk.purpose == handed.purpose),
        ("task", disk.task == handed.task),
    ];
    if let Some(field) = first_failing(&scalar) {
        return Some(field);
    }
    if let Some(field) = requirements_divergence(&disk.requirements, &handed.requirements) {
        return Some(field);
    }
    first_failing(&[
        ("boundaries", disk.boundaries == handed.boundaries),
        ("verification", disk.verification == handed.verification),
    ])
}

fn requirements_divergence(
    disk: &[BriefRequirement],
    handed: &[BriefRequirement],
) -> Option<String> {
    if disk.len() != handed.len() {
        return Some("requirements".to_owned());
    }
    for (index, (disk_req, handed_req)) in disk.iter().zip(handed.iter()).enumerate() {
        let prefix = format!("requirements/{index}/");
        let checks = [
            (format!("{prefix}id"), disk_req.id == handed_req.id),
            (format!("{prefix}title"), disk_req.title == handed_req.title),
            (format!("{prefix}spec"), disk_req.spec == handed_req.spec),
            (
                format!("{prefix}acceptance"),
                disk_req.acceptance == handed_req.acceptance,
            ),
            (format!("{prefix}files"), disk_req.files == handed_req.files),
            (
                format!("{prefix}checklist"),
                disk_req.checklist == handed_req.checklist,
            ),
            (
                format!("{prefix}stories"),
                disk_req.stories == handed_req.stories,
            ),
        ];
        if let Some(failing) = checks.iter().find(|(_, ok)| !*ok) {
            return Some(failing.0.clone());
        }
    }
    None
}

fn first_failing(checks: &[(&str, bool)]) -> Option<String> {
    checks
        .iter()
        .find(|(_, ok)| !*ok)
        .map(|(field, _)| (*field).to_owned())
}

/// Apply the merge selected by the [`Enrichment`] variant to the handed
/// document, replacing that stage's block wholesale. The execution block is
/// written exactly as given — gate and attestation stay separate (P1).
///
/// # Errors
///
/// [`UnknownRequirementId`] when a report enrichment names a requirement the
/// brief does not define.
pub fn apply(
    mut document: BriefDocument,
    enrichment: &Enrichment,
) -> Result<BriefDocument, UnknownRequirementId> {
    match enrichment {
        Enrichment::Scout { report } => {
            for entry in &report.enrichments {
                let requirement = find_requirement(&mut document, &entry.id)?;
                requirement.scout = Some(ScoutBlock {
                    files: entry.files.clone(),
                    context: entry.context.clone(),
                    approach: entry.approach.clone(),
                    notes: entry.notes.clone(),
                });
            }
        }
        Enrichment::Dev { report } => {
            for entry in &report.enrichments {
                let requirement = find_requirement(&mut document, &entry.id)?;
                requirement.dev = Some(DevBlock {
                    status: entry.status,
                    files_changed: entry.files_changed.clone(),
                    how: entry.how.clone(),
                    deviation: entry.deviation.clone(),
                    checklist: entry.checklist.clone(),
                    stories: entry.stories.clone(),
                });
            }
        }
        Enrichment::Review { report } => {
            for entry in &report.enrichments {
                let requirement = find_requirement(&mut document, &entry.id)?;
                requirement.review = Some(ReviewBlock {
                    alignment: entry.alignment,
                    acceptance: entry.acceptance.clone(),
                    checklist: entry.checklist.clone(),
                    stories: entry.stories.clone(),
                    issues: entry.issues.clone(),
                    fixes: entry.fixes.clone(),
                });
            }
        }
        Enrichment::Execution { block } => {
            document.execution = Some(block.clone());
        }
    }
    Ok(document)
}

fn find_requirement<'a>(
    document: &'a mut BriefDocument,
    id: &str,
) -> Result<&'a mut BriefRequirement, UnknownRequirementId> {
    document
        .requirements
        .iter_mut()
        .find(|requirement| requirement.id == id)
        .ok_or_else(|| UnknownRequirementId(id.to_owned()))
}
