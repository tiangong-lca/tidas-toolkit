use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use walkdir::WalkDir;
use zip::ZipArchive;

const TEXT_PROBE_BYTES: u64 = 4 * 1024;
const STRUCTURED_PROBE_BYTES: u64 = 1024 * 1024;
const EVIDENCE_LIMIT: usize = 25;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceFormat {
    Ecospold1,
    Ecospold2,
    SimaproCsv,
    OpenlcaJsonld,
    OpenlcaProcessXlsx,
    Ilcd,
    UnsupportedZolca,
    Unknown,
}

impl SourceFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ecospold1 => "ecospold1",
            Self::Ecospold2 => "ecospold2",
            Self::SimaproCsv => "simapro-csv",
            Self::OpenlcaJsonld => "openlca-jsonld",
            Self::OpenlcaProcessXlsx => "openlca-process-xlsx",
            Self::Ilcd => "ilcd",
            Self::UnsupportedZolca => "unsupported-zolca",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DetectionConfidence {
    Low,
    Medium,
    High,
}

impl DetectionConfidence {
    const fn score(self) -> u64 {
        match self {
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DetectionRequest {
    pub source: PathBuf,
    pub requested_format: Option<SourceFormat>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DetectedFormat {
    pub format: SourceFormat,
    pub confidence: DetectionConfidence,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug)]
struct Candidate {
    format: SourceFormat,
    confidence: DetectionConfidence,
    evidence: String,
}

#[derive(Default)]
struct CandidateAccumulator {
    groups: BTreeMap<SourceFormat, CandidateGroup>,
}

struct CandidateGroup {
    aggregate_score: u64,
    strongest_confidence: DetectionConfidence,
    evidence: Vec<String>,
    total: u64,
}

impl CandidateAccumulator {
    fn push(&mut self, candidate: Candidate) {
        let group = self
            .groups
            .entry(candidate.format)
            .or_insert_with(|| CandidateGroup {
                aggregate_score: 0,
                strongest_confidence: candidate.confidence,
                evidence: Vec::with_capacity(EVIDENCE_LIMIT),
                total: 0,
            });
        group.aggregate_score = group
            .aggregate_score
            .saturating_add(candidate.confidence.score());
        group.strongest_confidence = group.strongest_confidence.max(candidate.confidence);
        group.total = group.total.saturating_add(1);
        if group.evidence.len() < EVIDENCE_LIMIT {
            group.evidence.push(candidate.evidence);
        }
    }
}

pub fn detect_format(request: &DetectionRequest) -> Result<DetectedFormat, DetectionError> {
    if !request.source.exists() {
        return Err(DetectionError::MissingSource(request.source.clone()));
    }
    if has_extension(&request.source, "zolca") {
        return Ok(DetectedFormat {
            format: SourceFormat::UnsupportedZolca,
            confidence: DetectionConfidence::High,
            evidence: vec!["file extension .zolca".to_owned()],
        });
    }
    if let Some(format) = request.requested_format {
        if matches!(
            format,
            SourceFormat::Unknown | SourceFormat::UnsupportedZolca
        ) {
            return Err(DetectionError::InvalidRequestedFormat(format));
        }
        return Ok(DetectedFormat {
            format,
            confidence: DetectionConfidence::High,
            evidence: vec![format!("explicit --from-format {}", format.as_str())],
        });
    }

    let mut candidates = CandidateAccumulator::default();
    scan_path(&request.source, &mut candidates)?;
    select_candidate(candidates)
}

fn select_candidate(
    mut candidates: CandidateAccumulator,
) -> Result<DetectedFormat, DetectionError> {
    if candidates.groups.is_empty() {
        return Ok(DetectedFormat {
            format: SourceFormat::Unknown,
            confidence: DetectionConfidence::Low,
            evidence: vec!["no supported LCA format signature found".to_owned()],
        });
    }
    if let Some(unsupported) = candidates.groups.remove(&SourceFormat::UnsupportedZolca) {
        return Ok(DetectedFormat {
            format: SourceFormat::UnsupportedZolca,
            confidence: DetectionConfidence::High,
            evidence: unsupported.render_evidence(""),
        });
    }

    let top_score = candidates
        .groups
        .values()
        .map(|group| group.aggregate_score)
        .max()
        .unwrap_or_default();
    let top_formats = candidates
        .groups
        .iter()
        .filter_map(|(format, group)| (group.aggregate_score == top_score).then_some(*format))
        .collect::<Vec<_>>();
    if top_formats.len() != 1 {
        let details = top_formats
            .iter()
            .map(|format| {
                let group = &candidates.groups[format];
                let extra = group.total.saturating_sub(1);
                let suffix = if extra == 0 {
                    String::new()
                } else {
                    format!(" (+{extra} more)")
                };
                format!("{}: {}{suffix}", format.as_str(), group.evidence[0])
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(DetectionError::Ambiguous(details));
    }
    let selected = top_formats[0];
    let selected_group = candidates
        .groups
        .remove(&selected)
        .expect("selected candidate group exists");
    let confidence = selected_group.strongest_confidence;
    let mut evidence = selected_group.render_evidence("");
    for (format, group) in candidates.groups {
        evidence.extend(group.render_evidence(&format!("secondary {}: ", format.as_str())));
    }
    Ok(DetectedFormat {
        format: selected,
        confidence,
        evidence,
    })
}

impl CandidateGroup {
    fn render_evidence(&self, prefix: &str) -> Vec<String> {
        let mut evidence = self
            .evidence
            .iter()
            .map(|item| format!("{prefix}{item}"))
            .collect::<Vec<_>>();
        let extra = self
            .total
            .saturating_sub(u64::try_from(self.evidence.len()).unwrap_or(u64::MAX));
        if extra > 0 {
            evidence.push(format!(
                "{prefix}{extra} additional matching signatures omitted"
            ));
        }
        evidence
    }
}

fn scan_path(path: &Path, candidates: &mut CandidateAccumulator) -> Result<(), DetectionError> {
    if path.is_dir() {
        return scan_directory(path, candidates);
    }
    match extension(path).as_deref() {
        Some("spold") => candidates.push(candidate(
            SourceFormat::Ecospold2,
            DetectionConfidence::High,
            "file extension .spold",
        )),
        Some("csv" | "txt") => scan_csv(&read_probe(path, TEXT_PROBE_BYTES)?, path, candidates),
        Some("xml") => scan_xml(&read_probe(path, STRUCTURED_PROBE_BYTES)?, path, candidates),
        Some("xlsx") => scan_xlsx(path, candidates)?,
        Some("zip") => scan_zip(path, candidates)?,
        Some("json" | "jsonld") => {
            scan_json(&read_probe(path, STRUCTURED_PROBE_BYTES)?, path, candidates);
        }
        _ => {}
    }
    Ok(())
}

fn scan_directory(
    path: &Path,
    candidates: &mut CandidateAccumulator,
) -> Result<(), DetectionError> {
    if path.join("context.jsonld").is_file() {
        candidates.push(candidate(
            SourceFormat::OpenlcaJsonld,
            DetectionConfidence::High,
            "directory has context.jsonld",
        ));
    }
    let mut paths = WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .map(|entry| entry.map(walkdir::DirEntry::into_path))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for child in paths {
        if !child.is_file() {
            continue;
        }
        match extension(&child).as_deref() {
            Some("zolca") => candidates.push(candidate(
                SourceFormat::UnsupportedZolca,
                DetectionConfidence::High,
                format!("{}: .zolca file", child.display()),
            )),
            Some("spold") => candidates.push(candidate(
                SourceFormat::Ecospold2,
                DetectionConfidence::High,
                format!("{}: .spold file", child.display()),
            )),
            Some("csv" | "txt") => {
                scan_csv(&read_probe(&child, TEXT_PROBE_BYTES)?, &child, candidates);
            }
            Some("xml") => scan_xml(
                &read_probe(&child, STRUCTURED_PROBE_BYTES)?,
                &child,
                candidates,
            ),
            Some("json" | "jsonld") => scan_json(
                &read_probe(&child, STRUCTURED_PROBE_BYTES)?,
                &child,
                candidates,
            ),
            Some("xlsx") => scan_xlsx(&child, candidates)?,
            _ => {}
        }
    }
    Ok(())
}

fn scan_zip(path: &Path, candidates: &mut CandidateAccumulator) -> Result<(), DetectionError> {
    let file = File::open(path)?;
    let mut archive = match ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(zip::result::ZipError::InvalidArchive(_)) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let has_context = (0..archive.len()).try_fold(false, |found, index| {
        let entry = archive.by_index(index)?;
        Ok::<_, zip::result::ZipError>(
            found
                || Path::new(entry.name()?.as_ref())
                    .file_name()
                    .is_some_and(|name| name == "context.jsonld"),
        )
    })?;
    if has_context {
        candidates.push(candidate(
            SourceFormat::OpenlcaJsonld,
            DetectionConfidence::High,
            format!("{}: zip contains context.jsonld", path.display()),
        ));
    }
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name()?.into_owned();
        let label = PathBuf::from(format!("{}:{name}", path.display()));
        match extension(Path::new(&name)).as_deref() {
            Some("zolca") => candidates.push(candidate(
                SourceFormat::UnsupportedZolca,
                DetectionConfidence::High,
                format!("{}: zip entry {name} is .zolca", path.display()),
            )),
            Some("spold") => candidates.push(candidate(
                SourceFormat::Ecospold2,
                DetectionConfidence::High,
                format!("{}: zip entry {name} has .spold extension", path.display()),
            )),
            Some("csv" | "txt") => {
                let bytes = read_bounded(&mut entry, TEXT_PROBE_BYTES)?;
                scan_csv(&bytes, &label, candidates);
            }
            Some("xml") => {
                let bytes = read_bounded(&mut entry, STRUCTURED_PROBE_BYTES)?;
                scan_xml(&bytes, &label, candidates);
            }
            Some("json" | "jsonld") => {
                let bytes = read_bounded(&mut entry, STRUCTURED_PROBE_BYTES)?;
                scan_json(&bytes, &label, candidates);
            }
            _ => {}
        }
    }
    Ok(())
}

fn scan_xlsx(path: &Path, candidates: &mut CandidateAccumulator) -> Result<(), DetectionError> {
    let file = File::open(path)?;
    let mut archive = match ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(zip::result::ZipError::InvalidArchive(_)) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut content_types = false;
    let mut workbook = false;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name()?;
        content_types |= name == "[Content_Types].xml";
        workbook |= name == "xl/workbook.xml";
    }
    if content_types && workbook {
        candidates.push(candidate(
            SourceFormat::OpenlcaProcessXlsx,
            DetectionConfidence::Medium,
            format!("{}: valid XLSX workbook container", path.display()),
        ));
    }
    Ok(())
}

fn scan_csv(bytes: &[u8], label: &Path, candidates: &mut CandidateAccumulator) {
    let text = String::from_utf8_lossy(bytes);
    let first_line = text
        .trim_start_matches('\u{feff}')
        .lines()
        .next()
        .unwrap_or("");
    if first_line.starts_with("{SimaPro ") {
        candidates.push(candidate(
            SourceFormat::SimaproCsv,
            DetectionConfidence::High,
            format!("{}: first line starts with '{{SimaPro '", label.display()),
        ));
    }
}

fn scan_xml(bytes: &[u8], label: &Path, candidates: &mut CandidateAccumulator) {
    let Some((name, namespace)) = xml_root(bytes) else {
        return;
    };
    let local_name = name.rsplit(':').next().unwrap_or(&name);
    let signature = format!("{name} {namespace}").to_ascii_lowercase();
    let evidence = format!("{}: XML root {{{namespace}}}{local_name}", label.display());
    if signature.contains("ecospold02") || local_name.eq_ignore_ascii_case("ecospold2") {
        candidates.push(candidate(
            SourceFormat::Ecospold2,
            DetectionConfidence::High,
            evidence,
        ));
    } else if signature.contains("ecospold01") || local_name.eq_ignore_ascii_case("ecospold") {
        let confidence = if signature.contains("ecospold01") {
            DetectionConfidence::High
        } else {
            DetectionConfidence::Medium
        };
        candidates.push(candidate(SourceFormat::Ecospold1, confidence, evidence));
    } else if signature.contains("lca.jrc.it/ilcd")
        && local_name.to_ascii_lowercase().ends_with("dataset")
    {
        candidates.push(candidate(
            SourceFormat::Ilcd,
            DetectionConfidence::High,
            evidence,
        ));
    }
}

fn xml_root(bytes: &[u8]) -> Option<(String, String)> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().check_end_names = true;
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Start(element) | Event::Empty(element) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let namespace = element
                    .attributes()
                    .with_checks(true)
                    .filter_map(Result::ok)
                    .find(|attribute| {
                        attribute.key.as_ref() == b"xmlns"
                            || attribute.key.as_ref().starts_with(b"xmlns:")
                    })
                    .map_or_else(String::new, |attribute| {
                        String::from_utf8_lossy(attribute.value.as_ref()).into_owned()
                    });
                return Some((name, namespace));
            }
            Event::Eof => return None,
            _ => {}
        }
        buffer.clear();
    }
}

