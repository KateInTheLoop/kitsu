//! Bounded, provider-neutral structural context selection.
//!
//! This module is deliberately not registered as a model-callable tool and is
//! not invoked by product composition. It is a compiled, read-only primitive
//! for measuring a future semantic-context contribution without granting any
//! filesystem authority of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use thiserror::Error;

use crate::builtins::fs::{
    find_symbol_tool, grep_tool, line_defines_symbol, read_file_tool, read_manifest_tool,
};
use crate::{scope_without_interactive_prompts, HandlerFn, Permissions, ToolError};

const MAX_PROMPT_BYTES: usize = 4 * 1024;
const MAX_TERMS: usize = 12;
const MAX_HITS: usize = 64;
const MAX_FILES: usize = 4;
const MAX_OUTPUT_BYTES: usize = 4 * 1024;
const MAX_ROOT_BYTES: usize = 4 * 1024;
const MAX_OBSERVED_PATH_BYTES: usize = 4 * 1024;

/// The evidence request class. `ChangeEvidence` selects read-only context for a
/// possible change; it never performs or validates a mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralContextTaskV1 {
    Locate,
    Explain,
    ChangeEvidence,
}

/// One bounded structural-context request.
///
/// `root` identifies where to search but grants no authority. The contributor
/// uses only the current [`Permissions`] supplied to its constructor.
pub struct StructuralContextRequestV1 {
    task: StructuralContextTaskV1,
    prompt: String,
    root: PathBuf,
}

impl StructuralContextRequestV1 {
    pub fn new(
        task: StructuralContextTaskV1,
        prompt: impl Into<String>,
        root: impl AsRef<Path>,
    ) -> Self {
        Self {
            task,
            prompt: prompt.into(),
            root: root.as_ref().to_path_buf(),
        }
    }
}

/// Complete evidence assembled from current permission-gated reads.
///
/// This value is created only after every selected hit has been reopened and
/// revalidated. An error therefore cannot carry a partial evidence artifact.
pub struct StructuralContextEvidenceV1 {
    rendered: String,
    symbol: String,
    selected_files: Vec<String>,
    definitions: usize,
    references: usize,
}

impl StructuralContextEvidenceV1 {
    pub fn rendered(&self) -> &str {
        &self.rendered
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    pub fn selected_files(&self) -> &[String] {
        &self.selected_files
    }

    pub fn definitions(&self) -> usize {
        self.definitions
    }

    pub fn references(&self) -> usize {
        self.references
    }
}

#[derive(Debug, Error)]
pub enum StructuralContextErrorV1 {
    #[error("invalid structural context request: {0}")]
    InvalidRequest(String),
    #[error("structural context observation failed: {0}")]
    Observation(#[source] ToolError),
    #[error("structural context observation contained an unsafe path")]
    UnsafeObservation,
    #[error("no prompt-derived structural evidence was observed")]
    NoEvidence,
    #[error("selected structural evidence changed before reading '{path}'")]
    SourceChanged { path: String },
    #[error("rendered structural evidence exceeded the {max_bytes}-byte limit")]
    OutputLimit { max_bytes: usize },
}

/// One compiled structural-context selector bound to existing read authority.
/// Construction is effect-free; selection occurs only through [`Self::contribute`].
pub struct StructuralContextContributorV1 {
    permissions: Arc<Permissions>,
}

impl StructuralContextContributorV1 {
    pub fn new(permissions: Arc<Permissions>) -> Self {
        Self { permissions }
    }

    /// Select and reconstruct one bounded evidence artifact.
    ///
    /// Interactive permission prompts are suppressed for the whole operation:
    /// an ungranted root fails closed instead of turning a caller-supplied path
    /// into authority. Discovery text is candidate metadata only. Every line
    /// returned to the caller comes from a subsequent exact authorized read.
    pub async fn contribute(
        &self,
        request: StructuralContextRequestV1,
    ) -> Result<StructuralContextEvidenceV1, StructuralContextErrorV1> {
        #[cfg(test)]
        let selection = self.contribute_inner(request, || {});
        #[cfg(not(test))]
        let selection = self.contribute_inner(request);
        scope_without_interactive_prompts(selection).await
    }

