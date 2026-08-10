//! Minimal `llms.txt` project-context loader (design §3.6 / §2.4, Stage 4).
//!
//! Discovers and parses an [`llms.txt`](https://llmstxt.org/) so kitsu ingests a
//! curated, budget-aware project context instead of slurping arbitrary markdown.
//! Tolerant by design — only the H1 is guaranteed by the spec. Falls back to an
//! `AGENTS.md`/`CLAUDE.md` when no `llms.txt` is present.

use std::path::{Path, PathBuf};

use kitsu_core::context::estimate_str;

/// Parsed `llms.txt`: an H1 name, a blockquote summary, the required body, and
/// the budget-optional `## Optional` section.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LlmsTxt {
    pub name: String,
    pub summary: String,
    pub required: String,
    pub optional: String,
}

impl LlmsTxt {
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.summary.is_empty() && self.required.is_empty()
    }

    /// Render to a context block; include the `## Optional` section only if asked.
    pub fn render(&self, include_optional: bool) -> String {
        let mut out = String::new();
        if !self.name.is_empty() {
            out.push_str("# ");
            out.push_str(&self.name);
            out.push('\n');
        }
        if !self.summary.is_empty() {
            out.push_str("> ");
            out.push_str(&self.summary);
            out.push('\n');
        }
        if !self.required.is_empty() {
            out.push('\n');
            out.push_str(&self.required);
            out.push('\n');
        }
        if include_optional && !self.optional.is_empty() {
            out.push_str("\n## Optional\n");
            out.push_str(&self.optional);
            out.push('\n');
        }
        out
    }
}

/// Parse `llms.txt` content. Blockquotes before the first `## ` section are the
/// summary; the `## Optional` section (case-insensitive) is separated out.
pub fn parse(content: &str) -> LlmsTxt {
    let mut t = LlmsTxt::default();
    let mut summary = Vec::new();
    let mut required = Vec::new();
    let mut optional = Vec::new();
    let mut seen_h1 = false;
    let mut in_sections = false;
    let mut in_optional = false;
    let mut summary_done = false;
    for raw in content.lines() {
        let line = raw.trim_end().trim_start_matches('\u{feff}');
        if !seen_h1 {
            if let Some(rest) = line.trim_start().strip_prefix("# ") {
                t.name = rest.trim().to_string();
                seen_h1 = true;
            }
            continue;
        }
        let ls = line.trim_start();
        if let Some(rest) = ls.strip_prefix("## ") {
            in_sections = true;
            in_optional = rest.trim().eq_ignore_ascii_case("optional");
            if in_optional {
                continue; // skip the Optional heading itself
            }
        }
        if !in_sections {
            if let Some(q) = ls.strip_prefix("> ") {
                // Only the blockquote(s) right after the H1 are the summary; a
                // blockquote after prose belongs to the body.
                if summary_done {
                    required.push(line.to_string());
                } else {
                    summary.push(q.trim().to_string());
                }
            } else if !ls.is_empty() {
                summary_done = true;
                required.push(line.to_string());
            }
            continue;
        }
        if in_optional {
            optional.push(line.to_string());
        } else {
            required.push(line.to_string());
        }
    }
    t.summary = summary.join(" ");
    t.required = required.join("\n").trim().to_string();
    t.optional = optional.join("\n").trim().to_string();
    t
}

/// Discover a project-context file: a configured path, else `llms.txt` walking up
/// from `start` to the git/project root, else an `AGENTS.md`/`CLAUDE.md` fallback
/// found along the way.
pub fn discover(start: &Path, configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = configured {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }
    let mut dir = Some(start);
    let mut fallback: Option<PathBuf> = None;
    while let Some(d) = dir {
        let llms = d.join("llms.txt");
        if llms.is_file() {
            return Some(llms); // llms.txt always wins
        }
        if fallback.is_none() {
            for name in ["AGENTS.md", "CLAUDE.md"] {
                let c = d.join(name);
                if c.is_file() {
                    fallback = Some(c);
                    break;
                }
            }
        }
        if d.join(".git").exists() {
            break; // don't walk past the project root
        }
        dir = d.parent();
    }
    fallback
}