fn scan_json(bytes: &[u8], label: &Path, candidates: &mut CandidateAccumulator) {
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes),
    ) else {
        return;
    };
    let Some(object) = payload.as_object() else {
        return;
    };
    if looks_like_openlca_context(object.get("@context"))
        || looks_like_openlca_type(object.get("@type"))
    {
        candidates.push(candidate(
            SourceFormat::OpenlcaJsonld,
            DetectionConfidence::High,
            format!(
                "{}: JSON-LD @context/@type looks like openLCA",
                label.display()
            ),
        ));
    } else if ["@id", "refId", "version"]
        .iter()
        .any(|key| object.contains_key(*key))
        && ["name", "category", "flowType", "processType"]
            .iter()
            .any(|key| object.contains_key(*key))
    {
        candidates.push(candidate(
            SourceFormat::OpenlcaJsonld,
            DetectionConfidence::Medium,
            format!("{}: JSON object has openLCA-like fields", label.display()),
        ));
    }
}

fn looks_like_openlca_context(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::String(value)) => {
            let value = value.to_ascii_lowercase();
            value.contains("openlca") || value.contains("olca") || value.contains("context.jsonld")
        }
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .any(|value| looks_like_openlca_context(Some(value))),
        Some(serde_json::Value::Object(values)) => values.keys().any(|key| {
            let key = key.to_ascii_lowercase();
            key == "olca" || key == "openlca"
        }),
        _ => false,
    }
}

