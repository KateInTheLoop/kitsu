use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use kitsu_tools::semantic::{
    StructuralContextContributorV1, StructuralContextErrorV1, StructuralContextRequestV1,
    StructuralContextTaskV1,
};
use kitsu_tools::{Decision, Mode, PermissionRequest, Permissions, Prompter, Registry, ToolError};

const LOCATE_PROMPT: &str = "Locate the definition and production use of normalize_slug.";
const EXPLAIN_PROMPT: &str =
    "Explain where compute_total is defined, called, and how it rounds invoice totals.";
const CHANGE_PROMPT: &str = "Identify the authoritative evidence needed to change the retry limit from 3 to 5 without touching generated mirrors.";

const LOCATE_OUTPUT: &str =
    "structural_selector> \"normalize_slug\" — 1 definition(s), 1 reference(s)\n\
src/lib.rs:4: format!(\"/products/{}\", slug::normalize_slug(name))\n\
src/slug.rs:1: pub fn normalize_slug(value: &str) -> String {\n\
src/slug.rs:2: value.trim().to_ascii_lowercase().replace(' ', \"-\")\n\
src/slug.rs:3: }\n";

const EXPLAIN_OUTPUT: &str =
    "structural_selector> \"compute_total\" — 1 definition(s), 2 reference(s)\n\
service/invoice.py:1: from decimal import Decimal, ROUND_HALF_UP\n\
service/invoice.py:4: def compute_total(subtotal: Decimal, tax: Decimal) -> Decimal:\n\
service/invoice.py:5: \"\"\"Add tax and round the payable total to cents.\"\"\"\n\
service/invoice.py:6: return (subtotal + tax).quantize(Decimal(\"0.01\"), rounding=ROUND_HALF_UP)\n\
service/report.py:1: from invoice import compute_total\n\
service/report.py:5: return f\"Total: {compute_total(subtotal, tax)}\"\n";

const CHANGE_OUTPUT: &str =
    "structural_selector> \"MAX_RETRY_ATTEMPTS\" — 1 definition(s), 1 reference(s)\n\
web/retry.ts:1: export const MAX_RETRY_ATTEMPTS = 3;\n\
web/retry.ts:3: export function shouldRetry(attempt: number): boolean {\n\
web/retry.ts:4: return attempt < MAX_RETRY_ATTEMPTS;\n\
manifest> $CORPUS/package.json: package.json (Node) — orchard-web v0.1.0\n";

const FROZEN_C122_CORPUS: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        include_str!("fixtures/semantic_evidence/corpus/Cargo.toml"),
    ),
    (
        "package.json",
        include_str!("fixtures/semantic_evidence/corpus/package.json"),
    ),
    (
        "src/slug.rs",
        include_str!("fixtures/semantic_evidence/corpus/src/slug.rs"),
    ),
    (
        "src/lib.rs",
        include_str!("fixtures/semantic_evidence/corpus/src/lib.rs"),
    ),
    (
        "service/invoice.py",
        include_str!("fixtures/semantic_evidence/corpus/service/invoice.py"),
    ),
    (
        "service/report.py",
        include_str!("fixtures/semantic_evidence/corpus/service/report.py"),
    ),
    (
        "web/retry.ts",
        include_str!("fixtures/semantic_evidence/corpus/web/retry.ts"),
    ),
    (
        "generated/retry_snapshot.ts",
        include_str!("fixtures/semantic_evidence/corpus/generated/retry_snapshot.ts"),
    ),
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/semantic_evidence/corpus")
}

fn permissions_for(root: &Path) -> Arc<Permissions> {
    Arc::new(Permissions::for_roots(&[root.display().to_string()], &[]).unwrap())
}