/// Load project context: discover, then parse (`llms.txt`) or read raw
/// (`AGENTS.md`/`CLAUDE.md`), rendering with the `## Optional` section included
/// only if the whole thing fits within `window/4` tokens. The result is capped at
/// `window/3` tokens so it can't swamp a small window (it lives in the protected
/// system prompt and isn't trimmed). Returns rendered context, token estimate,
/// and the exact source path so the UI can disclose what will be sent.
pub fn load(
    start: &Path,
    configured: Option<&Path>,
    window: Option<u64>,
) -> Option<(String, u64, PathBuf)> {
    let path = discover(start, configured)?;
    // Bound the read so a pathologically large file can't blow up memory; the
    // rendered context is capped again below.
    let content = read_capped(&path, 256 * 1024)?;
    let is_llms = path.file_name().map(|n| n == "llms.txt").unwrap_or(false);
    let optional_cap = window.map(|w| w / 4).unwrap_or(2048);
    // Leave the rest of the window for the base prompt, tool schemas, and the
    // conversation; project context is at most a third.
    let required_cap = window.map(|w| w / 3).unwrap_or(4096);

    let rendered = if is_llms {
        let parsed = parse(&content);
        if parsed.is_empty() {
            return None;
        }
        let full = parsed.render(true);
        if estimate_str(&full) <= optional_cap {
            full
        } else {
            parsed.render(false)
        }
    } else {
        if content.trim().is_empty() {
            return None;
        }
        content
    };

    let rendered = cap_to_tokens(rendered, required_cap);
    let tokens = estimate_str(&rendered);
    Some((rendered, tokens, path))
}

/// Read at most `max` bytes of a file as (lossy) UTF-8.
fn read_capped(path: &Path, max: u64) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(max)
        .read_to_end(&mut buf)
        .ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Truncate to at most `cap` tokens (~4 chars/token) on a char boundary.
fn cap_to_tokens(s: String, cap: u64) -> String {
    if estimate_str(&s) <= cap {
        return s;
    }
    let mut end = ((cap as usize) * 4).min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("\n…[project context truncated to fit the window]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\u{feff}# Acme\n> A tiny widget library.\nSome details.\n\n\
## Docs\n- [Readme](README.md): the readme\n- [Guide](guide.md)\n\n\
## Optional\n- [Changelog](CHANGELOG.md): history\n";

    #[test]
    fn parses_name_summary_required_and_optional() {
        let t = parse(SAMPLE);
        assert_eq!(t.name, "Acme");
        assert_eq!(t.summary, "A tiny widget library.");
        assert!(t.required.contains("## Docs"));
        assert!(t.required.contains("[Readme](README.md)"));
        assert!(t.required.contains("Some details."));
        assert!(
            !t.required.contains("Changelog"),
            "optional must be split out"
        );
        assert!(t.optional.contains("[Changelog](CHANGELOG.md)"));
    }

    #[test]
    fn render_includes_optional_only_when_asked() {
        let t = parse(SAMPLE);
        assert!(!t.render(false).contains("Changelog"));
        assert!(t.render(true).contains("Changelog"));
        assert!(t.render(false).contains("# Acme"));
    }

    #[test]
    fn empty_without_an_h1() {
        assert!(parse("no heading here\n- just a list").is_empty());
    }

    #[test]
    fn load_drops_optional_when_over_budget() {
        let dir = tempfile::tempdir().unwrap();
        let big_optional = "- [x](y): ".to_string() + &"z".repeat(4000);
        let content = format!("# P\n> sum\n## S\n- [a](b)\n## Optional\n{big_optional}\n");
        std::fs::write(dir.path().join("llms.txt"), content).unwrap();
        // Small window → window/4 optional budget is tiny → optional dropped.
        let (rendered, _, source) = load(dir.path(), None, Some(2000)).unwrap();
        assert!(rendered.contains("# P"));
        assert!(
            !rendered.contains("zzz"),
            "optional should be dropped under budget"
        );
        assert_eq!(source, dir.path().join("llms.txt"));
    }

    #[test]
    fn discover_prefers_llms_txt_over_agents_md_and_stops_at_git_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "agents").unwrap();
        let sub = root.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.path().join("llms.txt"), "# Root").unwrap();
        // From the subdir, walk up to the git root and find llms.txt (beats AGENTS.md).
        let found = discover(&sub, None).unwrap();
        assert_eq!(found.file_name().unwrap(), "llms.txt");
    }

    #[test]
    fn discover_falls_back_to_agents_md() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "agents").unwrap();
        let found = discover(root.path(), None).unwrap();
        assert_eq!(found.file_name().unwrap(), "AGENTS.md");
    }
    #[test]
    fn blockquote_after_prose_is_not_summary() {
        let t = parse("# N\n> real summary\nBody text.\n> an inline quote\n## Docs\n- [a](b)");
        assert_eq!(t.summary, "real summary");
        assert!(
            t.required.contains("an inline quote"),
            "post-prose quote goes to body"
        );
        assert!(t.required.contains("Body text."));
    }
}
