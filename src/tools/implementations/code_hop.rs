//! Body-free sibling routing over the persistent repository source index.

use crate::source_index::{
    DirectoryChildKind, IndexedOutline, RepositoryCache, RepositoryIndexError, RepositoryIndexer,
    RepositorySnapshot, RetrievalMethod, RetrievalProvenance, RetrievalProvenanceClass,
    SourceIdentity, SourceResolver, SourceSpan,
};
use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::tools::Tool;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use finch_programs::ExecutionEffect;
use serde::Serialize;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const MAX_QUERY_BYTES: usize = 1_024;
const MAX_HOPS: usize = 32;
const MAX_MENU_CANDIDATES: usize = 2_000;
const MAX_DISAMBIGUATION_CANDIDATES: usize = 16;
const MAX_DISAMBIGUATION_BYTES: usize = 16 * 1024;
const MAX_DISAMBIGUATION_CALLS: usize = 8;
const DISAMBIGUATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SELECTED_PER_MENU: usize = 3;
const DEFAULT_RESULT_LIMIT: usize = 5;
const MAX_RETURNED_SPANS: usize = 10;
const MAX_WHY_BYTES: usize = 256;
const MAX_MECHANICAL_FILES: usize = 2_048;
const MAX_MECHANICAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_MECHANICAL_MATCHES: usize = 100;
const MAX_MECHANICAL_METADATA_BYTES: usize = 64 * 1024;
const EXACT_PATH_WINDOW_LINES: usize = 80;
const MAX_INDEX_WORK_BYTES: u64 = 256 * 1024 * 1024;
const INDEX_WORK_TIMEOUT: Duration = Duration::from_secs(20);
const CONFIDENT_SCORE_FLOOR: f64 = 1.0;
const CONFIDENT_MARGIN: f64 = 0.75;

/// Optional local-only chooser. Production roots intentionally leave this unbound
/// until the dedicated local `compress` lane exists.
#[async_trait]
trait CodeHopDisambiguator: Send + Sync {
    async fn choose(&self, request: &DisambiguationRequest) -> Result<Vec<String>>;
}

#[derive(Debug, Clone, Serialize)]
struct DisambiguationRequest {
    query: String,
    candidates: Vec<DisambiguationCandidate>,
}

#[derive(Debug, Clone, Serialize)]
struct DisambiguationCandidate {
    id: String,
    name: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lead: Option<String>,
}

