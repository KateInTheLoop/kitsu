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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kitsu_providers::ToolDescriptor;
use kitsu_tools::{HandlerFn, Permissions, ToolError};

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

    /// Bodies of `always: true` skills, concatenated (loaded up front).
    pub fn always_bodies(&self) -> String {
        self.skills
            .iter()
            .filter(|s| s.always && !s.body.is_empty())
            .map(|s| format!("# Skill: {}\n{}", s.name, s.body))
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
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort(); // deterministic order
    for p in paths {
        let md = p.join("SKILL.md");
        if !md.is_file() {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&md) {
            let skill = parse(&content, &p);
            if !skill.name.is_empty() && seen.insert(skill.name.clone()) {
                skills.push(skill);
            }
        }
    }
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

/// Grant read access to `skill`'s directory (so its bundled files are reachable
/// via `fs_read_file` without a broad `--allow-path`) and render its body plus a
/// list of bundled files and declared links. Shared by the `skill` tool,
/// `/skill`, and `--skill` (Stage S2). Links are surfaced, never fetched.
pub fn load_skill(skill: &Skill, perms: &Permissions) -> String {
    let _ = perms.grant_read_root(&skill.dir.to_string_lossy());
    let mut out = format!("# Skill: {}\n{}", skill.name, skill.body);
    let files = bundled_files(&skill.dir);
    if !files.is_empty() {
        out.push_str("\n\nBundled files (read with fs_read_file):\n");
        for f in &files {
            out.push_str(&format!("- {f}\n"));
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

/// The skill's bundled resource files (everything in its dir except `SKILL.md`).
fn bundled_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_name() != "SKILL.md")
        .filter(|e| e.path().is_file())
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();
    files.sort();
    files
}

/// Build the `skill` tool: `skill(name)` loads a skill's instructions (and grants
/// read to its dir for bundled files) on demand. Registered only when skills exist.
pub fn skill_tool(skills: Arc<SkillSet>, perms: Arc<Permissions>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "skill".to_string(),
        description: "Load a named skill's full instructions into context. Call this when a task \
                      matches a skill's description (skills are listed in the system prompt). \
                      Returns the instructions and any bundled files you may then read."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "name": {"type": "string", "description": "The skill name to load"} },
            "required": ["name"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let skills = skills.clone();
        let perms = perms.clone();
        Box::pin(async move {
            let a: SkillArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            match skills.get(&a.name) {
                Some(s) => Ok(load_skill(s, &perms)),
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
                },
                Skill {
                    name: "review".into(),
                    description: "reviews".into(),
                    when_to_use: String::new(),
                    always: false,
                    body: String::new(),
                    links: Vec::new(),
                    dir: PathBuf::new(),
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
        let set = Arc::new(SkillSet {
            skills: vec![Skill {
                name: "greet".into(),
                description: "say hi".into(),
                when_to_use: String::new(),
                always: false,
                body: "Say hello warmly.".into(),
                links: Vec::new(),
                dir: PathBuf::new(),
            }],
        });
        let perms = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let (desc, handler) = skill_tool(set, perms);
        assert_eq!(desc.name, "skill");
        let ok = handler(r#"{"name":"greet"}"#.to_string()).await.unwrap();
        assert!(ok.contains("Say hello warmly."));
        assert!(handler(r#"{"name":"nope"}"#.to_string()).await.is_err());
        assert!(handler("not json".to_string()).await.is_err());
    }

    #[tokio::test]
    async fn load_skill_grants_read_lists_files_and_surfaces_links_without_fetching() {
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

        let out = load_skill(&skill, &perms);
        assert!(out.contains("checklist.md"), "bundled file is listed");
        assert!(out.contains("https://example.com/spec"), "link surfaced");
        assert!(out.contains("NOT fetched"), "links are not fetched");
        // Loading granted read to the skill dir, so now it's readable.
        assert!(perms.check_read(&checklist).await.is_ok());
    }
}