fn looks_like_openlca_type(value: Option<&serde_json::Value>) -> bool {
    const TYPES: &[&str] = &[
        "actor",
        "source",
        "unitgroup",
        "flowproperty",
        "flow",
        "process",
        "impactcategory",
        "impactmethod",
        "productsystem",
        "epd",
    ];
    value
        .and_then(serde_json::Value::as_str)
        .map(|value| value.replace(' ', "").to_ascii_lowercase())
        .is_some_and(|value| TYPES.contains(&value.as_str()))
}

fn read_probe(path: &Path, limit: u64) -> Result<Vec<u8>, DetectionError> {
    read_bounded(&mut File::open(path)?, limit)
}

fn read_bounded(reader: &mut impl Read, limit: u64) -> Result<Vec<u8>, DetectionError> {
    let capacity = usize::try_from(limit).map_err(|_| DetectionError::ProbeTooLarge(limit))?;
    let mut bytes = Vec::with_capacity(capacity);
    reader.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
}

fn has_extension(path: &Path, expected: &str) -> bool {
    extension(path).as_deref() == Some(expected)
}

fn candidate(
    format: SourceFormat,
    confidence: DetectionConfidence,
    evidence: impl Into<String>,
) -> Candidate {
    Candidate {
        format,
        confidence,
        evidence: evidence.into(),
    }
}