#[derive(Debug, Clone)]
struct RankedCandidate {
    id: String,
    name: String,
    kind: String,
    target: String,
    lead: Option<String>,
    score: f64,
    ranking_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
struct CodeHopSpan {
    path: String,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
    symbol: Option<String>,
    symbol_kind: Option<String>,
    match_kind: &'static str,
    why: String,
}

#[derive(Debug, Serialize)]
struct FindCodeMatch(String, usize, usize, Option<String>, &'static str);

#[derive(Debug, Serialize)]
struct FindCodeResponse {
    matches: Vec<FindCodeMatch>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchKind {
    Function,
    Type,
    Module,
    File,
}

#[derive(Debug, Clone)]
struct SearchOptions {
    path: Option<String>,
    kind: Option<SearchKind>,
    limit: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            path: None,
            kind: None,
            limit: DEFAULT_RESULT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct HopStep {
    directory: String,
    picks: Vec<String>,
    method: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct CodeHopMetrics {
    menus_considered: usize,
    candidates_considered: usize,
    routing_context_bytes: usize,
    ranker_input_bytes: usize,
    disambiguation_input_bytes: usize,
    mechanical_files_examined: usize,
    mechanical_source_bytes_examined: usize,
    selected_files: usize,
    body_bytes_disclosed: usize,
    disambiguation_calls: usize,
}

#[derive(Debug, Clone, Serialize)]
struct CodeHopResult {
    spans: Vec<CodeHopSpan>,
    hop_path: Vec<HopStep>,
    provenance: RetrievalProvenance,
    warnings: Vec<String>,
    metrics: CodeHopMetrics,
}

struct RouteAttempt {
    result: CodeHopResult,
    selected: Vec<(SourceIdentity, SourceSpan)>,
}

/// Routes a question to a bounded set of exact source spans without returning bodies.
#[derive(Clone)]
pub struct FindCodeTool {
    workspace_root: PathBuf,
    state_directory: PathBuf,
    disambiguator: Option<Arc<dyn CodeHopDisambiguator>>,
    #[cfg(test)]
    disambiguation_timeout: Duration,
}

impl FindCodeTool {
    pub fn new(workspace_root: impl Into<PathBuf>, state_directory: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            workspace_root: finch_tools_api::resolve_workspace_root(&workspace_root),
            state_directory: state_directory.into(),
            disambiguator: None,
            #[cfg(test)]
            disambiguation_timeout: DISAMBIGUATION_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_disambiguator(mut self, disambiguator: Arc<dyn CodeHopDisambiguator>) -> Self {
        self.disambiguator = Some(disambiguator);
        self
    }

    #[cfg(test)]
    fn with_disambiguation_timeout(mut self, timeout: Duration) -> Self {
        self.disambiguation_timeout = timeout;
        self
    }

    async fn snapshot(&self, force_rebuild: bool) -> Result<(SourceResolver, RepositorySnapshot)> {
        let workspace_root = self.workspace_root.clone();
        let state_directory = self.state_directory.clone();
        let task = tokio::task::spawn_blocking(move || {
            Self::snapshot_blocking(workspace_root, state_directory, force_rebuild)
        });
        tokio::time::timeout(INDEX_WORK_TIMEOUT + Duration::from_secs(1), task)
            .await
            .context("source-index work exceeded the find_code deadline")?
            .context("source-index worker task failed")?
    }

    fn snapshot_blocking(
        workspace_root: PathBuf,
        state_directory: PathBuf,
        force_rebuild: bool,
    ) -> Result<(SourceResolver, RepositorySnapshot)> {
        let deadline = std::time::Instant::now() + INDEX_WORK_TIMEOUT;
        let resolver = SourceResolver::new(&workspace_root)?;
        let cache = RepositoryCache::prepare_bounded(&state_directory, &resolver, deadline)?;
        let cached = match cache.load_with_deadline(deadline) {
            Ok(cached) => cached,
            Err(RepositoryIndexError::IncompatibleCache { .. }) => {
                let indexer = RepositoryIndexer::new(resolver, cache)?;
                let snapshot = indexer
                    .rebuild_bounded(deadline, MAX_INDEX_WORK_BYTES)?
                    .snapshot;
                return Ok((SourceResolver::new(&workspace_root)?, snapshot));
            }
            Err(error) => return Err(error.into()),
        };
        let indexer = RepositoryIndexer::new(resolver, cache)?;
        let snapshot = if force_rebuild {
            indexer
                .rebuild_bounded(deadline, MAX_INDEX_WORK_BYTES)?
                .snapshot
        } else if let Some(snapshot) = cached {
            if indexer.is_snapshot_current_with_budget(&snapshot, deadline, MAX_INDEX_WORK_BYTES)? {
                snapshot
            } else {
                indexer
                    .build_bounded(deadline, MAX_INDEX_WORK_BYTES)?
                    .snapshot
            }
        } else {
            indexer
                .build_bounded(deadline, MAX_INDEX_WORK_BYTES)?
                .snapshot
        };
        Ok((SourceResolver::new(&workspace_root)?, snapshot))
    }

    #[cfg(test)]
    async fn execute_query(&self, query: &str) -> Result<CodeHopResult> {
        self.execute_search(query, SearchOptions::default()).await
    }

    async fn execute_search(&self, query: &str, options: SearchOptions) -> Result<CodeHopResult> {
        for attempt in 0..2 {
            let (resolver, snapshot) = self.snapshot(attempt == 1).await?;
            let routed = self
                .route_off_thread(query.to_string(), snapshot, options.clone())
                .await?;
            let mut stale = false;
            for (source, span) in &routed.selected {
                if resolver.read_span(source, span).is_err() {
                    stale = true;
                    break;
                }
            }
            if !stale {
                return Ok(routed.result);
            }
        }
        bail!("source generation changed during both find_code routing attempts")
    }

    async fn route_off_thread(
        &self,
        query: String,
        snapshot: RepositorySnapshot,
        options: SearchOptions,
    ) -> Result<RouteAttempt> {
        let tool = self.clone();
        let workspace_root = self.workspace_root.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let resolver = SourceResolver::new(workspace_root)?;
            let snapshot = constrain_snapshot(snapshot, &options)?;
            let require_structural_match =
                options.kind.is_some_and(|kind| kind != SearchKind::File);
            let mut routed = runtime.block_on(tool.route(
                &query,
                &resolver,
                &snapshot,
                require_structural_match,
            ))?;
            apply_result_options(&mut routed, &options)?;
            Ok(routed)
        })
        .await
        .context("find_code routing worker failed")?
    }

    async fn route(
        &self,
        query: &str,
        resolver: &SourceResolver,
        snapshot: &RepositorySnapshot,
        require_structural_match: bool,
    ) -> Result<RouteAttempt> {
        let (mechanical, partial_warning) =
            mechanical_route(query, resolver, snapshot, require_structural_match)?;
        if partial_warning.is_none() {
            if let Some(mechanical) = mechanical {
                return Ok(mechanical);
            }
        }
        let sibling = self.sibling_route(query, resolver, snapshot).await;
        match (mechanical, sibling) {
            (Some(mut mechanical), Ok(sibling)) => {
                merge_route_attempts(&mut mechanical, sibling);
                if let Some(warning) = partial_warning {
                    push_warning_once(&mut mechanical.result.warnings, &warning);
                }
                Ok(mechanical)
            }
            (Some(mut mechanical), Err(_)) => {
                if let Some(warning) = partial_warning {
                    push_warning_once(&mut mechanical.result.warnings, &warning);
                }
                Ok(mechanical)
            }
            (None, Ok(mut sibling)) => {
                if let Some(warning) = partial_warning {
                    push_warning_once(&mut sibling.result.warnings, &warning);
                }
                Ok(sibling)
            }
            (None, Err(error)) => Err(error),
        }
    }

    async fn sibling_route(
        &self,
        query: &str,
        resolver: &SourceResolver,
        snapshot: &RepositorySnapshot,
    ) -> Result<RouteAttempt> {
        let mut queue = VecDeque::from([(String::new(), Vec::<HopStep>::new())]);
        let mut chosen_files = Vec::new();
        let mut warnings = Vec::new();
        let mut metrics = CodeHopMetrics::default();
        let mut used_model = false;
        let mut hops = 0usize;

        while let Some((directory_path, path_steps)) = queue.pop_front() {
            if chosen_files.len() >= MAX_RETURNED_SPANS || hops >= MAX_HOPS {
                break;
            }
            hops += 1;
            let Some(directory) = snapshot.directory(&directory_path) else {
                continue;
            };
            if directory.children.len() > MAX_MENU_CANDIDATES {
                warnings.push(format!(
                    "directory {directory_path:?} was partially routed at {MAX_MENU_CANDIDATES} children"
                ));
            }
            let candidates = directory
                .children
                .iter()
                .take(MAX_MENU_CANDIDATES)
                .enumerate()
                .map(|(index, child)| {
                    let target = join_path(&directory_path, &child.name);
                    let lead = (child.kind == DirectoryChildKind::Directory)
                        .then(|| snapshot.directory(&target))
                        .flatten()
                        .and_then(|record| record.agent_lead.as_ref())
                        .map(|lead| lead.text.clone());
                    let rank_text = subtree_rank_text(snapshot, &target, child.kind);
                    RankedCandidate {
                        id: format!("h{hops}c{index}"),
                        name: child.name.clone(),
                        kind: match child.kind {
                            DirectoryChildKind::Directory => "directory",
                            DirectoryChildKind::File => "file",
                        }
                        .to_string(),
                        target,
                        lead,
                        score: lexical_score(query, &rank_text),
                        ranking_bytes: serialized_ranker_input_bytes(query, &rank_text),
                    }
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                continue;
            }
            let selection = self
                .select_candidates(query, candidates, &mut warnings, &mut metrics)
                .await?;
            used_model |= selection.used_model;
            let picks = selection
                .candidates
                .iter()
                .map(|candidate| candidate.name.clone())
                .collect::<Vec<_>>();
            let mut next_steps = path_steps;
            next_steps.push(HopStep {
                directory: directory_path.clone(),
                picks,
                method: selection.method.to_string(),
            });
            for candidate in selection.candidates {
                if candidate.kind == "directory" {
                    queue.push_back((candidate.target, next_steps.clone()));
                } else if let Some(file) = snapshot.file(&candidate.target) {
                    chosen_files.push((file, next_steps.clone()));
                }
            }
        }

        if chosen_files.is_empty() {
            bail!("find_code could not route the query to an indexed file")
        }

        let mut spans = Vec::new();
        let mut selected = Vec::new();
        let mut final_hops = Vec::new();
        for (file, mut path_steps) in chosen_files {
            if spans.len() >= MAX_RETURNED_SPANS {
                break;
            }
            let records = rank_file_records(query, resolver, file)?;
            if records.is_empty() {
                continue;
            }
            let selection = self
                .select_candidates(query, records, &mut warnings, &mut metrics)
                .await?;
            used_model |= selection.used_model;
            path_steps.push(HopStep {
                directory: file.outline.source.path.clone(),
                picks: selection
                    .candidates
                    .iter()
                    .map(|candidate| candidate.name.clone())
                    .collect(),
                method: selection.method.to_string(),
            });
            for candidate in selection.candidates {
                let index = candidate
                    .target
                    .parse::<usize>()
                    .context("invalid internal outline candidate index")?;
                let record = &file.outline.records[index];
                let why = bounded_why(format!(
                    "{} match for {} ({})",
                    selection.method, record.name, record.kind
                ));
                spans.push(span_result(
                    &file.outline.source,
                    &record.span,
                    Some(record.name.clone()),
                    Some(record.kind.clone()),
                    if selection.used_model {
                        "inferred"
                    } else {
                        "lexical"
                    },
                    why,
                ));
                selected.push((file.outline.source.clone(), record.span.clone()));
                if spans.len() == MAX_RETURNED_SPANS {
                    break;
                }
            }
            final_hops.extend(path_steps);
        }
        metrics.selected_files = selected
            .iter()
            .map(|(source, _)| source.path.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        if spans.is_empty() {
            bail!("find_code found indexed files but no routable source spans")
        }
        Ok(RouteAttempt {
            result: CodeHopResult {
                spans,
                hop_path: final_hops,
                provenance: if used_model {
                    RetrievalProvenance {
                        class: RetrievalProvenanceClass::InferredModel,
                        method: RetrievalMethod::Model,
                    }
                } else {
                    RetrievalProvenance {
                        class: RetrievalProvenanceClass::LexicalGrep,
                        method: RetrievalMethod::Grep,
                    }
                },
                warnings,
                metrics,
            },
            selected,
        })
    }

    async fn select_candidates(
        &self,
        query: &str,
        mut candidates: Vec<RankedCandidate>,
        warnings: &mut Vec<String>,
        metrics: &mut CodeHopMetrics,
    ) -> Result<Selection> {
        candidates.sort_by(rank_order);
        metrics.menus_considered += 1;
        metrics.candidates_considered += candidates.len();
        let ranker_bytes = candidates
            .iter()
            .map(|candidate| candidate.ranking_bytes)
            .sum::<usize>();
        metrics.ranker_input_bytes += ranker_bytes;
        metrics.routing_context_bytes += ranker_bytes;
        let offered = candidates
            .iter()
            .take(MAX_DISAMBIGUATION_CANDIDATES)
            .cloned()
            .collect::<Vec<_>>();
        let confident = offered.len() == 1
            || (offered[0].score >= CONFIDENT_SCORE_FLOOR
                && offered[0].score - offered[1].score >= CONFIDENT_MARGIN);
        if confident {
            return Ok(Selection {
                candidates: vec![offered[0].clone()],
                method: "ranker",
                used_model: false,
            });
        }

        if let Some(disambiguator) = &self.disambiguator {
            let request = DisambiguationRequest {
                query: query.to_string(),
                candidates: offered
                    .iter()
                    .map(|candidate| DisambiguationCandidate {
                        id: candidate.id.clone(),
                        name: candidate.name.clone(),
                        kind: candidate.kind.clone(),
                        lead: candidate.lead.clone(),
                    })
                    .collect(),
            };
            let request_bytes = serde_json::to_vec(&request)?;
            if request_bytes.len() > MAX_DISAMBIGUATION_BYTES {
                bail!("find_code sibling payload exceeds {MAX_DISAMBIGUATION_BYTES} bytes")
            }
            if metrics.disambiguation_calls >= MAX_DISAMBIGUATION_CALLS {
                bail!("find_code exceeded {MAX_DISAMBIGUATION_CALLS} disambiguation calls")
            }
            metrics.disambiguation_input_bytes += request_bytes.len();
            metrics.routing_context_bytes += request_bytes.len();
            metrics.disambiguation_calls += 1;
            #[cfg(not(test))]
            let timeout = DISAMBIGUATION_TIMEOUT;
            #[cfg(test)]
            let timeout = self.disambiguation_timeout;
            let response = tokio::time::timeout(timeout, disambiguator.choose(&request)).await;
            match response {
                Ok(Ok(ids)) => {
                    if let Some(selected) = validate_disambiguation(&offered, &ids) {
                        return Ok(Selection {
                            candidates: selected,
                            method: "local_disambiguator",
                            used_model: true,
                        });
                    }
                    push_warning_once(
                        warnings,
                        "local disambiguator returned invalid candidate IDs; using ranked choices",
                    );
                }
                Ok(Err(_)) => {
                    push_warning_once(warnings, "local disambiguator failed; using ranked choices")
                }
                Err(_) => push_warning_once(
                    warnings,
                    "local disambiguator timed out; using ranked choices",
                ),
            }
        } else {
            push_warning_once(
                warnings,
                "local compress lane is unbound; using ranker-only choices",
            );
        }
        Ok(Selection {
            candidates: offered.into_iter().take(MAX_SELECTED_PER_MENU).collect(),
            method: "ranker_ambiguous",
            used_model: false,
        })
    }
}

struct Selection {
    candidates: Vec<RankedCandidate>,
    method: &'static str,
    used_model: bool,
}

#[async_trait]
impl Tool for FindCodeTool {
    fn name(&self) -> &str {
        "find_code"
    }

    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::WorkspaceRead
    }

    fn description(&self) -> &str {
        "Find code locations without returning file bodies. Query with a workspace path, identifier, quoted fixed string, or plain lexical terms; optional path, kind, and limit constrain results. Each match is [path,start_line,end_line,symbol,match_type]; read the returned range."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "query": {
                    "type": "string",
                    "description": "Workspace path, identifier, quoted fixed string, or plain lexical search terms"
                },
                "path": {
                    "type": "string",
                    "description": "Optional workspace-relative file or directory scope"
                },
                "kind": {
                    "type": "string",
                    "enum": ["function", "type", "module", "file"],
                    "description": "Optional structural result filter"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_RETURNED_SPANS,
                    "description": "Maximum matches; defaults to 5"
                }
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .context("Missing query parameter")?
            .trim();
        if query.is_empty() {
            bail!("find_code query must not be empty")
        }
        if query.len() > MAX_QUERY_BYTES {
            bail!("find_code query exceeds {MAX_QUERY_BYTES} UTF-8 bytes")
        }
        let options = parse_search_options(&input)?;
        let result = self.execute_search(query, options).await?;
        serde_json::to_string(&compact_response(result))
            .context("failed to serialize find_code result")
    }

    fn workspace_root(&self) -> Option<&Path> {
        Some(&self.workspace_root)
    }
}

fn parse_search_options(input: &Value) -> Result<SearchOptions> {
    let path = input
        .get("path")
        .map(|value| {
            value
                .as_str()
                .context("find_code path must be a string")
                .and_then(normalize_scope)
        })
        .transpose()?;
    let kind = input
        .get("kind")
        .map(|value| match value.as_str() {
            Some("function") => Ok(SearchKind::Function),
            Some("type") => Ok(SearchKind::Type),
            Some("module") => Ok(SearchKind::Module),
            Some("file") => Ok(SearchKind::File),
            _ => bail!("find_code kind must be function, type, module, or file"),
        })
        .transpose()?;
    let limit = input
        .get("limit")
        .map(|value| {
            let limit = value
                .as_u64()
                .context("find_code limit must be an integer")? as usize;
            if !(1..=MAX_RETURNED_SPANS).contains(&limit) {
                bail!("find_code limit must be between 1 and {MAX_RETURNED_SPANS}")
            }
            Ok(limit)
        })
        .transpose()?
        .unwrap_or(DEFAULT_RESULT_LIMIT);
    Ok(SearchOptions { path, kind, limit })
}

fn normalize_scope(value: &str) -> Result<String> {
    let value = value.trim().trim_start_matches("./").trim_end_matches('/');
    if value.is_empty() || value.len() > 4_096 {
        bail!("find_code path must be a nonempty workspace-relative path of at most 4096 bytes")
    }
    let mut parts = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .context("find_code path must be valid UTF-8")?
                    .to_string(),
            ),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("find_code path must stay within the workspace")
            }
        }
    }
    if parts.is_empty() {
        bail!("find_code path must name a workspace file or directory")
    }
    Ok(parts.join("/"))
}

fn constrain_snapshot(
    mut snapshot: RepositorySnapshot,
    options: &SearchOptions,
) -> Result<RepositorySnapshot> {
    if let Some(scope) = &options.path {
        let prefix = format!("{scope}/");
        snapshot.files.retain(|file| {
            file.outline.source.path == *scope || file.outline.source.path.starts_with(&prefix)
        });
        if snapshot.files.is_empty() {
            bail!("find_code path scope {scope:?} contains no indexed files")
        }
    }
    if let Some(kind) = options.kind.filter(|kind| *kind != SearchKind::File) {
        for file in &mut snapshot.files {
            file.outline
                .records
                .retain(|record| record_matches_kind(&record.kind, kind));
        }
        snapshot
            .files
            .retain(|file| !file.outline.records.is_empty());
        if snapshot.files.is_empty() {
            bail!("find_code found no indexed records of the requested kind")
        }
    }

    let allowed_files = snapshot
        .files
        .iter()
        .map(|file| file.outline.source.path.clone())
        .collect::<BTreeSet<_>>();
    let mut allowed_directories = BTreeSet::from([String::new()]);
    for path in &allowed_files {
        for directory in Path::new(path).ancestors().skip(1) {
            let directory = directory.to_str().unwrap_or_default().replace('\\', "/");
            allowed_directories.insert(directory);
        }
    }
    snapshot
        .directories
        .retain(|directory| allowed_directories.contains(&directory.path));
    for directory in &mut snapshot.directories {
        directory.children.retain(|child| {
            let target = join_path(&directory.path, &child.name);
            allowed_files.contains(&target) || allowed_directories.contains(&target)
        });
    }
    Ok(snapshot)
}

fn record_matches_kind(kind: &str, requested: SearchKind) -> bool {
    let kind = kind.to_ascii_lowercase();
    match requested {
        SearchKind::Function => kind.contains("function") || kind.contains("method"),
        SearchKind::Type => ["class", "struct", "enum", "interface", "trait", "type"]
            .iter()
            .any(|candidate| kind.contains(candidate)),
        SearchKind::Module => ["module", "namespace", "package"]
            .iter()
            .any(|candidate| kind.contains(candidate)),
        SearchKind::File => true,
    }
}

fn apply_result_options(attempt: &mut RouteAttempt, options: &SearchOptions) -> Result<()> {
    let mut seen_files = BTreeSet::new();
    let mut kept_spans = Vec::new();
    let mut kept_selected = Vec::new();
    for (mut span, selected) in attempt
        .result
        .spans
        .drain(..)
        .zip(attempt.selected.drain(..))
    {
        let keep = match options.kind {
            Some(SearchKind::File) => seen_files.insert(span.path.clone()),
            Some(kind) => span
                .symbol_kind
                .as_deref()
                .is_some_and(|actual| record_matches_kind(actual, kind)),
            None => true,
        };
        if keep {
            if options.kind == Some(SearchKind::File) {
                span.symbol = None;
                span.symbol_kind = None;
                span.match_kind = "file";
            }
            kept_spans.push(span);
            kept_selected.push(selected);
            if kept_spans.len() == options.limit {
                break;
            }
        }
    }
    if kept_spans.is_empty() {
        bail!("find_code found no matches satisfying the requested constraints")
    }
    attempt.result.spans = kept_spans;
    attempt.selected = kept_selected;
    attempt.result.metrics.selected_files = attempt
        .selected
        .iter()
        .map(|(source, _)| source.path.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    Ok(())
}

fn compact_response(result: CodeHopResult) -> FindCodeResponse {
    let matches = result
        .spans
        .into_iter()
        .map(|span| {
            FindCodeMatch(
                span.path,
                span.start_line,
                span.end_line,
                span.symbol,
                span.match_kind,
            )
        })
        .collect();
    let mut warnings = Vec::new();
    for warning in result.warnings {
        let compact = if warning.contains("scan bound") || warning.contains("partially routed") {
            "partial"
        } else if warning.contains("suffix matched multiple") {
            "ambiguous path"
        } else if warning.contains("unbound") {
            "ambiguous"
        } else if warning.contains("disambiguator") {
            "local disambiguation failed"
        } else {
            continue;
        };
        push_warning_once(&mut warnings, compact);
    }
    FindCodeResponse { matches, warnings }
}

fn mechanical_route(
    query: &str,
    resolver: &SourceResolver,
    snapshot: &RepositorySnapshot,
    require_structural_match: bool,
) -> Result<(Option<RouteAttempt>, Option<String>)> {
    if !is_quoted_literal(query) && looks_like_path(query) {
        let normalized = query.trim_matches(['\'', '"']).trim_start_matches("./");
        let mut files = snapshot
            .files
            .iter()
            .filter(|file| {
                file.outline.source.path == normalized
                    || file
                        .outline
                        .source
                        .path
                        .ends_with(&format!("/{normalized}"))
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.outline.source.path.cmp(&right.outline.source.path));
        if let Some(file) = files
            .iter()
            .copied()
            .find(|file| file.outline.source.path == normalized)
        {
            return Ok((
                Some(mechanical_file_result(resolver, file, "exact path match")?),
                None,
            ));
        }
        if files.len() > 1 {
            let mut attempts = files.into_iter();
            let first = attempts
                .next()
                .expect("multiple suffix matches have a first");
            let mut combined = mechanical_file_result(resolver, first, "suffix path match")?;
            for file in attempts {
                merge_route_attempts(
                    &mut combined,
                    mechanical_file_result(resolver, file, "suffix path match")?,
                );
                if combined.result.spans.len() == MAX_RETURNED_SPANS {
                    break;
                }
            }
            push_warning_once(
                &mut combined.result.warnings,
                "path suffix matched multiple files; returning bounded deterministic matches",
            );
            return Ok((Some(combined), None));
        }
        if let Some(file) = files.first() {
            return Ok((
                Some(mechanical_file_result(resolver, file, "suffix path match")?),
                None,
            ));
        }
    }
    let Some(term) = mechanical_term(query) else {
        return Ok((None, None));
    };
    let mut metrics = CodeHopMetrics::default();
    let warnings = Vec::new();
    let mut spans = Vec::new();
    let mut selected = Vec::new();
    let mut metadata_bytes = 0usize;
    let mut exhausted = false;
    for file in &snapshot.files {
        if metrics.mechanical_files_examined == MAX_MECHANICAL_FILES
            || metrics.mechanical_source_bytes_examined + file.outline.source.byte_len
                > MAX_MECHANICAL_BYTES
        {
            exhausted = true;
            break;
        }
        metrics.mechanical_files_examined += 1;
        metrics.mechanical_source_bytes_examined += file.outline.source.byte_len;
        let source = resolver.read(&file.outline.source.path)?;
        for (start, _) in source.text.match_indices(&term) {
            let end = start + term.len();
            let (start_line, end_line) = line_coordinates(&source.text, start, end);
            let matched_record = file
                .outline
                .records
                .iter()
                .find(|record| record.span.start_byte <= start && record.span.end_byte >= end);
            if require_structural_match && matched_record.is_none() {
                continue;
            }
            let span = matched_record
                .map(|record| record.span.clone())
                .unwrap_or(SourceSpan {
                    start_byte: start,
                    end_byte: end,
                    start_line,
                    end_line,
                });
            let why = bounded_why(format!("fixed-string match for {term:?}"));
            metadata_bytes += file.outline.source.path.len() + why.len() + 64;
            if metadata_bytes > MAX_MECHANICAL_METADATA_BYTES
                || spans.len() == MAX_MECHANICAL_MATCHES
            {
                exhausted = true;
                break;
            }
            spans.push(span_result(
                &file.outline.source,
                &span,
                matched_record.map(|record| record.name.clone()),
                matched_record.map(|record| record.kind.clone()),
                if query.trim().starts_with('\'') || query.trim().starts_with('"') {
                    "literal"
                } else {
                    "identifier"
                },
                why,
            ));
            selected.push((file.outline.source.clone(), span));
        }
        if exhausted {
            break;
        }
    }
    if spans.is_empty() {
        return Ok((
            None,
            exhausted.then(|| {
                "mechanical fixed-string search reached its deterministic scan bound; sibling routing continued"
                    .to_string()
            }),
        ));
    }
    spans.truncate(MAX_RETURNED_SPANS);
    selected.truncate(MAX_RETURNED_SPANS);
    let partial_warning = exhausted.then(|| {
        "mechanical fixed-string search reached its deterministic scan/result bound; sibling routing continued"
            .to_string()
    });
    metrics.selected_files = selected
        .iter()
        .map(|(source, _)| source.path.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    Ok((
        Some(RouteAttempt {
            result: CodeHopResult {
                spans,
                hop_path: vec![HopStep {
                    directory: String::new(),
                    picks: vec![term],
                    method: "mechanical_fixed_string".to_string(),
                }],
                provenance: RetrievalProvenance {
                    class: RetrievalProvenanceClass::LexicalGrep,
                    method: RetrievalMethod::Grep,
                },
                warnings,
                metrics,
            },
            selected,
        }),
        partial_warning,
    ))
}

fn merge_route_attempts(primary: &mut RouteAttempt, secondary: RouteAttempt) {
    let mut seen = primary
        .selected
        .iter()
        .map(|(source, span)| (source.path.clone(), span.start_byte, span.end_byte))
        .collect::<BTreeSet<_>>();
    for (span, selected) in secondary
        .result
        .spans
        .into_iter()
        .zip(secondary.selected.into_iter())
    {
        let key = (
            selected.0.path.clone(),
            selected.1.start_byte,
            selected.1.end_byte,
        );
        if primary.result.spans.len() == MAX_RETURNED_SPANS {
            break;
        }
        if seen.insert(key) {
            primary.result.spans.push(span);
            primary.selected.push(selected);
        }
    }
    primary.result.hop_path.extend(secondary.result.hop_path);
    for warning in secondary.result.warnings {
        push_warning_once(&mut primary.result.warnings, &warning);
    }
    primary.result.metrics.menus_considered += secondary.result.metrics.menus_considered;
    primary.result.metrics.candidates_considered += secondary.result.metrics.candidates_considered;
    primary.result.metrics.routing_context_bytes += secondary.result.metrics.routing_context_bytes;
    primary.result.metrics.ranker_input_bytes += secondary.result.metrics.ranker_input_bytes;
    primary.result.metrics.disambiguation_input_bytes +=
        secondary.result.metrics.disambiguation_input_bytes;
    primary.result.metrics.disambiguation_calls += secondary.result.metrics.disambiguation_calls;
    primary.result.metrics.selected_files = primary
        .selected
        .iter()
        .map(|(source, _)| source.path.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    if secondary.result.provenance.class == RetrievalProvenanceClass::InferredModel {
        primary.result.provenance = secondary.result.provenance;
    }
}

fn mechanical_file_result(
    resolver: &SourceResolver,
    file: &IndexedOutline,
    why: &str,
) -> Result<RouteAttempt> {
    let mut selected_records = file
        .outline
        .records
        .iter()
        .map(|record| record.span.clone())
        .take(MAX_RETURNED_SPANS)
        .collect::<Vec<_>>();
    if selected_records.is_empty() {
        let source = resolver.read(&file.outline.source.path)?;
        let end_byte = source
            .text
            .match_indices('\n')
            .nth(EXACT_PATH_WINDOW_LINES - 1)
            .map(|(index, _)| index + 1)
            .unwrap_or(source.text.len());
        let (start_line, end_line) = line_coordinates(&source.text, 0, end_byte);
        selected_records.push(SourceSpan {
            start_byte: 0,
            end_byte,
            start_line,
            end_line,
        });
    }
    let spans = selected_records
        .iter()
        .map(|span| {
            let symbol = file
                .outline
                .records
                .iter()
                .find(|record| record.span == *span)
                .map(|record| record.name.clone());
            let symbol_kind = file
                .outline
                .records
                .iter()
                .find(|record| record.span == *span)
                .map(|record| record.kind.clone());
            span_result(
                &file.outline.source,
                span,
                symbol,
                symbol_kind,
                "path",
                why.to_string(),
            )
        })
        .collect();
    let selected = selected_records
        .iter()
        .map(|span| (file.outline.source.clone(), span.clone()))
        .collect();
    Ok(RouteAttempt {
        result: CodeHopResult {
            spans,
            hop_path: vec![HopStep {
                directory: String::new(),
                picks: vec![file.outline.source.path.clone()],
                method: "mechanical_path".to_string(),
            }],
            provenance: RetrievalProvenance {
                class: RetrievalProvenanceClass::LexicalGrep,
                method: RetrievalMethod::Grep,
            },
            warnings: Vec::new(),
            metrics: CodeHopMetrics {
                selected_files: 1,
                ..CodeHopMetrics::default()
            },
        },
        selected,
    })
}

fn rank_file_records(
    query: &str,
    resolver: &SourceResolver,
    file: &IndexedOutline,
) -> Result<Vec<RankedCandidate>> {
    let fallback = file.outline.provenance.class == RetrievalProvenanceClass::StructuralFallback;
    let source = fallback
        .then(|| resolver.read(&file.outline.source.path))
        .transpose()?;
    Ok(file
        .outline
        .records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let rank_text = source
                .as_ref()
                .and_then(|source| {
                    source
                        .text
                        .get(record.span.start_byte..record.span.end_byte)
                })
                .unwrap_or(&record.name);
            RankedCandidate {
                id: format!("s{index}"),
                name: record.name.clone(),
                kind: record.kind.clone(),
                target: index.to_string(),
                lead: None,
                score: lexical_score(query, rank_text),
                ranking_bytes: serialized_ranker_input_bytes(query, rank_text),
            }
        })
        .collect())
}

fn subtree_rank_text(
    snapshot: &RepositorySnapshot,
    target: &str,
    kind: DirectoryChildKind,
) -> String {
    let mut text = target.to_string();
    let prefix = format!("{target}/");
    for file in &snapshot.files {
        let path = &file.outline.source.path;
        let participates = match kind {
            DirectoryChildKind::File => path == target,
            DirectoryChildKind::Directory => path.starts_with(&prefix),
        };
        if !participates {
            continue;
        }
        text.push(' ');
        text.push_str(path);
        for record in &file.outline.records {
            text.push(' ');
            text.push_str(&record.name);
            text.push(' ');
            text.push_str(&record.kind);
            if text.len() >= 16 * 1024 {
                truncate_utf8(&mut text, 16 * 1024);
                return text;
            }
        }
    }
    text
}

fn truncate_utf8(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

fn serialized_ranker_input_bytes(query: &str, candidate: &str) -> usize {
    serde_json::to_vec(&serde_json::json!({"query": query, "candidate": candidate}))
        .expect("strings always serialize to JSON")
        .len()
}

fn lexical_score(query: &str, candidate: &str) -> f64 {
    let query_tokens = tokens(query);
    let candidate_tokens = tokens(candidate);
    let mut score = 0.0;
    for query_token in &query_tokens {
        for candidate_token in &candidate_tokens {
            if query_token == candidate_token {
                score += 2.0;
            } else if query_token.starts_with(candidate_token)
                || candidate_token.starts_with(query_token)
            {
                score += 1.0;
            }
        }
    }
    score + trigram_similarity(query, candidate)
}

fn tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn trigram_similarity(left: &str, right: &str) -> f64 {
    let grams = |value: &str| {
        let normalized = value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<Vec<_>>();
        normalized
            .windows(3)
            .map(|window| window.iter().collect::<String>())
            .collect::<BTreeSet<_>>()
    };
    let left = grams(left);
    let right = grams(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count() as f64;
    let union = left.union(&right).count() as f64;
    intersection / union
}

fn rank_order(left: &RankedCandidate, right: &RankedCandidate) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.target.cmp(&right.target))
}

fn validate_disambiguation(
    offered: &[RankedCandidate],
    ids: &[String],
) -> Option<Vec<RankedCandidate>> {
    if ids.is_empty() || ids.len() > MAX_SELECTED_PER_MENU {
        return None;
    }
    let unique = ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != ids.len() {
        return None;
    }
    ids.iter()
        .map(|id| {
            offered
                .iter()
                .find(|candidate| candidate.id == *id)
                .cloned()
        })
        .collect()
}

fn mechanical_term(query: &str) -> Option<String> {
    let trimmed = query.trim();
    if is_quoted_literal(trimmed) {
        let literal = &trimmed[1..trimmed.len() - 1];
        return (!literal.is_empty()).then(|| literal.to_string());
    }
    (trimmed
        .chars()
        .all(|character| character.is_alphanumeric() || "_:-.".contains(character))
        && trimmed.chars().any(|character| character.is_alphabetic()))
    .then(|| trimmed.to_string())
}

fn is_quoted_literal(query: &str) -> bool {
    let trimmed = query.trim();
    trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
}

fn looks_like_path(query: &str) -> bool {
    let query = query.trim();
    query.contains('/') || Path::new(query).extension().is_some()
}

fn join_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}/{child}")
    }
}