    #[cfg(test)]
    async fn contribute_with_test_checkpoint<F>(
        &self,
        request: StructuralContextRequestV1,
        after_ranking: F,
    ) -> Result<StructuralContextEvidenceV1, StructuralContextErrorV1>
    where
        F: FnOnce(),
    {
        self.contribute_inner(request, after_ranking).await
    }

    async fn contribute_inner(
        &self,
        request: StructuralContextRequestV1,
        #[cfg(test)] after_ranking: impl FnOnce(),
    ) -> Result<StructuralContextEvidenceV1, StructuralContextErrorV1> {
        if request.prompt.is_empty() || request.prompt.len() > MAX_PROMPT_BYTES {
            return Err(StructuralContextErrorV1::InvalidRequest(
                "prompt is empty or exceeds 4096 bytes".to_string(),
            ));
        }
        let root = request.root.to_str().ok_or_else(|| {
            StructuralContextErrorV1::InvalidRequest(
                "repository root is not valid UTF-8".to_string(),
            )
        })?;
        if root.is_empty() || root.len() > MAX_ROOT_BYTES {
            return Err(StructuralContextErrorV1::InvalidRequest(
                "repository root is empty or exceeds 4096 bytes".to_string(),
            ));
        }

        let (symbol, hits) =
            discover_symbol(&request.prompt, root, self.permissions.clone()).await?;
        let selected_files = rank_structural_files(&hits);
        if selected_files.is_empty() {
            return Err(StructuralContextErrorV1::NoEvidence);
        }
        let selected_set: BTreeSet<&str> = selected_files.iter().map(String::as_str).collect();
        let selected_hits: Vec<&StructuralHit> = hits
            .iter()
            .filter(|hit| selected_set.contains(hit.path.as_str()))
            .collect();

        // This checkpoint is absent from production builds and exists only for
        // deterministic unit fault injection. Candidate text observed before
        // this point is never retained as evidence.
        #[cfg(test)]
        after_ranking();

        let mut definitions = 0_usize;
        let mut references = 0_usize;
        let mut evidence_lines = BTreeMap::<(String, usize), String>::new();
        let mut evidence_line_bytes = 0_usize;

        for path in &selected_files {
            let path_hits: Vec<&StructuralHit> = selected_hits
                .iter()
                .copied()
                .filter(|hit| hit.path == *path)
                .collect();
            let absolute = request.root.join(path);
            let absolute = absolute.to_str().ok_or_else(|| {
                StructuralContextErrorV1::InvalidRequest(
                    "selected evidence path is not valid UTF-8".to_string(),
                )
            })?;
            let mut requested_windows: Vec<(usize, usize)> = path_hits
                .iter()
                .map(|hit| match hit.role {
                    StructuralRole::Definition => (
                        hit.line.saturating_sub(3).max(1),
                        hit.line.saturating_add(3),
                    ),
                    StructuralRole::Reference => (hit.line, hit.line),
                })
                .collect();
            requested_windows.sort_unstable();
            let mut windows = Vec::<(usize, usize)>::new();
            for (start, end) in requested_windows {
                if let Some((_, previous_end)) = windows.last_mut() {
                    if start <= previous_end.saturating_add(1) {
                        *previous_end = (*previous_end).max(end);
                        continue;
                    }
                }
                windows.push((start, end));
            }

            let mut current_lines = BTreeMap::<usize, String>::new();
            for (start, end) in windows {
                let snippet = call_observer(
                    read_file_tool(self.permissions.clone()).1,
                    json!({
                        "path": absolute,
                        "start_line": start,
                        "end_line": end,
                    }),
                )
                .await?;
                for line in snippet.lines() {
                    let Some((number, text)) = line.split_once('\t') else {
                        continue;
                    };
                    let Ok(line_number) = number.parse::<usize>() else {
                        continue;
                    };
                    if (start..=end).contains(&line_number) {
                        let text = text.trim();
                        if !text.is_empty() {
                            current_lines
                                .entry(line_number)
                                .or_insert_with(|| text.to_string());
                        }
                    }
                }
            }
            for hit in &path_hits {
                let Some(text) = current_lines.get(&hit.line) else {
                    return Err(StructuralContextErrorV1::SourceChanged { path: path.clone() });
                };
                if !identifier_words(text).iter().any(|word| word == &symbol) {
                    return Err(StructuralContextErrorV1::SourceChanged { path: path.clone() });
                }
                if line_defines_symbol(text, &symbol) {
                    definitions += 1;
                } else {
                    references += 1;
                }
            }
            for (line, text) in current_lines {
                let key = (path.clone(), line);
                if evidence_lines.contains_key(&key) {
                    continue;
                }
                let line_bytes = path
                    .len()
                    .saturating_add(line.to_string().len())
                    .saturating_add(text.len())
                    .saturating_add(4);
                evidence_line_bytes = evidence_line_bytes.saturating_add(line_bytes);
                if evidence_line_bytes > MAX_OUTPUT_BYTES {
                    return Err(StructuralContextErrorV1::OutputLimit {
                        max_bytes: MAX_OUTPUT_BYTES,
                    });
                }
                evidence_lines.insert(key, text);
            }
        }

        // Render only after every selected read and hit validation succeeded.
        let mut rendered = format!(
            "structural_selector> \"{symbol}\" — {definitions} definition(s), {references} reference(s)\n"
        );
        if rendered.len().saturating_add(evidence_line_bytes) > MAX_OUTPUT_BYTES {
            return Err(StructuralContextErrorV1::OutputLimit {
                max_bytes: MAX_OUTPUT_BYTES,
            });
        }
        for ((path, line), text) in evidence_lines {
            rendered.push_str(&format!("{path}:{line}: {text}\n"));
        }
        if let Some(manifest) = manifest_name(request.task, &selected_files) {
            let path = request.root.join(manifest);
            let path = path.to_str().ok_or_else(|| {
                StructuralContextErrorV1::InvalidRequest(
                    "manifest path is not valid UTF-8".to_string(),
                )
            })?;
            let manifest_output = call_observer(
                read_manifest_tool(self.permissions.clone()).1,
                json!({"path": path}),
            )
            .await?;
            if let Some(header) = manifest_output.lines().next() {
                if rendered
                    .len()
                    .saturating_add(header.len())
                    .saturating_add(1)
                    > MAX_OUTPUT_BYTES
                {
                    return Err(StructuralContextErrorV1::OutputLimit {
                        max_bytes: MAX_OUTPUT_BYTES,
                    });
                }
                rendered.push_str(header);
                rendered.push('\n');
            }
        }
        if rendered.len() > MAX_OUTPUT_BYTES {
            return Err(StructuralContextErrorV1::OutputLimit {
                max_bytes: MAX_OUTPUT_BYTES,
            });
        }

        Ok(StructuralContextEvidenceV1 {
            rendered,
            symbol,
            selected_files,
            definitions,
            references,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuralRole {
    Definition,
    Reference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StructuralHit {
    role: StructuralRole,
    path: String,
    line: usize,
}

#[derive(Debug, Default)]
struct RankedFile {
    score: usize,
    definitions: usize,
    references: usize,
}

fn identifier_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in input.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            current.push(character);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "and"
            | "are"
            | "authoritative"
            | "called"
            | "change"
            | "definition"
            | "defined"
            | "evidence"
            | "explain"
            | "for"
            | "from"
            | "how"
            | "identify"
            | "in"
            | "is"
            | "it"
            | "limit"
            | "locate"
            | "mirrors"
            | "needed"
            | "of"
            | "production"
            | "the"
            | "to"
            | "touching"
            | "use"
            | "where"
            | "without"
    )
}

fn prompt_terms(prompt: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    identifier_words(prompt)
        .into_iter()
        .filter(|word| word.len() >= 3)
        .filter(|word| {
            let lower = word.to_ascii_lowercase();
            !is_stop_word(&lower) && seen.insert(lower)
        })
        .take(MAX_TERMS)
        .collect()
}

fn valid_relative_observation_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_OBSERVED_PATH_BYTES
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn parse_structural_hits(raw: &str) -> Result<Vec<StructuralHit>, StructuralContextErrorV1> {
    let mut role = None;
    let mut hits = Vec::new();
    for line in raw.lines() {
        match line {
            "definitions:" => {
                role = Some(StructuralRole::Definition);
                continue;
            }
            "references:" => {
                role = Some(StructuralRole::Reference);
                continue;
            }
            _ => {}
        }
        let Some(hit_role) = role else {
            continue;
        };
        let Some(candidate) = line.strip_prefix("  ") else {
            continue;
        };
        if candidate.starts_with('[') {
            continue;
        }
        let mut fields = candidate.splitn(3, ':');
        let (Some(path), Some(line), Some(_text)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(line) = line.parse::<usize>() else {
            continue;
        };
        if !valid_relative_observation_path(path) {
            return Err(StructuralContextErrorV1::UnsafeObservation);
        }
        hits.push(StructuralHit {
            role: hit_role,
            path: path.to_string(),
            line,
        });
        if hits.len() > MAX_HITS {
            return Err(StructuralContextErrorV1::InvalidRequest(
                "symbol graph exceeded its 64-hit bound".to_string(),
            ));
        }
    }
    Ok(hits)
}

fn path_is_generated_or_build_output(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        let Component::Normal(name) = component else {
            return true;
        };
        let lower = name.to_string_lossy().to_ascii_lowercase();
        matches!(
            lower.as_str(),
            "generated" | "target" | "node_modules" | "dist" | "build" | "vendor"
        ) || lower.contains(".generated.")
    })
}

fn rank_structural_files(hits: &[StructuralHit]) -> Vec<String> {
    let mut graph = BTreeMap::<String, RankedFile>::new();
    for hit in hits {
        if path_is_generated_or_build_output(&hit.path) {
            continue;
        }
        let node = graph.entry(hit.path.clone()).or_default();
        match hit.role {
            StructuralRole::Definition => {
                node.definitions += 1;
                node.score += 100;
            }
            StructuralRole::Reference => {
                node.references += 1;
                node.score += 60;
            }
        }
    }
    for node in graph.values_mut() {
        if node.definitions > 0 && node.references > 0 {
            node.score += 20;
        }
    }
    let mut ranked: Vec<(String, usize)> = graph
        .into_iter()
        .map(|(path, node)| (path, node.score))
        .collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked
        .into_iter()
        .take(MAX_FILES)
        .map(|(path, _)| path)
        .collect()
}

async fn call_observer(
    handler: HandlerFn,
    args: Value,
) -> Result<String, StructuralContextErrorV1> {
    handler(args.to_string())
        .await
        .map_err(StructuralContextErrorV1::Observation)
}

fn choose_identifier_from_search(raw: &str, term: &str) -> Option<String> {
    let term = term.to_ascii_lowercase();
    let mut counts = BTreeMap::<String, usize>::new();
    for word in identifier_words(raw) {
        let lower = word.to_ascii_lowercase();
        if lower != term
            && lower.contains(&term)
            && word.len() <= 128
            && word
                .bytes()
                .any(|byte| byte == b'_' || byte.is_ascii_uppercase())
        {
            *counts.entry(word).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|left, right| {
            let score = |(word, count): &(String, usize)| {
                count * 100
                    + word.bytes().filter(|byte| *byte == b'_').count() * 20
                    + word.bytes().filter(u8::is_ascii_uppercase).count()
            };
            score(left)
                .cmp(&score(right))
                .then_with(|| right.0.cmp(&left.0))
        })
        .map(|(word, _)| word)
}

async fn discover_symbol(
    prompt: &str,
    root: &str,
    permissions: Arc<Permissions>,
) -> Result<(String, Vec<StructuralHit>), StructuralContextErrorV1> {
    let terms = prompt_terms(prompt);
    for candidate in terms.iter().filter(|term| term.contains('_')) {
        let raw = call_observer(
            find_symbol_tool(permissions.clone()).1,
            json!({
                "symbol": candidate,
                "path": root,
                "max_results": MAX_HITS,
            }),
        )
        .await?;
        let hits = parse_structural_hits(&raw)?;
        if !hits.is_empty() {
            return Ok((candidate.clone(), hits));
        }
    }

    for term in terms.iter().filter(|term| !term.contains('_')) {
        let raw = call_observer(
            grep_tool(permissions.clone()).1,
            json!({
                "pattern": format!("(?i){}", regex::escape(term)),
                "path": root,
                "max_count": MAX_HITS,
            }),
        )
        .await?;
        let Some(candidate) = choose_identifier_from_search(&raw, term) else {
            continue;
        };
        let symbol_raw = call_observer(
            find_symbol_tool(permissions.clone()).1,
            json!({
                "symbol": candidate,
                "path": root,
                "max_results": MAX_HITS,
            }),
        )
        .await?;
        let hits = parse_structural_hits(&symbol_raw)?;
        if !hits.is_empty() {
            return Ok((candidate, hits));
        }
    }
    Err(StructuralContextErrorV1::NoEvidence)
}

fn manifest_name(task: StructuralContextTaskV1, selected: &[String]) -> Option<&'static str> {
    if task != StructuralContextTaskV1::ChangeEvidence {
        return None;
    }
    if selected
        .iter()
        .any(|path| path.ends_with(".ts") || path.ends_with(".js"))
    {
        Some("package.json")
    } else if selected.iter().any(|path| path.ends_with(".rs")) {
        Some("Cargo.toml")
    } else if selected.iter().any(|path| path.ends_with(".py")) {
        Some("pyproject.toml")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGE_PROMPT: &str = "Identify the authoritative evidence needed to change the retry limit from 3 to 5 without touching generated mirrors.";

    fn write_fixture(root: &Path, attempts: usize) {
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(
            root.join("web/retry.ts"),
            format!(
                "export const MAX_RETRY_ATTEMPTS = {attempts};\n\n\
                 export function shouldRetry(attempt: number): boolean {{\n  \
                 return attempt < MAX_RETRY_ATTEMPTS;\n}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"orchard-web","version":"0.1.0"}"#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn private_checkpoint_reconstructs_current_source() {
        let corpus = tempfile::tempdir().unwrap();
        write_fixture(corpus.path(), 3);
        let root = corpus.path().display().to_string();
        let permissions = Arc::new(Permissions::for_roots(&[root], &[]).unwrap());
        let contributor = StructuralContextContributorV1::new(permissions);
        let changed = corpus.path().to_path_buf();

        let evidence =
            scope_without_interactive_prompts(contributor.contribute_with_test_checkpoint(
                StructuralContextRequestV1::new(
                    StructuralContextTaskV1::ChangeEvidence,
                    CHANGE_PROMPT,
                    corpus.path(),
                ),
                || write_fixture(&changed, 5),
            ))
            .await
            .unwrap();

        assert!(evidence.rendered().contains("MAX_RETRY_ATTEMPTS = 5"));
        assert!(!evidence.rendered().contains("MAX_RETRY_ATTEMPTS = 3"));
    }

    #[tokio::test]
    async fn private_checkpoint_reclassifies_current_symbol_roles() {
        let corpus = tempfile::tempdir().unwrap();
        write_fixture(corpus.path(), 3);
        let root = corpus.path().display().to_string();
        let permissions = Arc::new(Permissions::for_roots(&[root], &[]).unwrap());
        let contributor = StructuralContextContributorV1::new(permissions);
        let changed_path = corpus.path().join("web/retry.ts");

        let evidence = scope_without_interactive_prompts(
            contributor.contribute_with_test_checkpoint(
                StructuralContextRequestV1::new(
                    StructuralContextTaskV1::ChangeEvidence,
                    CHANGE_PROMPT,
                    corpus.path(),
                ),
                || {
                    std::fs::write(
                        &changed_path,
                        "MAX_RETRY_ATTEMPTS; // current reference, not a definition\n\nexport function shouldRetry(attempt: number): boolean {\n  return attempt < MAX_RETRY_ATTEMPTS;\n}\n",
                    )
                    .unwrap();
                },
            ),
        )
        .await
        .unwrap();

        assert_eq!(evidence.definitions(), 0);
        assert_eq!(evidence.references(), 2);
        assert!(evidence.rendered().starts_with(
            "structural_selector> \"MAX_RETRY_ATTEMPTS\" — 0 definition(s), 2 reference(s)\n"
        ));
        assert!(!evidence
            .rendered()
            .contains("— 1 definition(s), 1 reference(s)"));
    }

    #[tokio::test]
    async fn private_checkpoint_revocation_returns_no_evidence() {
        let corpus = tempfile::tempdir().unwrap();
        write_fixture(corpus.path(), 3);
        let root = corpus.path().display().to_string();
        let permissions =
            Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());
        let contributor = StructuralContextContributorV1::new(permissions.clone());

        let result =
            scope_without_interactive_prompts(contributor.contribute_with_test_checkpoint(
                StructuralContextRequestV1::new(
                    StructuralContextTaskV1::ChangeEvidence,
                    CHANGE_PROMPT,
                    corpus.path(),
                ),
                || {
                    permissions.revoke_read_root(&root).unwrap();
                },
            ))
            .await;

        assert!(matches!(
            result,
            Err(StructuralContextErrorV1::Observation(
                ToolError::PolicyDenied(_)
            ))
        ));
    }

    #[tokio::test]
    async fn private_checkpoint_rejects_shifted_or_removed_hits_without_partial_evidence() {
        for replacement in [
            "\nexport const MAX_RETRY_ATTEMPTS = 5;\n\nexport function shouldRetry(attempt: number): boolean {\n  return attempt < MAX_RETRY_ATTEMPTS;\n}\n",
            "export const ANOTHER_LIMIT = 5;\n",
        ] {
            let corpus = tempfile::tempdir().unwrap();
            write_fixture(corpus.path(), 3);
            let root = corpus.path().display().to_string();
            let permissions =
                Arc::new(Permissions::for_roots(std::slice::from_ref(&root), &[]).unwrap());
            let contributor = StructuralContextContributorV1::new(permissions);
            let changed_path = corpus.path().join("web/retry.ts");

            let result = scope_without_interactive_prompts(
                contributor.contribute_with_test_checkpoint(
                    StructuralContextRequestV1::new(
                        StructuralContextTaskV1::ChangeEvidence,
                        CHANGE_PROMPT,
                        corpus.path(),
                    ),
                    || std::fs::write(&changed_path, replacement).unwrap(),
                ),
            )
            .await;

            assert!(matches!(
                result,
                Err(StructuralContextErrorV1::SourceChanged { ref path })
                    if path == "web/retry.ts"
            ));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn denied_private_and_symlink_candidates_never_enter_production_evidence() {
        let corpus = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_fixture(corpus.path(), 3);
        std::fs::create_dir_all(corpus.path().join("private")).unwrap();
        std::fs::write(
            corpus.path().join("private/retry.ts"),
            "export const MAX_RETRY_ATTEMPTS = 777; // PRIVATE_CANARY\n",
        )
        .unwrap();
        std::fs::write(
            outside.path().join("retry.ts"),
            "export const MAX_RETRY_ATTEMPTS = 888; // DENIED_SYMLINK_CANARY\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("retry.ts"),
            corpus.path().join("web/symlink-retry.ts"),
        )
        .unwrap();

        let root = corpus.path().display().to_string();
        let permissions = Arc::new(Permissions::for_roots(&[root], &[]).unwrap());
        permissions
            .protect_read_path(&corpus.path().join("private").display().to_string())
            .unwrap();
        let contributor = StructuralContextContributorV1::new(permissions);
        let evidence = contributor
            .contribute(StructuralContextRequestV1::new(
                StructuralContextTaskV1::ChangeEvidence,
                CHANGE_PROMPT,
                corpus.path(),
            ))
            .await
            .unwrap();

        assert!(evidence.rendered().contains("MAX_RETRY_ATTEMPTS = 3"));
        assert!(!evidence.rendered().contains("PRIVATE_CANARY"));
        assert!(!evidence.rendered().contains("DENIED_SYMLINK_CANARY"));
        assert!(!evidence.rendered().contains("private/retry.ts"));
        assert!(!evidence.rendered().contains("symlink-retry.ts"));
        assert!(!evidence.rendered().contains("777"));
        assert!(!evidence.rendered().contains("888"));
    }
}