fn materialize_frozen_c122_corpus(root: &Path) {
    for (relative, content) in FROZEN_C122_CORPUS {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("fixture path has a parent")).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

async fn contribute(
    task: StructuralContextTaskV1,
    prompt: &str,
    root: &Path,
) -> Result<String, StructuralContextErrorV1> {
    let contributor = StructuralContextContributorV1::new(permissions_for(root));
    let evidence = contributor
        .contribute(StructuralContextRequestV1::new(task, prompt, root))
        .await?;
    Ok(evidence.rendered().to_string())
}

fn normalize_root(output: &str, root: &Path) -> String {
    output.replace(&root.display().to_string(), "$CORPUS")
}

#[tokio::test]
async fn public_contributor_preserves_the_three_frozen_selector_outputs_byte_for_byte() {
    // Cycle 125 added hostile fixture source files beside the frozen corpus.
    // They are inputs to a separate explicit-authority test, not part of the
    // Cycle 122 authorized benchmark root. Re-materialize exactly that root so
    // this test does not silently invent a deny boundary for ordinary files.
    let corpus = tempfile::tempdir().unwrap();
    materialize_frozen_c122_corpus(corpus.path());
    let root = corpus.path().to_path_buf();
    let cases = [
        (
            StructuralContextTaskV1::Locate,
            LOCATE_PROMPT,
            LOCATE_OUTPUT,
            286,
        ),
        (
            StructuralContextTaskV1::Explain,
            EXPLAIN_PROMPT,
            EXPLAIN_OUTPUT,
            517,
        ),
        (
            StructuralContextTaskV1::ChangeEvidence,
            CHANGE_PROMPT,
            CHANGE_OUTPUT,
            331,
        ),
    ];

    let mut total = 0;
    for (task, prompt, expected, expected_bytes) in cases {
        let first = normalize_root(&contribute(task, prompt, &root).await.unwrap(), &root);
        let second = normalize_root(&contribute(task, prompt, &root).await.unwrap(), &root);
        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert_eq!(first.len(), expected_bytes);
        total += first.len();
    }
    assert_eq!(total, 1_134);
}

#[test]
fn construction_is_effect_free_and_never_registers_a_tool() {
    let root = corpus_root();
    let canary = std::fs::read(root.join("web/retry.ts")).unwrap();
    let _contributor = StructuralContextContributorV1::new(permissions_for(&root));

    let registry = Registry::new();
    assert!(registry.descriptors().is_empty());
    assert!(!registry.is_registered("semantic_context"));
    assert_eq!(std::fs::read(root.join("web/retry.ts")).unwrap(), canary);
}

struct CountingPrompter(AtomicUsize);

impl Prompter for CountingPrompter {
    fn ask(&self, _request: &PermissionRequest) -> Decision {
        self.0.fetch_add(1, Ordering::SeqCst);
        Decision::AllowAlways
    }
}

#[tokio::test]
async fn caller_supplied_root_is_not_authority_and_ask_mode_never_prompts() {
    let root = corpus_root();
    let prompts = Arc::new(CountingPrompter(AtomicUsize::new(0)));
    let permissions = Arc::new(
        Permissions::new(&[], &[], Mode::Ask)
            .unwrap()
            .with_prompter(prompts.clone()),
    );
    let contributor = StructuralContextContributorV1::new(permissions);

    let error = contributor
        .contribute(StructuralContextRequestV1::new(
            StructuralContextTaskV1::Locate,
            LOCATE_PROMPT,
            &root,
        ))
        .await
        .err()
        .expect("ungranted request must fail");

    assert!(matches!(
        error,
        StructuralContextErrorV1::Observation(ToolError::PolicyDenied(_))
    ));
    assert_eq!(prompts.0.load(Ordering::SeqCst), 0);
}

fn write_retry_fixture(root: &Path, attempts: usize) {
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
async fn a_later_invocation_reobserves_current_source_without_a_cache() {
    let corpus = tempfile::tempdir().unwrap();
    write_retry_fixture(corpus.path(), 3);
    let contributor = StructuralContextContributorV1::new(permissions_for(corpus.path()));
    let request = || {
        StructuralContextRequestV1::new(
            StructuralContextTaskV1::ChangeEvidence,
            CHANGE_PROMPT,
            corpus.path(),
        )
    };

    let before = contributor.contribute(request()).await.unwrap();
    write_retry_fixture(corpus.path(), 5);
    let after = contributor.contribute(request()).await.unwrap();

    assert!(before.rendered().contains("MAX_RETRY_ATTEMPTS = 3"));
    assert!(after.rendered().contains("MAX_RETRY_ATTEMPTS = 5"));
    assert!(!after.rendered().contains("MAX_RETRY_ATTEMPTS = 3"));
}

#[tokio::test]
async fn prompt_and_rendered_evidence_limits_fail_closed() {
    let root = corpus_root();
    let contributor = StructuralContextContributorV1::new(permissions_for(&root));
    let oversized_prompt = "x".repeat(4 * 1024 + 1);
    assert!(matches!(
        contributor
            .contribute(StructuralContextRequestV1::new(
                StructuralContextTaskV1::Locate,
                oversized_prompt,
                &root,
            ))
            .await,
        Err(StructuralContextErrorV1::InvalidRequest(_))
    ));

    let corpus = tempfile::tempdir().unwrap();
    std::fs::write(
        corpus.path().join("large.rs"),
        format!(
            "const HUGE_NEEDLE: usize = 1;\n{}\nfn use_it() {{ let _ = HUGE_NEEDLE; }}\n",
            "z".repeat(5 * 1024)
        ),
    )
    .unwrap();
    let contributor = StructuralContextContributorV1::new(permissions_for(corpus.path()));
    assert!(matches!(
        contributor
            .contribute(StructuralContextRequestV1::new(
                StructuralContextTaskV1::Explain,
                "Explain HUGE_NEEDLE.",
                corpus.path(),
            ))
            .await,
        Err(StructuralContextErrorV1::OutputLimit { max_bytes: 4_096 })
    ));
}