fn line_coordinates(text: &str, start_byte: usize, end_byte: usize) -> (usize, usize) {
    let start_line = text[..start_byte]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let end_line = if end_byte == start_byte {
        start_line
    } else {
        text[..end_byte - 1]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1
    };
    (start_line, end_line)
}

fn span_result(
    source: &SourceIdentity,
    span: &SourceSpan,
    symbol: Option<String>,
    symbol_kind: Option<String>,
    match_kind: &'static str,
    why: String,
) -> CodeHopSpan {
    CodeHopSpan {
        path: source.path.clone(),
        start_byte: span.start_byte,
        end_byte: span.end_byte,
        start_line: span.start_line,
        end_line: span.end_line,
        symbol,
        symbol_kind,
        match_kind,
        why: bounded_why(why),
    }
}

fn bounded_why(mut why: String) -> String {
    if why.len() <= MAX_WHY_BYTES {
        return why;
    }
    let mut end = MAX_WHY_BYTES;
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    why.truncate(end);
    why
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry, ToolUse};
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("workspace");
        run_git(root.path(), &["init", "-q"]);
        fs::create_dir(root.path().join("src")).expect("src");
        fs::create_dir(root.path().join("left")).expect("left");
        fs::create_dir(root.path().join("right")).expect("right");
        fs::write(root.path().join("AGENTS.md"), "Repository map.\n").expect("root agents");
        fs::write(
            root.path().join("src/AGENTS.md"),
            "Claim admission and scheduling live in this capsule.\n",
        )
        .expect("src agents");
        fs::write(
            root.path().join("src/claim.rs"),
            "pub struct Claim;\npub fn admit_claim() { let _ = \"TARGET_BODY_PRIVATE\"; }\n",
        )
        .expect("claim source");
        fs::write(
            root.path().join("src/distractor.rs"),
            "// DISTRACTOR_SECRET_MUST_NOT_ROUTE\npub const CLAIM_PATH: &str = \"src/claim.rs\";\npub fn render_status() {}\n",
        )
        .expect("distractor source");
        fs::write(
            root.path().join("src/ambiguous.rs"),
            "pub fn alpha_route() {}\npub fn beta_route() {}\n",
        )
        .expect("ambiguous source");
        fs::write(
            root.path().join("fallback.txt"),
            "ordinary text\nwhere unusual widgets are calibrated\n",
        )
        .expect("fallback source");
        fs::write(
            root.path().join("headingless.md"),
            "introductory prose without a heading\nsecond line\n",
        )
        .expect("headingless Markdown");
        fs::write(root.path().join("left/mod.rs"), "pub fn route_left() {}\n")
            .expect("left module");
        fs::write(
            root.path().join("right/mod.rs"),
            "pub fn route_right() {}\n",
        )
        .expect("right module");
        run_git(root.path(), &["add", "."]);
        run_git(
            root.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        root
    }

    fn state_parent() -> tempfile::TempDir {
        let state = tempfile::tempdir().expect("state parent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700))
                .expect("private state parent");
        }
        state
    }

    #[derive(Debug)]
    struct ComparisonMetrics {
        task_success: bool,
        serialized_response_bytes: usize,
        files_read: usize,
        source_body_bytes_disclosed: usize,
        unsupported_language_success: bool,
    }

    fn naive_grep_read_baseline(
        resolver: &SourceResolver,
        snapshot: &RepositorySnapshot,
    ) -> ComparisonMetrics {
        let mut files_read = 0;
        let mut returned = Vec::new();
        let mut source_body_bytes_disclosed = 0;
        let mut task_success = false;
        let mut unsupported_language_success = false;
        for file in &snapshot.files {
            let source = resolver
                .read(&file.outline.source.path)
                .expect("baseline read");
            files_read += 1;
            if source.text.contains("admit_claim") {
                source_body_bytes_disclosed += source.text.len();
                returned.push((file.outline.source.path.clone(), source.text.clone()));
                task_success = file.outline.source.path == "src/claim.rs"
                    && file
                        .outline
                        .records
                        .iter()
                        .any(|record| record.name == "admit_claim");
            }
            if source.text.contains("unusual widgets") {
                source_body_bytes_disclosed += source.text.len();
                returned.push((file.outline.source.path.clone(), source.text.clone()));
                unsupported_language_success = file.outline.source.path == "fallback.txt";
            }
        }
        let serialized_response_bytes = serde_json::to_vec(&returned)
            .expect("naive grep/read response")
            .len();
        ComparisonMetrics {
            task_success,
            serialized_response_bytes,
            files_read,
            source_body_bytes_disclosed,
            unsupported_language_success,
        }
    }

    fn file_list_pick<'a>(snapshot: &'a RepositorySnapshot, query: &str) -> Option<&'a str> {
        snapshot
            .files
            .iter()
            .map(|file| file.outline.source.path.as_str())
            .max_by(|left, right| {
                lexical_score(query, left)
                    .total_cmp(&lexical_score(query, right))
                    .then_with(|| right.as_bytes().cmp(left.as_bytes()))
            })
    }

    fn file_list_baseline(snapshot: &RepositorySnapshot) -> ComparisonMetrics {
        let serialized_response_bytes = serde_json::to_vec(
            &snapshot
                .files
                .iter()
                .map(|file| file.outline.source.path.as_str())
                .collect::<Vec<_>>(),
        )
        .expect("file-list JSON")
        .len();
        let task_success =
            file_list_pick(snapshot, "where does claim admission live?") == Some("src/claim.rs");
        let unsupported_language_success =
            file_list_pick(snapshot, "where are unusual widgets calibrated?")
                == Some("fallback.txt");
        ComparisonMetrics {
            task_success,
            serialized_response_bytes,
            files_read: 0,
            source_body_bytes_disclosed: 0,
            unsupported_language_success,
        }
    }

    fn find_code_comparison(
        snapshot: &RepositorySnapshot,
        primary: &CodeHopResult,
        unsupported: &CodeHopResult,
    ) -> ComparisonMetrics {
        ComparisonMetrics {
            task_success: primary.spans.iter().any(|span| {
                span.path == "src/claim.rs" && span.symbol.as_deref() == Some("admit_claim")
            }),
            serialized_response_bytes: serde_json::to_vec(&compact_response(primary.clone()))
                .expect("complete find_code response")
                .len(),
            // A current cached query hashes each indexed source once, then
            // generation-validates each returned span before exposing it.
            files_read: snapshot.files.len() + primary.spans.len(),
            source_body_bytes_disclosed: primary.metrics.body_bytes_disclosed,
            unsupported_language_success: unsupported
                .spans
                .iter()
                .any(|span| span.path == "fallback.txt"),
        }
    }

    #[tokio::test]
    async fn test_natural_query_returns_claim_definition_without_body_or_distractor_secret() {
        let root = fixture();
        let state = state_parent();
        let result = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .execute_query("where does claim admission live?")
            .await
            .expect("code hop");
        let json = serde_json::to_string(&result).expect("result JSON");

        assert!(json.contains("src/claim.rs"), "{json}");
        assert!(json.contains("admit_claim"), "{json}");
        assert!(!json.contains("TARGET_BODY_PRIVATE"), "{json}");
        assert!(!json.contains("DISTRACTOR_SECRET_MUST_NOT_ROUTE"), "{json}");
        assert_eq!(result.metrics.body_bytes_disclosed, 0);
    }

    #[tokio::test]
    async fn test_exact_headingless_path_returns_a_bounded_span() {
        let root = fixture();
        let state = state_parent();
        let result = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .execute_query("headingless.md")
            .await
            .expect("exact path route");

        assert_eq!(result.spans.len(), 1);
        assert_eq!(result.spans[0].path, "headingless.md");
        assert_eq!(result.spans[0].match_kind, "path");
        assert_eq!(result.spans[0].start_byte, 0);
        assert!(result.spans[0].end_byte > 0);
        assert_eq!(result.metrics.body_bytes_disclosed, 0);
    }

    struct RecordingDisambiguator {
        calls: AtomicUsize,
        payloads: Mutex<Vec<String>>,
        preferred_name: String,
        invalid: bool,
    }

    struct MutatingDisambiguator {
        source: PathBuf,
        calls: AtomicUsize,
    }

    struct AlwaysMutatingDisambiguator {
        source: PathBuf,
        calls: AtomicUsize,
    }

    struct FailingDisambiguator;

    struct PendingDisambiguator;

    #[async_trait]
    impl CodeHopDisambiguator for MutatingDisambiguator {
        async fn choose(&self, request: &DisambiguationRequest) -> Result<Vec<String>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                fs::write(
                    &self.source,
                    "pub fn alpha_route() {}\npub fn beta_route() {}\n// changed generation\n",
                )
                .expect("mutate selected source");
            }
            Ok(request
                .candidates
                .first()
                .map(|candidate| vec![candidate.id.clone()])
                .unwrap_or_default())
        }
    }

    #[async_trait]
    impl CodeHopDisambiguator for AlwaysMutatingDisambiguator {
        async fn choose(&self, request: &DisambiguationRequest) -> Result<Vec<String>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            fs::write(
                &self.source,
                format!("pub fn alpha_route() {{}}\npub fn beta_route() {{}}\n// race {call}\n"),
            )
            .expect("mutate selected source on every route");
            Ok(request
                .candidates
                .first()
                .map(|candidate| vec![candidate.id.clone()])
                .unwrap_or_default())
        }
    }

    #[async_trait]
    impl CodeHopDisambiguator for FailingDisambiguator {
        async fn choose(&self, _request: &DisambiguationRequest) -> Result<Vec<String>> {
            bail!("injected disambiguator failure")
        }
    }

    #[async_trait]
    impl CodeHopDisambiguator for PendingDisambiguator {
        async fn choose(&self, _request: &DisambiguationRequest) -> Result<Vec<String>> {
            std::future::pending().await
        }
    }

    #[async_trait]
    impl CodeHopDisambiguator for RecordingDisambiguator {
        async fn choose(&self, request: &DisambiguationRequest) -> Result<Vec<String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.payloads
                .lock()
                .expect("payload lock")
                .push(serde_json::to_string(request)?);
            if self.invalid {
                return Ok(vec!["invented-path.rs".to_string()]);
            }
            Ok(request
                .candidates
                .iter()
                .find(|candidate| candidate.name == self.preferred_name)
                .or_else(|| request.candidates.first())
                .map(|candidate| vec![candidate.id.clone()])
                .unwrap_or_default())
        }
    }

    #[tokio::test]
    async fn test_identifier_route_skips_disambiguator_and_model_payloads_omit_bodies() {
        let root = fixture();
        let state = state_parent();
        let recorder = Arc::new(RecordingDisambiguator {
            calls: AtomicUsize::new(0),
            payloads: Mutex::new(Vec::new()),
            preferred_name: "claim.rs".to_string(),
            invalid: false,
        });
        let tool = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .with_disambiguator(recorder.clone());

        let mechanical = tool
            .execute_query("admit_claim")
            .await
            .expect("mechanical route");
        assert_eq!(recorder.calls.load(Ordering::SeqCst), 0);
        assert_eq!(mechanical.provenance.method, RetrievalMethod::Grep);

        let literal = tool
            .execute_query("\"TARGET_BODY_PRIVATE\"")
            .await
            .expect("quoted literal route");
        assert_eq!(recorder.calls.load(Ordering::SeqCst), 0);
        assert!(
            literal.spans.iter().any(|span| span.path == "src/claim.rs"),
            "spans: {:?}",
            literal.spans
        );

        let path_shaped_literal = tool
            .execute_query("\"src/claim.rs\"")
            .await
            .expect("path-shaped quoted literal route");
        assert!(
            path_shaped_literal
                .spans
                .iter()
                .any(|span| span.path == "src/distractor.rs"),
            "spans: {:?}",
            path_shaped_literal.spans
        );
        assert!(
            path_shaped_literal
                .spans
                .iter()
                .all(|span| span.match_kind == "literal"),
            "spans: {:?}",
            path_shaped_literal.spans
        );
        assert!(
            literal
                .spans
                .iter()
                .all(|span| span.match_kind == "literal"),
            "spans: {:?}",
            literal.spans
        );

        let routed = tool
            .execute_query("alpha beta")
            .await
            .expect("ambiguous route");
        assert!(!routed.spans.is_empty());
        let payloads = recorder.payloads.lock().expect("payload lock");
        let payload = payloads.join("\n");
        assert!(!payload.contains("TARGET_BODY_PRIVATE"), "{payload}");
        assert!(
            !payload.contains("DISTRACTOR_SECRET_MUST_NOT_ROUTE"),
            "{payload}"
        );
    }

    #[tokio::test]
    async fn test_stale_selected_leaf_discards_result_rebuilds_and_reroutes_once() {
        let root = fixture();
        let state = state_parent();
        let disambiguator = Arc::new(MutatingDisambiguator {
            source: root.path().join("src/ambiguous.rs"),
            calls: AtomicUsize::new(0),
        });
        let result = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .with_disambiguator(disambiguator.clone())
            .execute_query("alpha beta")
            .await
            .expect("rerouted result");

        assert!(
            disambiguator.calls.load(Ordering::SeqCst) >= 2,
            "stale first selection must cause a complete reroute"
        );
        assert!(
            result
                .spans
                .iter()
                .all(|span| span.path == "src/ambiguous.rs"),
            "spans: {:?}",
            result.spans
        );
    }

    #[tokio::test]
    async fn test_second_selected_leaf_race_returns_one_error_without_partial_result() {
        let root = fixture();
        let state = state_parent();
        let disambiguator = Arc::new(AlwaysMutatingDisambiguator {
            source: root.path().join("src/ambiguous.rs"),
            calls: AtomicUsize::new(0),
        });
        let error = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .with_disambiguator(disambiguator.clone())
            .execute_query("alpha beta")
            .await
            .err()
            .expect("second race must fail closed");

        assert!(error
            .to_string()
            .contains("both find_code routing attempts"));
        assert!(disambiguator.calls.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn test_ambiguous_suffix_path_returns_each_bounded_branch_and_trace() {
        let root = fixture();
        let state = state_parent();
        let result = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .execute_query("mod.rs")
            .await
            .expect("suffix path route");

        assert!(result.spans.iter().any(|span| span.path == "left/mod.rs"));
        assert!(result.spans.iter().any(|span| span.path == "right/mod.rs"));
        assert!(result
            .hop_path
            .iter()
            .flat_map(|step| &step.picks)
            .any(|pick| pick == "left/mod.rs"));
        assert!(result
            .hop_path
            .iter()
            .flat_map(|step| &step.picks)
            .any(|pick| pick == "right/mod.rs"));
    }

    #[tokio::test]
    async fn test_ambiguous_natural_route_preserves_every_returned_branch_trace() {
        let root = fixture();
        let state = state_parent();
        let result = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .execute_query("left right modules?")
            .await
            .expect("ambiguous natural route");
        let returned_paths = result
            .spans
            .iter()
            .map(|span| span.path.as_str())
            .collect::<BTreeSet<_>>();
        let traced_files = result
            .hop_path
            .iter()
            .map(|step| step.directory.as_str())
            .collect::<BTreeSet<_>>();
        assert!(returned_paths.len() > 1, "spans: {:?}", result.spans);
        for path in returned_paths {
            assert!(
                traced_files.contains(path),
                "missing branch trace for {path}"
            );
        }
    }

    #[tokio::test]
    async fn test_repeat_route_is_byte_deterministic_and_records_comparative_metrics() {
        let root = fixture();
        let state = state_parent();
        let tool = FindCodeTool::new(root.path(), state.path().join("source-index"));
        let first = tool
            .execute_query("where does claim admission live?")
            .await
            .expect("first route");
        let second = tool
            .execute_query("where does claim admission live?")
            .await
            .expect("second route");
        assert_eq!(
            serde_json::to_vec(&first).expect("first JSON"),
            serde_json::to_vec(&second).expect("second JSON"),
            "same-generation routing must be byte deterministic"
        );

        let resolver = SourceResolver::new(root.path()).expect("resolver");
        let cache =
            RepositoryCache::prepare(state.path().join("source-index"), &resolver).expect("cache");
        let snapshot = cache.load().expect("load cache").expect("snapshot");
        let naive = naive_grep_read_baseline(&resolver, &snapshot);
        let file_list = file_list_baseline(&snapshot);
        let response = serde_json::to_vec(&compact_response(first.clone()))
            .expect("complete find_code response");
        let unsupported = tool
            .execute_query("where are unusual widgets calibrated?")
            .await
            .expect("unsupported-language route");
        let find_code = find_code_comparison(&snapshot, &first, &unsupported);

        assert!(find_code.task_success);
        assert_eq!(find_code.source_body_bytes_disclosed, 0);
        assert_eq!(find_code.serialized_response_bytes, response.len());
        assert_eq!(
            find_code.files_read,
            snapshot.files.len() + first.spans.len()
        );
        assert_eq!(first.metrics.selected_files, 1);
        assert_eq!(
            first.metrics.routing_context_bytes,
            first.metrics.ranker_input_bytes + first.metrics.disambiguation_input_bytes
        );
        assert!(first.metrics.routing_context_bytes > 0);
        let response_text = String::from_utf8(response.clone()).expect("UTF-8 response");
        for internal in ["hop_path", "metrics", "provenance", "why", "cache"] {
            assert!(!response_text.contains(internal), "{response_text}");
        }
        assert!(naive.task_success);
        assert!(naive.unsupported_language_success);
        assert_eq!(naive.files_read, snapshot.files.len());
        assert!(naive.serialized_response_bytes > 0);
        assert!(naive.source_body_bytes_disclosed > 0);
        assert_eq!(
            file_list.task_success,
            file_list_pick(&snapshot, "where does claim admission live?") == Some("src/claim.rs")
        );
        assert_eq!(
            file_list.unsupported_language_success,
            file_list_pick(&snapshot, "where are unusual widgets calibrated?")
                == Some("fallback.txt")
        );
        assert_eq!(file_list.files_read, 0);
        assert_eq!(file_list.source_body_bytes_disclosed, 0);
        assert!(file_list.serialized_response_bytes > 0);
        assert!(
            find_code.serialized_response_bytes < naive.serialized_response_bytes,
            "{response_text}"
        );
        assert!(find_code.unsupported_language_success);
    }

    #[tokio::test]
    async fn test_invalid_disambiguator_ids_fall_back_with_warning() {
        let root = fixture();
        let state = state_parent();
        let recorder = Arc::new(RecordingDisambiguator {
            calls: AtomicUsize::new(0),
            payloads: Mutex::new(Vec::new()),
            preferred_name: String::new(),
            invalid: true,
        });
        let result = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .with_disambiguator(recorder)
            .execute_query("alpha beta")
            .await
            .expect("fallback route");

        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("invalid candidate IDs")),
            "warnings: {:?}",
            result.warnings
        );
        assert_eq!(result.provenance.method, RetrievalMethod::Grep);
    }

    #[test]
    fn test_disambiguator_rejects_duplicate_and_excessive_ids() {
        let offered = (0..4)
            .map(|index| RankedCandidate {
                id: format!("id-{index}"),
                name: format!("candidate-{index}"),
                kind: "file".to_string(),
                target: index.to_string(),
                lead: None,
                score: 0.0,
                ranking_bytes: 0,
            })
            .collect::<Vec<_>>();
        assert!(validate_disambiguation(&offered, &["id-0".into(), "id-0".into()]).is_none());
        assert!(validate_disambiguation(
            &offered,
            &["id-0".into(), "id-1".into(), "id-2".into(), "id-3".into()]
        )
        .is_none());
    }

    #[tokio::test]
    async fn test_disambiguator_error_and_timeout_fall_back_with_warnings() {
        let root = fixture();
        let error_state = state_parent();
        let failed = FindCodeTool::new(root.path(), error_state.path().join("source-index"))
            .with_disambiguator(Arc::new(FailingDisambiguator))
            .execute_query("alpha beta")
            .await
            .expect("error fallback");
        assert!(failed
            .warnings
            .iter()
            .any(|warning| warning.contains("failed")));

        let timeout_state = state_parent();
        let timed_out = FindCodeTool::new(root.path(), timeout_state.path().join("source-index"))
            .with_disambiguator(Arc::new(PendingDisambiguator))
            .with_disambiguation_timeout(Duration::from_millis(1))
            .execute_query("alpha beta")
            .await
            .expect("timeout fallback");
        assert!(timed_out
            .warnings
            .iter()
            .any(|warning| warning.contains("timed out")));
    }

    #[tokio::test]
    async fn test_directory_and_symbol_near_ties_use_the_same_disambiguation_rule() {
        let root = fixture();
        let state = state_parent();
        let recorder = Arc::new(RecordingDisambiguator {
            calls: AtomicUsize::new(0),
            payloads: Mutex::new(Vec::new()),
            preferred_name: "left".to_string(),
            invalid: false,
        });
        let tool = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .with_disambiguator(recorder.clone());
        tool.execute_query("left right modules?")
            .await
            .expect("directory near tie");
        tool.execute_query("alpha beta")
            .await
            .expect("symbol near tie");
        let payload = recorder.payloads.lock().expect("payload lock").join("\n");
        assert!(payload.contains("\"kind\":\"directory\""), "{payload}");
        assert!(payload.contains("alpha_route"), "{payload}");
    }

    #[test]
    fn test_utf8_truncation_stops_at_a_character_boundary() {
        let mut text = format!("{}é", "a".repeat(16 * 1024 - 1));
        truncate_utf8(&mut text, 16 * 1024);
        assert_eq!(text.len(), 16 * 1024 - 1);
        assert!(text.is_char_boundary(text.len()));
    }

    #[tokio::test]
    async fn test_unbound_ranker_warns_and_routes_unsupported_language_window() {
        let root = fixture();
        let state = state_parent();
        let result = FindCodeTool::new(root.path(), state.path().join("source-index"))
            .execute_query("where are unusual widgets calibrated?")
            .await
            .expect("fallback route");
        assert!(
            result.spans.iter().any(|span| span.path == "fallback.txt"),
            "spans: {:?}",
            result.spans
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("unbound")),
            "warnings: {:?}",
            result.warnings
        );
        assert_eq!(result.metrics.disambiguation_calls, 0);
        assert_eq!(result.metrics.disambiguation_input_bytes, 0);
    }

    #[tokio::test]
    async fn test_tool_runs_through_executor_with_workspace_read_authority() {
        let root = fixture();
        let state = state_parent();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FindCodeTool::new(
            root.path(),
            state.path().join("source-index"),
        )));
        let permissions = PermissionManager::new()
            .with_default_rule(PermissionRule::Allow)
            .with_workspace_root(root.path().to_path_buf());
        let executor = ToolExecutor::new(
            registry,
            permissions,
            state.path().join("tool-patterns.json"),
        )
        .expect("executor");
        let result = executor
            .execute_tool(
                &ToolUse::new(
                    "find_code".to_string(),
                    serde_json::json!({"query": "admit_claim"}),
                ),
                None::<fn() -> anyhow::Result<()>>,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("tool result");

        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content.contains("src/claim.rs"),
            "{}",
            result.content
        );
        let value: Value = serde_json::from_str(&result.content).expect("compact JSON response");
        let matches = value["matches"].as_array().expect("matches array");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0][0], "src/claim.rs");
        assert_eq!(matches[0][3], "admit_claim");
        assert_eq!(matches[0][4], "identifier");
        assert!(!result.content.contains("TARGET_BODY_PRIVATE"));
        for internal in ["hop_path", "metrics", "provenance", "why", "cache"] {
            assert!(!result.content.contains(internal), "{}", result.content);
        }
        assert!(result.content.len() < 128, "{}", result.content);

        let ambiguous = executor
            .execute_tool(
                &ToolUse::new(
                    "find_code".to_string(),
                    serde_json::json!({"query": "where does claim admission live?"}),
                ),
                None::<fn() -> anyhow::Result<()>>,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("ambiguous lexical result");
        let value: Value =
            serde_json::from_str(&ambiguous.content).expect("compact ambiguous response");
        assert_eq!(value["warnings"], serde_json::json!(["ambiguous"]));
    }

    #[tokio::test]
    async fn test_structured_scope_kind_and_limit_constrain_results() {
        let root = fixture();
        let state = state_parent();
        let tool = FindCodeTool::new(root.path(), state.path().join("source-index"));
        let scoped = tool
            .execute_search(
                "route_left",
                SearchOptions {
                    path: Some("left".to_string()),
                    kind: Some(SearchKind::Function),
                    limit: 1,
                },
            )
            .await
            .expect("scoped function search");
        assert_eq!(scoped.spans.len(), 1);
        assert_eq!(scoped.spans[0].path, "left/mod.rs");
        assert_eq!(scoped.spans[0].symbol.as_deref(), Some("route_left"));

        let type_only = tool
            .execute_search(
                "claim type",
                SearchOptions {
                    path: Some("src".to_string()),
                    kind: Some(SearchKind::Type),
                    limit: 1,
                },
            )
            .await
            .expect("type-only search");
        assert_eq!(type_only.spans.len(), 1);
        assert_eq!(type_only.spans[0].symbol.as_deref(), Some("Claim"));

        let exact_identifier_with_type_filter = tool
            .execute_search(
                "admit_claim",
                SearchOptions {
                    path: Some("src".to_string()),
                    kind: Some(SearchKind::Type),
                    limit: 1,
                },
            )
            .await
            .expect("wrong-kind exact match falls through to structural routing");
        assert_eq!(exact_identifier_with_type_filter.spans.len(), 1);
        assert_eq!(
            exact_identifier_with_type_filter.spans[0].symbol.as_deref(),
            Some("Claim")
        );

        let file_only = tool
            .execute_search(
                "claim admission",
                SearchOptions {
                    path: Some("src".to_string()),
                    kind: Some(SearchKind::File),
                    limit: 1,
                },
            )
            .await
            .expect("file-only search");
        assert_eq!(file_only.spans.len(), 1);
        assert_eq!(file_only.spans[0].path, "src/claim.rs");
        assert!(file_only.spans[0].symbol.is_none());
        assert_eq!(file_only.spans[0].match_kind, "file");
    }

    #[test]
    fn test_find_code_schema_and_option_bounds_are_explicit() {
        let tool = FindCodeTool::new(".", "state");
        assert_eq!(tool.name(), "find_code");
        let schema = tool.input_schema();
        assert_eq!(schema.required, vec!["query"]);
        for property in ["query", "path", "kind", "limit"] {
            assert!(
                schema.properties.get(property).is_some(),
                "missing {property}"
            );
        }
        assert!(parse_search_options(&serde_json::json!({"limit": 0})).is_err());
        assert!(parse_search_options(&serde_json::json!({"limit": 11})).is_err());
        assert!(parse_search_options(&serde_json::json!({"kind": "class"})).is_err());
        assert!(parse_search_options(&serde_json::json!({"path": "../secret"})).is_err());
    }

    #[test]
    fn test_rank_order_is_deterministic_for_ties() {
        let mut candidates = vec![
            RankedCandidate {
                id: "b".to_string(),
                name: "beta".to_string(),
                kind: "file".to_string(),
                target: "beta".to_string(),
                lead: None,
                score: 1.0,
                ranking_bytes: 1,
            },
            RankedCandidate {
                id: "a".to_string(),
                name: "alpha".to_string(),
                kind: "file".to_string(),
                target: "alpha".to_string(),
                lead: None,
                score: 1.0,
                ranking_bytes: 1,
            },
        ];
        candidates.sort_by(rank_order);
        assert_eq!(candidates[0].name, "alpha");
    }

    #[tokio::test]
    #[ignore = "reports comparative latency; correctness is covered by deterministic tests"]
    async fn benchmark_find_code_routing_latency() {
        const ITERATIONS: usize = 25;
        let root = fixture();
        let state = state_parent();
        let tool = FindCodeTool::new(root.path(), state.path().join("source-index"));
        let query = "where does claim admission live?";
        tool.execute_query(query)
            .await
            .expect("warm repository cache");

        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            std::hint::black_box(tool.execute_query(query).await.expect("find_code route"));
        }
        let find_code_elapsed = start.elapsed();

        let resolver = SourceResolver::new(root.path()).expect("resolver");
        let cache =
            RepositoryCache::prepare(state.path().join("source-index"), &resolver).expect("cache");
        let snapshot = cache.load().expect("load cache").expect("snapshot");
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            std::hint::black_box(naive_grep_read_baseline(&resolver, &snapshot));
        }
        let naive_scan_elapsed = start.elapsed();

        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            std::hint::black_box(file_list_baseline(&snapshot));
        }
        let file_list_elapsed = start.elapsed();
        let naive = naive_grep_read_baseline(&resolver, &snapshot);
        let file_list = file_list_baseline(&snapshot);
        let final_find_code = tool
            .execute_query(query)
            .await
            .expect("final find_code route");
        let unsupported = tool
            .execute_query("where are unusual widgets calibrated?")
            .await
            .expect("unsupported-language route");
        let find_code = find_code_comparison(&snapshot, &final_find_code, &unsupported);
        eprintln!(
            "{ITERATIONS} iterations: find_code={find_code_elapsed:?} {find_code:?}, naive_grep_read={naive_scan_elapsed:?} {naive:?}, file_list={file_list_elapsed:?} {file_list:?}"
        );
    }
}
