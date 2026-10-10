//! Test-only frozen025 control: source-only AST context and indivisible exact support.
//! No query, task, discovery rank, expected answer or reference packet enters compile().
use super::*;
use crate::catalog::{DocumentRow, query::QuerySnapshot};
use pulldown_cmark::{Event, Parser, Tag};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) const RECIPE: &str = "lwiki.structural025.v1;pulldown-cmark0.13.4-default-outer-heading-paragraph-code-list-item;no-split;heading-chain+adjacent-list-intro+whole-unit;exact-title-ancestor-intro-unit;unicode61-remove_diacritics2;normalized-token-set-literal-OR;bm25-1-1-1-1;source-revision-start-end-id;80-once;exact-closure-no-merge;serialized-spans-v1";
const MIB: usize = 1024 * 1024;
const OWNER_BYTES: usize = MIB;
const PREP_BYTES: usize = 24 * MIB;
const PREP_STARTS: usize = 24576;
const OWNER_ROWS: usize = 128;
const OWNER_RECORD_BYTES: usize = 64 * 1024;
const PREP_RECORD_BYTES: usize = 4 * MIB;

fn err(code: ErrorCode, text: &str) -> WikiError {
    WikiError::new(code, text)
}
fn require(ok: bool, code: ErrorCode, text: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(err(code, text)) }
}
fn encoded<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| WikiError::invalid(e.to_string()))
}
fn span(start: usize, end: usize) -> Result<ByteSpan> {
    ByteSpan::new(start as u64, end as u64)
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(|e| WikiError::invalid(e.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Owner {
    source: RecordId,
    revision: RecordId,
    path: VaultRelativePath,
    hash: Blake3Hash,
    title: String,
}
impl Owner {
    fn of(document: &DocumentRow) -> Result<Self> {
        require(
            document.eligibility == Eligibility::Current && document.record_id.is_none(),
            ErrorCode::FreshnessConflict,
            "025 owner is not a current captured source",
        )?;
        Ok(Self {
            source: document
                .source_id
                .clone()
                .ok_or_else(|| err(ErrorCode::FreshnessConflict, "025 source missing"))?,
            revision: document
                .owner_revision
                .clone()
                .ok_or_else(|| err(ErrorCode::FreshnessConflict, "025 revision missing"))?,
            path: document.path.clone(),
            hash: document.hash.clone(),
            title: document.title.clone(),
        })
    }
    fn key(&self) -> (RecordId, RecordId) {
        (self.source.clone(), self.revision.clone())
    }
}

// This is the complete compiler boundary. It contains source-owned data only.
pub(crate) struct CompilerInput {
    owner: Owner,
    raw: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: Blake3Hash,
    unit: ByteSpan,
    headings: Vec<ByteSpan>,
    intro: Option<ByteSpan>,
}
impl Record {
    fn closure(&self) -> Vec<ByteSpan> {
        let mut result = Vec::new();
        for range in self
            .headings
            .iter()
            .copied()
            .chain(self.intro)
            .chain(std::iter::once(self.unit))
        {
            if !result.contains(&range) {
                result.push(range);
            }
        }
        result
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sidecar {
    recipe: String,
    owner: Owner,
    parser_starts: usize,
    records: Vec<Record>,
    oversized_closures: usize,
}
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct PreparationUsage {
    pub owners: usize,
    pub source_bytes: usize,
    pub parser_starts: usize,
    pub records: usize,
    pub record_bytes: usize,
    pub oversized_closures: usize,
    pub seconds: f64,
}
struct PreparationMeter {
    start: Instant,
    usage: PreparationUsage,
}
impl PreparationMeter {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            usage: PreparationUsage::default(),
        }
    }
    fn check(&self) -> Result<()> {
        require(
            self.start.elapsed() < Duration::from_secs(3600),
            ErrorCode::BudgetExceeded,
            "025 preparation60min cap",
        )
    }
    fn parser_start(&mut self) -> Result<()> {
        self.check()?;
        self.usage.parser_starts += 1;
        require(
            self.usage.parser_starts <= PREP_STARTS,
            ErrorCode::BudgetExceeded,
            "025 complete parser-start cap",
        )
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Heading(usize),
    Prose,
    Code,
    List,
    Item,
}
struct Block {
    span: ByteSpan,
    kind: Kind,
}

fn compile(input: &CompilerInput, meter: &mut PreparationMeter) -> Result<Sidecar> {
    meter.check()?;
    require(
        input.raw.len() <= OWNER_BYTES,
        ErrorCode::BudgetExceeded,
        "025 complete per-owner source-byte cap",
    )?;
    require(
        Blake3Hash::digest(input.raw.as_bytes()) == input.owner.hash,
        ErrorCode::FreshnessConflict,
        "025 complete source bytes/hash disagree",
    )?;
    require(
        meter.usage.owners < 60 && meter.usage.source_bytes + input.raw.len() <= PREP_BYTES,
        ErrorCode::BudgetExceeded,
        "025 complete owner/source coverage cap",
    )?;
    meter.usage.owners += 1;
    meter.usage.source_bytes += input.raw.len();
    let before = meter.usage.parser_starts;
    let mut blocks = Vec::new();
    for (event, range) in Parser::new(&input.raw).into_offset_iter() {
        let kind = match event {
            Event::Start(Tag::Heading { level, .. }) => Kind::Heading(level as usize),
            Event::Start(Tag::Paragraph) => Kind::Prose,
            Event::Start(Tag::CodeBlock(_)) => Kind::Code,
            Event::Start(Tag::List(_)) => Kind::List,
            Event::Start(Tag::Item) => Kind::Item,
            _ => continue,
        };
        meter.parser_start()?;
        if !range.is_empty() {
            blocks.push(Block {
                span: span(range.start, range.end)?,
                kind,
            });
        }
    }
    blocks.sort_by_key(|b| (b.span.start(), std::cmp::Reverse(b.span.end())));
    let mut outer: Vec<Block> = Vec::new();
    for block in blocks {
        if outer
            .last()
            .is_some_and(|previous| previous.span.end() >= block.span.end())
        {
            continue;
        }
        outer.push(block);
    }
    if outer.is_empty() && !input.raw.is_empty() {
        outer.push(Block {
            span: span(0, input.raw.len())?,
            kind: Kind::Prose,
        });
    }
    let mut chain = Vec::<(usize, ByteSpan)>::new();
    let mut previous: Option<(Kind, ByteSpan)> = None;
    let mut records = Vec::new();
    for block in outer {
        meter.check()?;
        if let Kind::Heading(level) = block.kind {
            while chain.last().is_some_and(|(old, _)| *old >= level) {
                chain.pop();
            }
            chain.push((level, block.span));
            previous = None;
            continue;
        }
        let intro = if block.kind == Kind::List {
            previous
                .filter(|(kind, range)| {
                    *kind == Kind::Prose
                        && input.raw[range.end() as usize..block.span.start() as usize]
                            .trim()
                            .is_empty()
                })
                .map(|(_, range)| range)
        } else {
            None
        };
        let headings = chain.iter().map(|(_, range)| *range).collect::<Vec<_>>();
        let id = Blake3Hash::digest(encoded(&(
            RECIPE,
            &input.owner,
            block.span,
            &headings,
            intro,
        ))?);
        records.push(Record {
            id,
            unit: block.span,
            headings,
            intro,
        });
        meter.usage.records += 1;
        require(
            records.len() <= OWNER_ROWS,
            ErrorCode::BudgetExceeded,
            "025 complete per-owner128-record cap",
        )?;
        previous = Some((block.kind, block.span));
    }
    let oversized_closures = records
        .iter()
        .filter(|record| {
            record.closure().len() > 4 || record.closure().iter().any(|range| range.len() > 1024)
        })
        .count();
    let sidecar = Sidecar {
        recipe: RECIPE.into(),
        owner: input.owner.clone(),
        parser_starts: meter.usage.parser_starts - before,
        records,
        oversized_closures,
    };
    let bytes = encoded(&sidecar)?;
    meter.usage.record_bytes += bytes.len();
    require(
        bytes.len() <= OWNER_RECORD_BYTES && meter.usage.record_bytes <= PREP_RECORD_BYTES,
        ErrorCode::BudgetExceeded,
        "025 complete serialized sidecar coverage cap",
    )?;
    meter.usage.oversized_closures += oversized_closures;
    Ok(sidecar)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Discovery {
    snapshot: ReadSnapshot,
    publication: Option<String>,
    fingerprint: Blake3Hash,
    owners: Vec<Owner>,
}
pub(crate) struct Capture {
    pub discovery: Discovery,
    inputs: Vec<CompilerInput>,
}
impl Capture {
    pub(crate) fn metadata(&self) -> serde_json::Value {
        serde_json::json!({"discovery":{"snapshot":self.discovery.snapshot,"publication":self.discovery.publication,
            "fingerprint":self.discovery.fingerprint,"owners":self.discovery.owners.iter().map(|owner| serde_json::json!({
                "source":owner.source,"revision":owner.revision,"path":owner.path,"hash":owner.hash,
                "title_hash":Blake3Hash::digest(owner.title.as_bytes()),"title_bytes":owner.title.len()})).collect::<Vec<_>>()},
            "complete_source_bytes":self.inputs.iter().map(|input| input.raw.len()).sum::<usize>()})
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    owner: Owner,
    path: PathBuf,
    bytes: usize,
    hash: Blake3Hash,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Prepared {
    recipe: String,
    owners: Vec<Stored>,
}
enum Active {
    Capture(Option<Capture>),
    Query(Prepared, Discovery),
}
thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    // Query-local expanded text, separately reported from serialized sidecar bytes.
    // Retained after observer cleanup so failed attempts preserve measured work.
    static EXPANDED_FTS_BYTES: Cell<usize> = const { Cell::new(0) };
}
pub(crate) fn expanded_fts_bytes() -> usize {
    EXPANDED_FTS_BYTES.with(Cell::get)
}
struct Clear;
impl Drop for Clear {
    fn drop(&mut self) {
        ACTIVE.with(|state| {
            state.borrow_mut().take();
        });
    }
}
pub(crate) fn is_active() -> bool {
    ACTIVE.with(|state| state.borrow().is_some())
}
pub(crate) fn discover_only(f: impl FnOnce() -> Result<ContextResult>) -> Result<Capture> {
    require(!is_active(), ErrorCode::Usage, "025 cannot nest observer")?;
    ACTIVE.with(|state| *state.borrow_mut() = Some(Active::Capture(None)));
    let _clear = Clear;
    let result = f()?;
    require(
        !result.network_used,
        ErrorCode::OfflineUnavailable,
        "025 discovery used network",
    )?;
    ACTIVE.with(|state| match state.borrow_mut().as_mut() {
        Some(Active::Capture(capture)) => capture.take().ok_or_else(|| {
            err(
                ErrorCode::CapabilityUnavailable,
                "025 normalized discovery hook absent",
            )
        }),
        _ => Err(err(ErrorCode::Internal, "025 observer state")),
    })
}
pub(crate) fn query(
    prepared: &Prepared,
    binding: &Discovery,
    f: impl FnOnce() -> Result<ContextResult>,
) -> Result<ContextResult> {
    require(!is_active(), ErrorCode::Usage, "025 cannot nest observer")?;
    EXPANDED_FTS_BYTES.with(|count| count.set(0));
    ACTIVE
        .with(|state| *state.borrow_mut() = Some(Active::Query(prepared.clone(), binding.clone())));
    let _clear = Clear;
    let result = f()?;
    require(
        !result.network_used,
        ErrorCode::OfflineUnavailable,
        "025 query used network",
    )?;
    Ok(result)
}
pub(crate) fn prepare(
    captures: &[Capture],
    directory: &Path,
) -> (Result<Prepared>, PreparationUsage) {
    let mut meter = PreparationMeter::new();
    let outcome = (|| -> Result<Prepared> {
        require(
            captures.len() == 6 || cfg!(test) && captures.len() == 1,
            ErrorCode::Usage,
            "025 complete capture set required",
        )?;
        require(
            !directory.exists(),
            ErrorCode::Usage,
            "025 no overwrite/retry sidecars",
        )?;
        fs::create_dir(directory).map_err(|e| WikiError::invalid(e.to_string()))?;
        let mut unique = BTreeMap::<(RecordId, RecordId), &CompilerInput>::new();
        for capture in captures {
            for input in &capture.inputs {
                if let Some(previous) = unique.insert(input.owner.key(), input) {
                    require(
                        previous.owner == input.owner && previous.raw == input.raw,
                        ErrorCode::FreshnessConflict,
                        "025 capture revision changed",
                    )?;
                }
            }
        }
        let mut stored = Vec::new();
        for input in unique.values() {
            let sidecar = compile(input, &mut meter)?;
            let bytes = encoded(&sidecar)?;
            let path = directory.join(format!(
                "{}.json",
                Blake3Hash::digest(encoded(&input.owner)?)
                    .as_str()
                    .trim_start_matches("blake3:")
            ));
            write(&path, &bytes)?;
            stored.push(Stored {
                owner: input.owner.clone(),
                path,
                bytes: bytes.len(),
                hash: Blake3Hash::digest(&bytes),
            });
        }
        meter.check()?;
        let prepared = Prepared {
            recipe: RECIPE.into(),
            owners: stored,
        };
        let manifest = encoded(&prepared)?;
        meter.usage.record_bytes += manifest.len();
        require(
            meter.usage.record_bytes <= PREP_RECORD_BYTES,
            ErrorCode::BudgetExceeded,
            "025 aggregate sidecar+manifest cap",
        )?;
        write(&directory.join("manifest.json"), &manifest)?;
        Ok(prepared)
    })();
    meter.usage.seconds = meter.start.elapsed().as_secs_f64();
    (outcome, meter.usage)
}

fn identities(reader: &dyn QueryCatalog, hits: &HitSet) -> Result<(Discovery, Vec<DocumentRow>)> {
    let mut owners = Vec::new();
    let mut documents = Vec::new();
    let mut keys = BTreeSet::new();
    let mut bytes = 0usize;
    for hit in &hits.hits {
        reader.check_query_budget()?;
        let document = reader.document(&hit.locator.path)?.ok_or_else(|| {
            err(
                ErrorCode::FreshnessConflict,
                "025 authenticated owner missing",
            )
        })?;
        let owner = Owner::of(&document)?;
        require(
            keys.insert(owner.key())
                && owner.path == hit.locator.path
                && owner.hash == hit.locator.observed_hash,
            ErrorCode::FreshnessConflict,
            "025 duplicate/inconsistent admitted owner",
        )?;
        bytes += document.raw_text.len();
        require(
            document.raw_text.len() <= OWNER_BYTES && bytes <= 4 * MIB,
            ErrorCode::BudgetExceeded,
            "025 complete query owner source-work cap",
        )?;
        owners.push(owner);
        documents.push(document);
    }
    Ok((
        Discovery {
            snapshot: reader.snapshot().clone(),
            publication: reader.publication_id().map(str::to_owned),
            fingerprint: reader.dependency_fingerprint()?,
            owners,
        },
        documents,
    ))
}
fn validate(sidecar: &Sidecar, document: &DocumentRow, expected: &Owner) -> Result<()> {
    require(
        sidecar.recipe == RECIPE
            && &sidecar.owner == expected
            && Owner::of(document)? == *expected
            && Blake3Hash::digest(document.raw_text.as_bytes()) == expected.hash
            && sidecar.records.len() <= OWNER_ROWS,
        ErrorCode::FreshnessConflict,
        "025 stale/untrusted sidecar identity or recipe",
    )?;
    for record in &sidecar.records {
        for range in record.closure() {
            range.slice(&document.raw_text)?;
        }
        require(
            !record.unit.is_empty()
                && record.id
                    == Blake3Hash::digest(encoded(&(
                        RECIPE,
                        expected,
                        record.unit,
                        &record.headings,
                        record.intro,
                    ))?),
            ErrorCode::FreshnessConflict,
            "025 record is not exact authenticated source/title metadata",
        )?;
    }
    Ok(())
}
struct Columns<'a> {
    title: &'a str,
    ancestors: String,
    intro: &'a str,
    unit: &'a str,
}
fn columns<'a>(
    record: &Record,
    document: &'a DocumentRow,
    owner: &'a Owner,
) -> Result<Columns<'a>> {
    let ancestors = record
        .headings
        .iter()
        .map(|range| range.slice(&document.raw_text))
        .collect::<Result<Vec<_>>>()?
        .join("\n");
    Ok(Columns {
        title: &owner.title,
        ancestors,
        intro: record
            .intro
            .map(|range| range.slice(&document.raw_text))
            .transpose()?
            .unwrap_or(""),
        unit: record.unit.slice(&document.raw_text)?,
    })
}
fn load(reader: &QuerySnapshot, stored: &Stored, document: &DocumentRow) -> Result<Sidecar> {
    let info = fs::symlink_metadata(&stored.path).map_err(|_| {
        err(
            ErrorCode::OfflineUnavailable,
            "025 missing prepared owner; no fallback/provider",
        )
    })?;
    require(
        info.is_file() && info.len() == stored.bytes as u64 && stored.bytes <= OWNER_RECORD_BYTES,
        ErrorCode::FreshnessConflict,
        "025 sidecar path/byte cap",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(&stored.path)
        .and_then(|file| {
            file.take((OWNER_RECORD_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| WikiError::invalid(e.to_string()))?;
    require(
        bytes.len() == stored.bytes && Blake3Hash::digest(&bytes) == stored.hash,
        ErrorCode::FreshnessConflict,
        "025 sidecar serialization hash changed",
    )?;
    // Charge exact sidecar bytes to the existing reader before deserialization.
    let text = std::str::from_utf8(&bytes).map_err(|e| WikiError::invalid(e.to_string()))?;
    reader
        .connection()
        .query_row("SELECT ?1", [text], |row| {
            Ok(reader.reserve_experimental_scalar_row(row, 1))
        })
        .map_err(|e| WikiError::invalid(e.to_string()))??;
    let sidecar: Sidecar =
        serde_json::from_slice(&bytes).map_err(|e| WikiError::invalid(e.to_string()))?;
    validate(&sidecar, document, &stored.owner)?;
    Ok(sidecar)
}

pub(crate) struct SupportTrial {
    pub(crate) passages: Vec<ContextPassage>,
    pub(crate) text: String,
    pub(crate) reason: Option<&'static str>,
}

pub(crate) fn exact_trial(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    current: &[ContextPassage],
    additions: &[ContextPassage],
) -> Result<SupportTrial> {
    let mut next = current.to_vec();
    for passage in additions {
        if !next
            .iter()
            .any(|old| old.locator == passage.locator && old.span == passage.span)
        {
            next.push(passage.clone());
        }
    }
    let mut counts = BTreeMap::new();
    for passage in &next {
        *counts.entry(bundles::owner(passage)).or_insert(0usize) += 1;
    }
    let (text, _) = render(reader, request.scope, &next, &[], &[])?;
    let bytes = request
        .budget
        .max_bytes
        .checked_sub(request.budget.instruction_bytes)
        .and_then(|n| n.checked_sub(request.budget.output_bytes))
        .ok_or_else(|| WikiError::invalid("025 byte reservation"))?;
    let tokens = request
        .budget
        .max_tokens
        .checked_sub(request.budget.instruction_tokens)
        .and_then(|n| n.checked_sub(request.budget.output_tokens))
        .ok_or_else(|| WikiError::invalid("025 token reservation"))?;
    let reason = if next
        .iter()
        .any(|p| p.text.len() > request.documents.limits.excerpt_bytes)
    {
        Some("support_passage_exceeds1024")
    } else if counts.values().any(|n| *n > 4) {
        Some("support_closure_owner_passage_cap")
    } else if text.len() > bytes || text.len().div_ceil(4) > tokens {
        Some("support_closure_render_budget")
    } else {
        None
    };
    Ok(SupportTrial {
        passages: next,
        text,
        reason,
    })
}
fn empty_draft(reader: &dyn QueryCatalog, request: &ContextRequest) -> Result<ContextDraft> {
    let trial = exact_trial(reader, request, &[], &[])?;
    require(
        trial.reason.is_none(),
        ErrorCode::BudgetExceeded,
        "025 empty header exceeds render budget",
    )?;
    let text = trial.text;
    Ok(ContextDraft { usage:ContextUsage { rendered_bytes:text.len(),estimated_tokens:text.len().div_ceil(4),token_accounting:TokenAccounting::EstimatedUtf8BytesDiv4Ceil,
        reserved_bytes:request.budget.instruction_bytes+request.budget.output_bytes,reserved_tokens:request.budget.instruction_tokens+request.budget.output_tokens,
        graph_bytes:0,graph_estimated_tokens:0,verification_bytes:0,verification_files:0,verification_entries:0 },text,passages:vec![],bundles:vec![],omissions:vec![],
        snapshot:reader.snapshot().clone(),dependency_fingerprint:reader.dependency_fingerprint()?,truncated:false,
        warnings:vec!["experimental workflow structural_representation025; exact AST ancestry/support closure; no completeness claim".into()],selection_packet:None })
}

pub(crate) fn assemble(
    reader: &QuerySnapshot,
    selected: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &HitSet,
    query: &str,
) -> Result<ContextDraft> {
    require(
        request.scope == ContextScope::IndexedDocuments
            && request.target == ContextTarget::Documents
            && request.graph.is_none()
            && request.documents.mode == SearchMode::Hybrid
            && request.documents.limits.hits <= 10
            && request.documents.limits.candidates <= 80
            && request.documents.limits.excerpt_bytes == 1024
            && request.budget.max_bytes <= 12000
            && request.budget.max_tokens <= 3000,
        ErrorCode::Usage,
        "025 frozen document-only hybrid request required",
    )?;
    let (discovery, documents) = identities(selected, hits)?;
    ACTIVE.with(|state| -> Result<ContextDraft> {
        let mut active=state.borrow_mut();
        match active.as_mut() {
            Some(Active::Capture(slot)) => {
                require(slot.is_none(),ErrorCode::Usage,"025 one discovery call per capture")?;
                *slot=Some(Capture { inputs:documents.iter().zip(&discovery.owners).map(|(document,owner)| CompilerInput { owner:owner.clone(),raw:document.raw_text.clone() }).collect(),discovery });
                empty_draft(selected,request)
            }
            Some(Active::Query(prepared,binding)) => {
                require(&discovery==binding && prepared.recipe==RECIPE,ErrorCode::FreshnessConflict,"025 snapshot or ordered owner set changed")?;
                let mut records=Vec::<(usize,Record)>::new();
                for (owner_index,(owner,document)) in discovery.owners.iter().zip(&documents).enumerate() {
                    selected.check_query_budget()?;
                    let stored=prepared.owners.iter().find(|stored| &stored.owner==owner)
                        .ok_or_else(|| err(ErrorCode::OfflineUnavailable,"025 missing prepared owner; no fallback/provider"))?;
                    let sidecar=load(reader,stored,document)?;
                    for record in sidecar.records { records.push((owner_index,record)); }
                }
                records.sort_by(|(a,x),(b,y)| discovery.owners[*a].source.cmp(&discovery.owners[*b].source)
                    .then(discovery.owners[*a].revision.cmp(&discovery.owners[*b].revision))
                    .then(x.unit.start().cmp(&y.unit.start())).then(x.unit.end().cmp(&y.unit.end())).then(x.id.cmp(&y.id)));
                let connection=reader.connection();
                // TEMP uses this same connection's cumulative SQL VM/deadline limits.
                connection.execute_batch("CREATE VIRTUAL TABLE temp.representation025_fts USING fts5(title,ancestors,intro,unit,tokenize='unicode61 remove_diacritics 2');")
                    .map_err(|e| WikiError::invalid(e.to_string()))?;
                for (index,(owner_index,record)) in records.iter().enumerate() {
                    selected.check_query_budget()?;
                    let owner=&discovery.owners[*owner_index];
                    let text=columns(record,&documents[*owner_index],owner)?;
                    let expanded=text.title.len()+text.ancestors.len()+text.intro.len()+text.unit.len();
                    EXPANDED_FTS_BYTES.with(|count| -> Result<()> {
                        let total=count.get().checked_add(expanded).ok_or_else(|| err(ErrorCode::BudgetExceeded,"025 expanded FTS byte counter overflow"))?;
                        count.set(total); Ok(())
                    })?;
                    // Charge the four actual FTS text fields to the shared cache-read
                    // byte/row budget. Title/intro/unit borrow authenticated originals;
                    // only the bounded heading separator string materializes separately.
                    connection.query_row("SELECT ?1,?2,?3,?4",params![text.title,text.ancestors,text.intro,text.unit],
                        |row| Ok(reader.reserve_experimental_scalar_row(row,4))).map_err(|e| WikiError::invalid(e.to_string()))??;
                    connection.execute("INSERT INTO temp.representation025_fts(rowid,title,ancestors,intro,unit) VALUES(?1,?2,?3,?4,?5)",
                        params![index as i64+1,text.title,text.ancestors,text.intro,text.unit]).map_err(|e| WikiError::invalid(e.to_string()))?;
                }
                let terms=super::super::excerpts::Tokenizer::new(connection)?.tokens(query)?.into_iter().map(|token| token.text).collect::<BTreeSet<_>>();
                require(terms.len()<=super::super::types::MAX_CONTEXT_QUERY_TERMS,ErrorCode::BudgetExceeded,"025 complete normalized query-term cap")?;
                let expression=terms.into_iter().map(|term| format!("\"{}\"",term.replace('"',"\"\""))).collect::<Vec<_>>().join(" OR ");
                let mut ranked=Vec::new();
                if !expression.is_empty() {
                    let mut statement=connection.prepare("SELECT rowid,bm25(representation025_fts,1.0,1.0,1.0,1.0) FROM temp.representation025_fts WHERE representation025_fts MATCH ?1 ORDER BY 2 ASC,rowid ASC LIMIT 80")
                        .map_err(|e| WikiError::invalid(e.to_string()))?;
                    let mut rows=statement.query([expression]).map_err(|e| WikiError::invalid(e.to_string()))?;
                    while let Some(row)=rows.next().map_err(|e| WikiError::invalid(e.to_string()))? {
                        reader.reserve_experimental_scalar_row(row,2)?;
                        let id = usize::try_from(row.get::<_, i64>(0).map_err(|e| WikiError::invalid(e.to_string()))?).map_err(|_| err(ErrorCode::IndexCorrupt, "025 invalid sparse row ID"))?;
                        let score:f64=row.get(1).map_err(|e| WikiError::invalid(e.to_string()))?;
                        require(id>0 && id<=records.len() && score.is_finite(),ErrorCode::IndexCorrupt,"025 invalid sparse rank")?;
                        ranked.push((id-1,score));
                    }
                }
                let mut draft=empty_draft(selected,request)?;
                for (ordinal,(index,score)) in ranked.iter().enumerate() {
                    selected.check_query_budget()?;
                    let (owner_index,record)=&records[*index]; let hit=&hits.hits[*owner_index];
                    let closure=record.closure();
                    let reason=if closure.len()>4 || closure.iter().any(|range| range.len()>1024) { Some("support_closure_oversized") } else { None };
                    if let Some(reason)=reason {
                        draft.omissions.push(ContextOmission { record_id:hit.owner_revision.clone(),path:Some(hit.locator.path.clone()),reason:format!("{reason}; record {}",record.id),count:1 }); continue;
                    }
                    let mut additions=Vec::new();
                    for range in closure {
                        let mut passage=passage_for_span_for_test(selected,request,hit,range,ordinal+1)?.ok_or_else(|| err(ErrorCode::FreshnessConflict,"025 support passage ineligible"))?;
                        passage.rank_contributions=vec![RankContribution { channel:"structural_representation025_bm25".into(),rank:ordinal+1,score:Some(*score) }];
                        additions.push(passage);
                    }
                    let trial=exact_trial(selected,request,&draft.passages,&additions)?;
                    if let Some(reason)=trial.reason { draft.omissions.push(ContextOmission { record_id:hit.owner_revision.clone(),path:Some(hit.locator.path.clone()),reason:format!("{reason}; record {}",record.id),count:1 }); }
                    else { draft.passages=trial.passages; draft.text=trial.text; }
                }
                if ranked.len()==80 { draft.omissions.push(ContextOmission { record_id:None,path:None,reason:"ranked_record80_cap; further_match_count_unavailable".into(),count:1 }); }
                if hits.truncated || hits.omitted_candidates>0 { draft.omissions.push(ContextOmission { record_id:None,path:None,reason:"unchanged_hybrid_discovery_is_bounded".into(),count:hits.omitted_candidates.max(1) }); }
                draft.usage.rendered_bytes=draft.text.len(); draft.usage.estimated_tokens=draft.text.len().div_ceil(4);
                draft.truncated=!draft.omissions.is_empty();
                draft.warnings.push(format!("025 {} contextual records loaded; {} ranked once; {} exact support passages; omissions are explicit coverage gaps",records.len(),ranked.len(),draft.passages.len()));
                selected.check_query_budget()?; Ok(draft)
            }
            None => Err(err(ErrorCode::Usage,"025 explicit test-only preparation required")),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(raw: &str) -> CompilerInput {
        CompilerInput {
            owner: Owner {
                source: RecordId::new("018f0000-0000-7000-8000-000000000001").unwrap(),
                revision: RecordId::new("018f0000-0000-7000-8000-000000000002").unwrap(),
                path: VaultRelativePath::new("sources/control/content.md").unwrap(),
                hash: Blake3Hash::digest(raw.as_bytes()),
                title: "Source title".into(),
            },
            raw: raw.into(),
        }
    }
    fn document(input: &CompilerInput) -> DocumentRow {
        DocumentRow {
            path: input.owner.path.clone(),
            hash: input.owner.hash.clone(),
            record_id: None,
            kind: None,
            title: input.owner.title.clone(),
            aliases: vec![],
            headings: String::new(),
            tags: vec![],
            body: input.raw.clone(),
            raw_text: input.raw.clone(),
            source_id: Some(input.owner.source.clone()),
            owner_revision: Some(input.owner.revision.clone()),
            eligibility: Eligibility::Current,
            reasons: vec![],
        }
    }
    #[test]
    fn serialized_span_graph_derives_byte_identical_fixed_fts_columns_and_invalidates_revision() {
        let input = input(
            "# Scope\n\n## Route\n\nBoth are needed:\n\n- first\n- second\n\nOther café 東京 🦀 prose.\n",
        );
        let sidecar = compile(&input, &mut PreparationMeter::new()).unwrap();
        let bytes = encoded(&sidecar).unwrap();
        let restored: Sidecar = serde_json::from_slice(&bytes).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("unit_text"));
        let document = document(&input);
        validate(&restored, &document, &input.owner).unwrap();
        let list = restored
            .records
            .iter()
            .find(|record| record.intro.is_some())
            .unwrap();
        let derived = columns(list, &document, &input.owner).unwrap();
        let formerly_serialized = [
            "Source title",
            "# Scope\n\n## Route\n",
            "Both are needed:\n",
            "- first\n- second\n\n",
        ];
        assert_eq!(
            [
                derived.title,
                derived.ancestors.as_str(),
                derived.intro,
                derived.unit
            ],
            formerly_serialized
        );
        // Compare fixed former serialized strings against authenticated span-derived
        // columns with the actual frozen FTS5 tokenizer, weights, rank and tie order.
        let frozen_rows = [
            [
                "Source title",
                "# Scope\n\n## Route\n",
                "",
                "Both are needed:\n",
            ],
            formerly_serialized,
            [
                "Source title",
                "# Scope\n\n## Route\n",
                "",
                "Other café 東京 🦀 prose.\n",
            ],
        ];
        assert_eq!(restored.records.len(), frozen_rows.len());
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        for name in ["serialized", "derived"] {
            connection.execute_batch(&format!("CREATE VIRTUAL TABLE {name} USING fts5(title,ancestors,intro,unit,tokenize='unicode61 remove_diacritics 2');")).unwrap();
        }
        for (index, (record, frozen)) in restored.records.iter().zip(frozen_rows).enumerate() {
            let text = columns(record, &document, &input.owner).unwrap();
            let actual = [text.title, text.ancestors.as_str(), text.intro, text.unit];
            assert_eq!(actual, frozen);
            for (name, fields) in [("serialized", frozen), ("derived", actual)] {
                connection.execute(&format!("INSERT INTO {name}(rowid,title,ancestors,intro,unit) VALUES(?1,?2,?3,?4,?5)"), params![index as i64+1, fields[0],fields[1],fields[2],fields[3]]).unwrap();
            }
        }
        let ranked = |name: &str, expression: &str| {
            let mut statement = connection.prepare(&format!("SELECT rowid,bm25({name},1.0,1.0,1.0,1.0) FROM {name} WHERE {name} MATCH ?1 ORDER BY 2 ASC,rowid ASC LIMIT 80")).unwrap();
            statement
                .query_map([expression], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        for expression in [
            "\"first\" OR \"needed\"",
            "\"cafe\" OR \"東京\"",
            "\"scope\"",
        ] {
            let expected = ranked("serialized", expression);
            assert!(!expected.is_empty());
            assert_eq!(ranked("derived", expression), expected);
        }
        assert_eq!(ranked("derived", "\"cafe\"")[0].0, 3);
        // UTF-8 source boundaries remain exact; fabricated middle-codepoint and
        // out-of-source spans refuse before any derived text can enter FTS.
        let mid = input.raw.find('é').unwrap() + 1;
        let invalid_utf8 = ByteSpan::new(mid as u64, (mid + 1) as u64).unwrap();
        let outside = ByteSpan::new(input.raw.len() as u64, input.raw.len() as u64 + 1).unwrap();
        for invalid in [invalid_utf8, outside] {
            let mut forged = restored.clone();
            forged.records[0].unit = invalid;
            assert!(validate(&forged, &document, &input.owner).is_err());
            assert!(columns(&forged.records[0], &document, &input.owner).is_err());
            let mut forged = restored.clone();
            forged.records[0].intro = Some(invalid);
            assert!(validate(&forged, &document, &input.owner).is_err());
            assert!(columns(&forged.records[0], &document, &input.owner).is_err());
            let mut forged = restored.clone();
            forged.records[0].headings.push(invalid);
            assert!(validate(&forged, &document, &input.owner).is_err());
            assert!(columns(&forged.records[0], &document, &input.owner).is_err());
        }
        let mut stale = document.clone();
        stale.raw_text.push_str("Changed revision.\n");
        stale.hash = Blake3Hash::digest(stale.raw_text.as_bytes());
        assert_eq!(
            validate(&restored, &stale, &input.owner).unwrap_err().code,
            ErrorCode::FreshnessConflict
        );
        let mut title_changed = document.clone();
        title_changed.title = "Untrusted replacement title".into();
        assert_eq!(
            validate(&restored, &title_changed, &input.owner)
                .unwrap_err()
                .code,
            ErrorCode::FreshnessConflict
        );
        let mut recipe_changed = restored.clone();
        recipe_changed.recipe = "other recipe".into();
        assert_eq!(
            validate(&recipe_changed, &document, &input.owner)
                .unwrap_err()
                .code,
            ErrorCode::FreshnessConflict
        );
    }
    #[test]
    fn source_only_heading_intro_and_whole_nested_list_support_are_exact() {
        let raw = "# Scope\n\n## Applicable route\n\nBoth requirements apply:\n\n- first condition\n- second condition\n  - nested exception\n\nOther prose.\n";
        let sidecar = compile(&input(raw), &mut PreparationMeter::new()).unwrap();
        let list = sidecar
            .records
            .iter()
            .find(|record| record.unit.slice(raw).unwrap().starts_with("- first"))
            .unwrap();
        assert_eq!(list.headings.len(), 2);
        assert_eq!(list.closure().len(), 4);
        let text = list.unit.slice(raw).unwrap();
        assert!(text.contains("second condition") && text.contains("nested exception"));
        assert_eq!(
            list.intro.unwrap().slice(raw).unwrap(),
            "Both requirements apply:\n"
        );
        assert!(
            sidecar
                .records
                .iter()
                .all(|record| !record.unit.slice(raw).unwrap().starts_with('#'))
        );
        for record in &sidecar.records {
            for range in record.closure() {
                assert_eq!(
                    range.slice(raw).unwrap(),
                    &raw[range.start() as usize..range.end() as usize]
                );
            }
        }
    }
    #[test]
    fn complete_preparation_refuses_bytes_starts_records_and_serialization_without_truncation() {
        let mut meter = PreparationMeter::new();
        meter.usage.parser_starts = PREP_STARTS;
        assert_eq!(
            compile(&input("ordinary paragraph"), &mut meter)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        let mut meter = PreparationMeter::new();
        meter.usage.source_bytes = PREP_BYTES;
        assert_eq!(
            compile(&input("ordinary paragraph"), &mut meter)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        let many = (0..129)
            .map(|n| format!("paragraph{n}\n\n"))
            .collect::<String>();
        assert_eq!(
            compile(&input(&many), &mut PreparationMeter::new())
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        let large = "x".repeat(OWNER_RECORD_BYTES + 1);
        let sidecar = compile(&input(&large), &mut PreparationMeter::new()).unwrap();
        assert_eq!(sidecar.records.len(), 1);
        assert_eq!(sidecar.oversized_closures, 1);
        let mut title_heavy = input("ordinary paragraph");
        title_heavy.owner.title = "x".repeat(OWNER_RECORD_BYTES);
        assert_eq!(
            compile(&title_heavy, &mut PreparationMeter::new())
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        let raw = format!("# Scope\n\n{}\n", "x".repeat(1025));
        let sidecar = compile(&input(&raw), &mut PreparationMeter::new()).unwrap();
        assert_eq!(sidecar.oversized_closures, 1);
        assert_eq!(sidecar.records[0].unit.slice(&raw).unwrap().len(), 1026);
    }
    #[test]
    fn immutable_source_and_recipe_keys_invalidate_stale_or_tampered_representations() {
        let mut original = input("# Scope\n\nOriginal entitlement.\n");
        let first = compile(&original, &mut PreparationMeter::new()).unwrap();
        original.raw.push_str("Changed revision.\n");
        assert_eq!(
            compile(&original, &mut PreparationMeter::new())
                .unwrap_err()
                .code,
            ErrorCode::FreshnessConflict
        );
        original.owner.hash = Blake3Hash::digest(original.raw.as_bytes());
        let next = compile(&original, &mut PreparationMeter::new()).unwrap();
        assert_ne!(first.owner.hash, next.owner.hash);
        assert_ne!(first.records[0].id, next.records[0].id);
    }
}