#[derive(Debug, Error)]
pub enum DetectionError {
    #[error("import source does not exist: {0}")]
    MissingSource(PathBuf),
    #[error("requested import format is not a supported explicit format: {0:?}")]
    InvalidRequestedFormat(SourceFormat),
    #[error("ambiguous source format detection; candidates: {0}")]
    Ambiguous(String),
    #[error("detection probe size cannot be represented: {0}")]
    ProbeTooLarge(u64),
    #[error("format detection I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("format detection traversal failed: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("format detection ZIP inspection failed: {0}")]
    Zip(#[from] zip::result::ZipError),
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    use super::*;

    fn request(source: &Path) -> DetectionRequest {
        DetectionRequest {
            source: source.to_path_buf(),
            requested_format: None,
        }
    }

    #[test]
    fn explicit_format_does_not_override_zolca_rejection() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("database.zolca");
        std::fs::write(&source, b"SQLite format 3").unwrap();
        let detected = detect_format(&DetectionRequest {
            source,
            requested_format: Some(SourceFormat::Ilcd),
        })
        .unwrap();
        assert_eq!(detected.format, SourceFormat::UnsupportedZolca);
    }

    #[test]
    fn detects_every_single_file_signature() {
        let directory = tempdir().unwrap();
        let cases = [
            (
                "input.csv",
                b"{SimaPro 9.5}\\n{processes}".as_slice(),
                SourceFormat::SimaproCsv,
            ),
            (
                "input.xml",
                br#"<ecoSpold xmlns="http://www.EcoInvent.org/EcoSpold01"/>"#.as_slice(),
                SourceFormat::Ecospold1,
            ),
            (
                "input.spold",
                b"<anything/>".as_slice(),
                SourceFormat::Ecospold2,
            ),
            (
                "input.json",
                br#"{"@type":"Process","@id":"p"}"#.as_slice(),
                SourceFormat::OpenlcaJsonld,
            ),
            (
                "process.xml",
                br#"<processDataSet xmlns="http://lca.jrc.it/ILCD/Process"/>"#.as_slice(),
                SourceFormat::Ilcd,
            ),
        ];
        for (name, bytes, expected) in cases {
            let source = directory.path().join(name);
            std::fs::write(&source, bytes).unwrap();
            assert_eq!(detect_format(&request(&source)).unwrap().format, expected);
        }
    }

