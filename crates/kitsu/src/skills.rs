//! Skills — opt-in, LOCAL capability bundles (design skills-and-context-
//! provenance §2, Stage S1).
//!
//! A skill is a directory with a `SKILL.md`: YAML-ish frontmatter (`name`,
//! `description`, `when_to_use`, `always`) plus a markdown body. Only the
//! frontmatter index is always in context; the body loads on demand when the
//! model calls the `skill` tool (or the user runs `/skill <name>`). Skills are
//! discovered locally under `.kitsu/skills/` (project) and `~/.kitsu/skills/`
//! (user) — never fetched from the network (prompt-injection into a tool-enabled
//! agent). A skill's text is context, not privileged: every tool action it
//! suggests still goes through the permission model.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kitsu_providers::ToolDescriptor;
#[cfg(test)]
use kitsu_tools::Permissions;
use kitsu_tools::{HandlerFn, ToolError};

const MAX_SKILL_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_SKILL_RESOURCE_BYTES: usize = 32 * 1024;
const MAX_SKILL_RESOURCES_TOTAL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillResource {
    name: String,
    content: String,
}

/// A parsed skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub when_to_use: String,
    /// Load the body up front rather than on demand (use sparingly).
    pub always: bool,
    pub body: String,
    /// Declared reference links — surfaced to the user, never auto-fetched.
    pub links: Vec<String>,
    /// The skill's directory (its bundled resources live here — Stage S2).
    pub dir: PathBuf,
    /// Bounded UTF-8 resources captured through no-follow file descriptors at
    /// discovery time. Loading a skill never widens filesystem authority.
    resources: Vec<SkillResource>,
}

/// The discovered skills for a session.
#[derive(Debug, Default, Clone)]
pub struct SkillSet {
    skills: Vec<Skill>,
}

impl SkillSet {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.skills.iter().map(|s| s.name.as_str()).collect()
    }

    /// One line per skill for the always-on system-prompt index.
    pub fn index(&self, filter: &str) -> String {
        let f = filter.trim().to_lowercase();
        self.skills
            .iter()
            .filter(|s| {
                f.is_empty()
                    || s.name.to_lowercase().contains(&f)
                    || s.description.to_lowercase().contains(&f)
            })
            .map(|s| format!("- {} — {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Terminal-safe index: every repository-controlled field is forced onto
    /// one row before the rows are joined.
    pub fn terminal_index(&self, filter: &str) -> String {
        fn inline(value: &str) -> String {
            kitsu_render::sanitize_terminal_text(value)
                .chars()
                .map(|ch| if matches!(ch, '\n' | '\t') { ' ' } else { ch })
                .collect()
        }

        let f = filter.trim().to_lowercase();
        self.skills
            .iter()
            .filter(|skill| {
                f.is_empty()
                    || skill.name.to_lowercase().contains(&f)
                    || skill.description.to_lowercase().contains(&f)
            })
            .map(|skill| format!("- {} — {}", inline(&skill.name), inline(&skill.description)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Bodies of `always: true` skills, concatenated (loaded up front).
    pub fn always_bodies(&self) -> String {
        self.skills
            .iter()
            .filter(|s| s.always && !s.body.is_empty())
            .map(load_skill)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Discover skills: project (`.kitsu/skills/*` walking up to the git/project
    /// root) then user (`~/.kitsu/skills/*`). Project wins on name collision.
    /// Returns empty when `disabled` (tests / `KITSU_NO_PROJECT_CONTEXT`).
    pub fn discover(start: &Path, disabled: bool) -> SkillSet {
        if disabled {
            return SkillSet::default();
        }
        let mut skills = Vec::new();
        let mut seen = HashSet::new();

        let mut dir = Some(start);
        while let Some(d) = dir {
            collect_from(&d.join(".kitsu").join("skills"), &mut skills, &mut seen);
            if d.join(".git").exists() {
                break;
            }
            dir = d.parent();
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            if !home.as_os_str().is_empty() {
                collect_from(&home.join(".kitsu").join("skills"), &mut skills, &mut seen);
            }
        }
        SkillSet { skills }
    }
}

/// Read every `<dir>/*/SKILL.md`, parsing and appending skills whose name hasn't
/// been seen yet.
fn collect_from(dir: &Path, skills: &mut Vec<Skill>, seen: &mut HashSet<String>) {
    // Skill roots and their `.kitsu` parent are configuration boundaries, not
    // shortcuts to arbitrary filesystem locations. Ignore symlinked roots so a
    // repository cannot redirect discovery outside the project or user skill
    // namespace.
    let regular_directory = |path: &Path| {
        std::fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
            .unwrap_or(false)
    };
    if !regular_directory(dir)
        || dir
            .parent()
            .is_some_and(|parent| !regular_directory(parent))
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    paths.sort(); // deterministic order
    for p in paths {
        let md = p.join("SKILL.md");
        let Some(content) = read_regular_utf8_no_follow(&md, MAX_SKILL_MANIFEST_BYTES) else {
            continue;
        };
        let mut skill = parse(&content, &p);
        skill.resources = collect_resources(&p);
        if !skill.name.is_empty() && seen.insert(skill.name.clone()) {
            skills.push(skill);
        }
    }
}

fn read_regular_utf8_no_follow(path: &Path, max_bytes: usize) -> Option<String> {
    read_regular_utf8_no_follow_with_hook(path, max_bytes, || {})
}

fn read_regular_utf8_no_follow_with_hook<F>(
    path: &Path,
    max_bytes: usize,
    after_open: F,
) -> Option<String>
where
    F: FnOnce(),
{
    #[cfg(unix)]
    let file = crate::attachments::open_regular_file_no_follow(path).ok()?;

    #[cfg(not(unix))]
    let file = {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return None;
        }
        std::fs::OpenOptions::new().read(true).open(path).ok()?
    };

    after_open();
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > max_bytes || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn collect_resources(dir: &Path) -> Vec<SkillResource> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_name() != "SKILL.md")
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .collect();
    paths.sort();

    let mut total = 0usize;
    let mut resources = Vec::new();
    for path in paths {
        let remaining = MAX_SKILL_RESOURCES_TOTAL_BYTES.saturating_sub(total);
        if remaining == 0 {
            break;
        }
        let cap = remaining.min(MAX_SKILL_RESOURCE_BYTES);
        let Some(content) = read_regular_utf8_no_follow(&path, cap) else {
            continue;
        };
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            continue;
        };
        total = total.saturating_add(content.len());
        resources.push(SkillResource { name, content });
    }
    resources
}

/// Parse a `SKILL.md`: optional `---`-delimited frontmatter (`key: value` lines)
/// then a markdown body. Falls back to the directory name when `name` is absent.
pub fn parse(content: &str, dir: &Path) -> Skill {
    let default_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut skill = Skill {
        name: default_name,
        description: String::new(),
        when_to_use: String::new(),
        always: false,
        body: String::new(),
        links: Vec::new(),
        dir: dir.to_path_buf(),
        resources: Vec::new(),
    };

    let trimmed = content.trim_start_matches('\u{feff}');
    if let Some(rest) = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))
    {
        // Frontmatter until the next line that is exactly `---`.
        let mut lines = rest.lines();
        let mut fm = Vec::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim() == "---" {
                closed = true;
                break;
            }
            fm.push(line);
        }
        for line in fm {
            if let Some((k, v)) = line.split_once(':') {
                let v = v.trim().to_string();
                match k.trim().to_lowercase().as_str() {
                    "name" if !v.is_empty() => skill.name = v,
                    "description" => skill.description = v,
                    "when_to_use" => skill.when_to_use = v,
                    "always" => skill.always = v.eq_ignore_ascii_case("true"),
                    "links" => {
                        skill.links = v
                            .split([',', ' '])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect()
                    }
                    _ => {}
                }
            }
        }
        if closed {
            skill.body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        }
    } else {
        skill.body = trimmed.trim().to_string();
    }
    skill
}

#[derive(serde::Deserialize)]
struct SkillArgs {
    name: String,
}

/// Render a skill body plus bounded UTF-8 resources captured during discovery.
/// Loading never widens filesystem authority. Shared by the `skill` tool,
/// `/skill`, and `--skill`; declared links are surfaced, never fetched.
pub fn load_skill(skill: &Skill) -> String {
    let mut out = format!("# Skill: {}\n{}", skill.name, skill.body);
    if !skill.resources.is_empty() {
        out.push_str("\n\nBundled resources (captured safely during discovery):\n");
        for resource in &skill.resources {
            out.push_str(&format!(
                "\n<skill-resource name={:?}>\n{}\n</skill-resource>\n",
                resource.name, resource.content
            ));
        }
    }
    if !skill.links.is_empty() {
        out.push_str("\nReference links (open manually — NOT fetched by kitsu):\n");
        for l in &skill.links {
            out.push_str(&format!("- {l}\n"));
        }
    }
    out
}

/// Build the `skill` tool: `skill(name)` loads instructions and the bounded
/// resource snapshot on demand. Registered only when skills exist.
pub fn skill_tool(skills: Arc<SkillSet>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "skill".to_string(),
        description: "Load a named skill's full instructions into context. Call this when a task \
                      matches a skill's description (skills are listed in the system prompt). \
                      Returns the instructions and bounded bundled resource contents."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "name": {"type": "string", "description": "The skill name to load"} },
            "required": ["name"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let skills = skills.clone();
        Box::pin(async move {
            let a: SkillArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            match skills.get(&a.name) {
                Some(s) => Ok(load_skill(s)),
                None => Err(ToolError::ExecError(format!(
                    "no skill named '{}'; available: {}",
                    a.name,
                    skills.names().join(", ")
                ))),
            }
        })
    });
    (descriptor, handler)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nname: code-review\ndescription: Review a diff for bugs.\n\