    #[test]
    fn zip_rejects_embedded_zolca_before_other_candidates() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("mixed.zip");
        let file = File::create(&source).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        writer.start_file("context.jsonld", options).unwrap();
        writer
            .write_all(br#"{"@context":"https://openlca.org"}"#)
            .unwrap();
        writer.start_file("database.zolca", options).unwrap();
        writer.write_all(b"SQLite format 3").unwrap();
        writer.finish().unwrap();

        let detected = detect_format(&request(&source)).unwrap();
        assert_eq!(detected.format, SourceFormat::UnsupportedZolca);
    }

    #[test]
    fn xlsx_container_is_a_medium_confidence_candidate() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("process.xlsx");
        let file = File::create(&source).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        writer.start_file("[Content_Types].xml", options).unwrap();
        writer.write_all(b"<Types/>").unwrap();
        writer.start_file("xl/workbook.xml", options).unwrap();
        writer.write_all(b"<workbook/>").unwrap();
        writer.finish().unwrap();

        let detected = detect_format(&request(&source)).unwrap();
        assert_eq!(detected.format, SourceFormat::OpenlcaProcessXlsx);
        assert_eq!(detected.confidence, DetectionConfidence::Medium);
    }

    #[test]
    fn zip_detection_decodes_utf8_and_legacy_entry_names() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.zip");
        for (raw_name, expected) in [
            ("目录/记录.spold".as_bytes(), "目录/记录.spold"),
            (b"\x82.spold".as_slice(), "é.spold"),
        ] {
            std::fs::write(
                &source,
                crate::source::zip_with_raw_name(raw_name, b"<ecoSpold/>"),
            )
            .unwrap();
            let detected = detect_format(&request(&source)).unwrap();
            assert_eq!(detected.format, SourceFormat::Ecospold2);
            assert!(detected.evidence.iter().any(|item| item.contains(expected)));
        }
    }

    #[test]
    fn corrupt_zip_entry_is_an_error_instead_of_a_format_candidate() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.zip");
        let mut bytes = crate::source::zip_with_raw_name(b"record.spold", b"<ecoSpold/>");
        bytes[0] ^= 1;
        std::fs::write(&source, bytes).unwrap();
        assert!(matches!(
            detect_format(&request(&source)),
            Err(DetectionError::Zip(_))
        ));
    }

    #[test]
    fn equal_aggregate_scores_are_rejected_as_ambiguous() {
        let directory = tempdir().unwrap();
        std::fs::write(
            directory.path().join("process.xml"),
            br#"<ecoSpold xmlns="http://www.EcoInvent.org/EcoSpold01"/>"#,
        )
        .unwrap();
        std::fs::write(
            directory.path().join("process.json"),
            br#"{"@type":"Process","@id":"p"}"#,
        )
        .unwrap();
        assert!(matches!(
            detect_format(&request(directory.path())),
            Err(DetectionError::Ambiguous(_))
        ));
    }

    #[test]
    fn directory_prefers_dominant_package_and_caps_evidence() {
        let directory = tempdir().unwrap();
        std::fs::write(
            directory.path().join("guide.csv"),
            b"{SimaPro 9.5}\\n{processes}",
        )
        .unwrap();
        for index in 0..30 {
            std::fs::write(
                directory.path().join(format!("{index}.spold")),
                b"<ecoSpold/>",
            )
            .unwrap();
        }
        let detected = detect_format(&request(directory.path())).unwrap();
        assert_eq!(detected.format, SourceFormat::Ecospold2);
        assert!(detected.evidence.len() <= EVIDENCE_LIMIT + 2);
    }

    #[test]
    fn candidate_accumulation_is_bounded_independent_of_match_count() {
        let mut candidates = CandidateAccumulator::default();
        for index in 0..100_000 {
            candidates.push(candidate(
                SourceFormat::Ecospold2,
                DetectionConfidence::High,
                format!("dataset-{index}.spold"),
            ));
        }
        let group = &candidates.groups[&SourceFormat::Ecospold2];
        assert_eq!(group.total, 100_000);
        assert_eq!(group.evidence.len(), EVIDENCE_LIMIT);

        let detected = select_candidate(candidates).unwrap();
        assert_eq!(detected.format, SourceFormat::Ecospold2);
        assert_eq!(detected.evidence.len(), EVIDENCE_LIMIT + 1);
        assert!(detected.evidence.last().unwrap().contains("99975"));
    }
}