when_to_use: When asked to review code.\nalways: false\n---\n# Code review\nStep 1. Read the file.\n";

    #[test]
    fn parses_frontmatter_and_body() {
        let s = parse(SAMPLE, Path::new("/skills/cr"));
        assert_eq!(s.name, "code-review");
        assert_eq!(s.description, "Review a diff for bugs.");
        assert_eq!(s.when_to_use, "When asked to review code.");
        assert!(!s.always);
        assert!(s.body.contains("# Code review"));
        assert!(s.body.contains("Step 1"));
    }

    #[test]
    fn name_defaults_to_dir_when_absent() {
        let s = parse(
            "# just a body\nno frontmatter",
            Path::new("/skills/my-skill"),
        );
        assert_eq!(s.name, "my-skill");
        assert!(s.body.contains("just a body"));
    }

    #[test]
    fn always_true_parsed() {
        let s = parse("---\nname: x\nalways: TRUE\n---\nbody", Path::new("/x"));
        assert!(s.always);
    }

    #[test]
    fn index_and_names() {
        let set = SkillSet {
            skills: vec![
                Skill {
                    name: "a".into(),
                    description: "does A".into(),
                    when_to_use: String::new(),
                    always: false,
                    body: String::new(),
                    links: Vec::new(),
                    dir: PathBuf::new(),
                    resources: Vec::new(),
                },
                Skill {
                    name: "review".into(),
                    description: "reviews".into(),
                    when_to_use: String::new(),
                    always: false,
                    body: String::new(),
                    links: Vec::new(),
                    dir: PathBuf::new(),
                    resources: Vec::new(),
                },
            ],
        };
        assert_eq!(set.names(), vec!["a", "review"]);
        assert!(set.index("").contains("- a — does A"));
        // filter
        assert!(set.index("rev").contains("review"));
        assert!(!set.index("rev").contains("- a —"));
    }

    #[test]
    fn terminal_index_keeps_each_repository_field_on_one_row() {
        let set = SkillSet {
            skills: vec![Skill {
                name: "safe\nforged".into(),
                description: "description\tspoof\u{1b}[2J".into(),
                when_to_use: String::new(),
                always: false,
                body: String::new(),
                links: Vec::new(),
                dir: PathBuf::new(),
                resources: Vec::new(),
            }],
        };
        let rendered = set.terminal_index("");
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\t'));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("safe forged"));
    }

    #[test]
    fn discover_finds_project_skills_and_stops_at_git_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        let sk = root.path().join(".kitsu/skills/greet");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(
            sk.join("SKILL.md"),
            "---\nname: greet\ndescription: say hi\n---\nsay hi",
        )
        .unwrap();
        let sub = root.path().join("crates/x");
        std::fs::create_dir_all(&sub).unwrap();

        let set = SkillSet::discover(&sub, false);
        assert!(set.get("greet").is_some());
        assert_eq!(set.get("greet").unwrap().description, "say hi");
    }

    #[test]
    fn discover_disabled_returns_empty() {
        let root = tempfile::tempdir().unwrap();
        assert!(SkillSet::discover(root.path(), true).is_empty());
    }

    #[test]
    fn collect_from_first_wins_on_name_collision() {
        // The first source collected (project, before user) wins a name clash.
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        for (d, desc) in [(&d1, "PROJECT"), (&d2, "USER")] {
            let s = d.path().join("dup");
            std::fs::create_dir_all(&s).unwrap();
            std::fs::write(
                s.join("SKILL.md"),
                format!("---\nname: dup\ndescription: {desc}\n---\nb"),
            )
            .unwrap();
        }
        let mut skills = Vec::new();
        let mut seen = HashSet::new();
        collect_from(d1.path(), &mut skills, &mut seen);
        collect_from(d2.path(), &mut skills, &mut seen);
        assert_eq!(skills.len(), 1, "the duplicate name is collected once");
        assert_eq!(skills[0].description, "PROJECT", "first source wins");
    }

    #[tokio::test]
    async fn skill_tool_returns_body_and_errors_on_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let set = Arc::new(SkillSet {
            skills: vec![Skill {
                name: "greet".into(),
                description: "say hi".into(),
                when_to_use: String::new(),
                always: false,
                body: "Say hello warmly.".into(),
                links: Vec::new(),
                dir: dir.path().to_path_buf(),
                resources: Vec::new(),
            }],
        });
        let (desc, handler) = skill_tool(set);
        assert_eq!(desc.name, "skill");
        let ok = handler(r#"{"name":"greet"}"#.to_string()).await.unwrap();
        assert!(ok.contains("Say hello warmly."));
        assert!(handler(r#"{"name":"nope"}"#.to_string()).await.is_err());
        assert!(handler("not json".to_string()).await.is_err());
    }

    #[tokio::test]
    async fn load_skill_captures_resources_without_granting_filesystem_access() {
        let dir = tempfile::tempdir().unwrap();
        let skdir = dir.path().join("cr");
        std::fs::create_dir_all(&skdir).unwrap();
        std::fs::write(
            skdir.join("SKILL.md"),
            "---\nname: cr\nlinks: https://example.com/spec\n---\nbody",
        )
        .unwrap();
        std::fs::write(skdir.join("checklist.md"), "1. check").unwrap();
        let skill = parse(
            &std::fs::read_to_string(skdir.join("SKILL.md")).unwrap(),
            &skdir,
        );
        assert_eq!(skill.links, vec!["https://example.com/spec"]);

        let perms = Permissions::for_roots(&[], &[]).unwrap();
        let checklist = skdir.join("checklist.md");
        // Before loading, the bundled file is NOT readable (deny-by-default).
        assert!(perms.check_read(&checklist).await.is_err());

        let mut skill = skill;
        skill.resources = collect_resources(&skdir);
        let out = load_skill(&skill);
        assert!(out.contains("checklist.md"), "bundled file is listed");
        assert!(out.contains("1. check"), "bundled contents are captured");
        assert!(out.contains("https://example.com/spec"), "link surfaced");
        assert!(out.contains("NOT fetched"), "links are not fetched");
        // Loading does not widen filesystem authority; resources are served
        // from the bounded discovery snapshot instead.
        assert!(perms.check_read(&checklist).await.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn discovery_ignores_symlinked_skill_directories_and_manifests() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".git")).unwrap();
        let skills = root.path().join(".kitsu/skills");
        std::fs::create_dir_all(&skills).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("SKILL.md"),
            "---\nname: escaped-dir\n---\nsecret",
        )
        .unwrap();
        symlink(outside.path(), skills.join("escaped-dir")).unwrap();

        let linked_manifest = skills.join("linked-manifest");
        std::fs::create_dir_all(&linked_manifest).unwrap();
        symlink(
            outside.path().join("SKILL.md"),
            linked_manifest.join("SKILL.md"),
        )
        .unwrap();

        let set = SkillSet::discover(root.path(), false);
        assert!(set.get("escaped-dir").is_none());
        assert!(set.get("linked-manifest").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn opened_manifest_stays_pinned_across_an_ancestor_symlink_swap() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let skill_dir = root.path().join("skill");
        let moved = root.path().join("moved-skill");
        let outside = root.path().join("outside");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let manifest = skill_dir.join("SKILL.md");
        std::fs::write(&manifest, "AUTHORIZED").unwrap();
        std::fs::write(outside.join("SKILL.md"), "PRIVATE").unwrap();

        let content = read_regular_utf8_no_follow_with_hook(&manifest, 1024, || {
            std::fs::rename(&skill_dir, &moved).unwrap();
            symlink(&outside, &skill_dir).unwrap();
        })
        .unwrap();

        assert_eq!(content, "AUTHORIZED");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn load_skill_never_grants_a_symlinked_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alias = root.path().join("skill-alias");
        symlink(outside.path(), &alias).unwrap();
        let skill = Skill {
            name: "escaped".into(),
            description: String::new(),
            when_to_use: String::new(),
            always: false,
            body: "body".into(),
            links: Vec::new(),
            dir: alias,
            resources: Vec::new(),
        };
        let perms = Permissions::for_roots(&[], &[]).unwrap();

        assert!(load_skill(&skill).contains("body"));
        assert!(perms.check_read(outside.path()).await.is_err());
    }
}
