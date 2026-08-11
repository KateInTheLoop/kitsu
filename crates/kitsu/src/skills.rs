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
//! Filesystem skill discovery currently requires Unix descriptor-relative,
//! no-follow traversal. Other platforms return an empty skill set until an
//! equivalent handle-relative, no-reparse implementation is available.

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kitsu_providers::ToolDescriptor;
#[cfg(unix)]
use kitsu_tools::TrustedImplicitReadRoot;
use kitsu_tools::{HandlerFn, Permissions, ToolError};

#[cfg(unix)]
const MAX_SKILL_MANIFEST_BYTES: usize = 256 * 1024;
#[cfg(unix)]
const MAX_SKILL_RESOURCE_BYTES: usize = 32 * 1024;
#[cfg(unix)]
const MAX_SKILL_RESOURCES_TOTAL_BYTES: usize = 64 * 1024;

#[cfg(unix)]
#[derive(Default)]
struct SkillCollection {
    skills: Vec<Skill>,
    sources: Vec<SkillSource>,
    duplicates: Vec<SkillDuplicate>,
    seen: HashMap<String, usize>,
}

#[cfg(unix)]
impl SkillCollection {
    fn into_skill_set(self) -> SkillSet {
        SkillSet {
            skills: self.skills,
            sources: self.sources,
            duplicates: self.duplicates,
        }
    }
}

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
    /// Request loading the body up front rather than on demand. Honored
    /// automatically only for user-origin skills; project skills require an
    /// explicit `/skill`, `--skill`, or tool load.
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
    sources: Vec<SkillSource>,
    duplicates: Vec<SkillDuplicate>,
}

/// Trust boundary from which a skill was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOrigin {
    /// Repository-controlled `.kitsu/skills` content.
    Project,
    /// User-controlled `~/.kitsu/skills` content.
    User,
}

impl SkillOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

/// Exact manifest provenance retained for a selected or shadowed skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSource {
    pub origin: SkillOrigin,
    pub manifest: PathBuf,
}

/// A duplicate name which was ignored in favor of an earlier, more trusted
/// source. Keeping both origins makes shadowing visible to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDuplicate {
    pub name: String,
    pub selected: SkillSource,
    pub ignored: SkillSource,
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

    pub fn source(&self, name: &str) -> Option<&SkillSource> {
        self.skills
            .iter()
            .position(|skill| skill.name == name)
            .and_then(|index| self.sources.get(index))
    }

    pub fn duplicates(&self) -> &[SkillDuplicate] {
        &self.duplicates
    }

    fn provenance_suffix(&self, skill: &Skill) -> String {
        let Some(source) = self.source(&skill.name) else {
            return String::new();
        };
        let mut notes = vec![source.origin.as_str().to_string()];
        if source.origin == SkillOrigin::Project && skill.always {
            notes.push("always requires explicit load".to_string());
        }
        let ignored: Vec<&str> = self
            .duplicates()
            .iter()
            .filter(|duplicate| duplicate.name == skill.name)
            .map(|duplicate| duplicate.ignored.origin.as_str())
            .collect();
        if !ignored.is_empty() {
            notes.push(format!("duplicate {} ignored", ignored.join("/")));
        }
        format!(" [{}]", notes.join("; "))
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
            .map(|skill| {
                format!(
                    "- {}{} — {}",
                    skill.name,
                    self.provenance_suffix(skill),
                    skill.description
                )
            })
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
            .map(|skill| {
                format!(
                    "- {}{} — {}",
                    inline(&skill.name),
                    inline(&self.provenance_suffix(skill)),
                    inline(&skill.description)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Bodies of `always: true` skills, concatenated (loaded up front).
    pub fn always_bodies(&self) -> String {
        self.skills
            .iter()
            .filter(|skill| {
                skill.always
                    && !skill.body.is_empty()
                    && self
                        .source(&skill.name)
                        .map_or(true, |source| source.origin != SkillOrigin::Project)
            })
            .map(load_skill)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Discover user skills and, unless `no_project` is set, project skills.
    /// User skills win name collisions so repository content cannot shadow an
    /// explicitly requested `--skill`; every ignored origin remains available
    /// through [`Self::duplicates`]. Project `always: true` bodies are indexed
    /// but never injected automatically (they require `/skill`/`--skill`/tool
    /// loading). `no_project` does not disable user skills on supported Unix
    /// platforms. Non-Unix platforms return an empty set until secure
    /// no-reparse traversal is implemented.
    pub fn discover(start: &Path, no_project: bool, permissions: &Permissions) -> SkillSet {
        let user_skills = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| !home.as_os_str().is_empty())
            .map(|home| home.join(".kitsu").join("skills"));
        Self::discover_from(start, no_project, user_skills.as_deref(), permissions)
    }

    fn discover_from(
        start: &Path,
        no_project: bool,
        user_skills: Option<&Path>,
        permissions: &Permissions,
    ) -> SkillSet {
        #[cfg(not(unix))]
        {
            let _ = (start, no_project, user_skills, permissions);
            SkillSet::default()
        }

        #[cfg(unix)]
        {
            let mut collection = SkillCollection::default();

            // User-controlled skills are the trusted default and win collisions.
            if let Some(user_skills) = user_skills {
                if let Ok(trusted_root) =
                    permissions.authorize_trusted_implicit_read_root(user_skills)
                {
                    collect_from(
                        user_skills,
                        SkillOrigin::User,
                        &mut collection,
                        permissions,
                        Some(&trusted_root),
                    );
                }
            }
            if !no_project {
                let mut dir = Some(start);
                while let Some(d) = dir {
                    collect_from(
                        &d.join(".kitsu").join("skills"),
                        SkillOrigin::Project,
                        &mut collection,
                        permissions,
                        None,
                    );
                    if std::fs::symlink_metadata(d.join(".git")).is_ok() {
                        break;
                    }
                    dir = d.parent();
                }
            }
            collection.into_skill_set()
        }
    }
}

/// Read every `<dir>/*/SKILL.md`, parsing and appending skills whose name hasn't
/// been seen yet.
#[cfg(unix)]
fn collect_from(
    dir: &Path,
    origin: SkillOrigin,
    collection: &mut SkillCollection,
    permissions: &Permissions,
    trusted_root: Option<&TrustedImplicitReadRoot>,
) {
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
        let Some(content) =
            read_regular_utf8_no_follow(&md, MAX_SKILL_MANIFEST_BYTES, permissions, trusted_root)
        else {
            continue;
        };
        let mut skill = parse(&content, &p);
        skill.resources = collect_resources(&p, permissions, trusted_root);
        if skill.name.is_empty() {
            continue;
        }
        let source = SkillSource {
            origin,
            manifest: md,
        };
        if let Some(index) = collection.seen.get(&skill.name).copied() {
            if let Some(selected) = collection.sources.get(index).cloned() {
                collection.duplicates.push(SkillDuplicate {
                    name: skill.name,
                    selected,
                    ignored: source,
                });
            }
        } else {
            let index = collection.skills.len();
            collection.seen.insert(skill.name.clone(), index);
            collection.skills.push(skill);
            collection.sources.push(source);
        }
    }
}

#[cfg(unix)]
fn read_regular_utf8_no_follow(
    path: &Path,
    max_bytes: usize,
    permissions: &Permissions,
    trusted_root: Option<&TrustedImplicitReadRoot>,
) -> Option<String> {
    read_regular_utf8_no_follow_with_hook(path, max_bytes, permissions, trusted_root, || {})
}

#[cfg(unix)]
fn read_regular_utf8_no_follow_with_hook<F>(
    path: &Path,
    max_bytes: usize,
    permissions: &Permissions,
    trusted_root: Option<&TrustedImplicitReadRoot>,
    after_open: F,
) -> Option<String>
where
    F: FnOnce(),
{
    let file = crate::attachments::open_regular_file_no_follow(path).ok()?;

    after_open();
    match trusted_root {
        Some(trusted_root) => trusted_root
            .validate_opened_file(path, &file, permissions)
            .ok()?,
        None => permissions.validate_trusted_opened_read(path, &file).ok()?,
    }
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > max_bytes || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(unix)]
fn collect_resources(
    dir: &Path,
    permissions: &Permissions,
    trusted_root: Option<&TrustedImplicitReadRoot>,
) -> Vec<SkillResource> {
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
        let Some(content) = read_regular_utf8_no_follow(&path, cap, permissions, trusted_root)
        else {
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

    fn permissions() -> Permissions {
        Permissions::for_roots(&[], &[]).unwrap()
    }

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
            ..SkillSet::default()
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
            ..SkillSet::default()
        };
        let rendered = set.terminal_index("");
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\t'));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("safe forged"));
    }

    #[cfg(unix)]
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

        let set = SkillSet::discover_from(&sub, false, None, &permissions());
        assert!(set.get("greet").is_some());
        assert_eq!(set.get("greet").unwrap().description, "say hi");
    }

    #[test]
    fn no_project_discovery_omits_project_skills() {
        let root = tempfile::tempdir().unwrap();
        let sk = root.path().join(".kitsu/skills/project-only-cycle-four");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(
            sk.join("SKILL.md"),
            "---\nname: project-only-cycle-four\n---\nbody",
        )
        .unwrap();
        assert!(
            SkillSet::discover_from(root.path(), true, None, &permissions())
                .get("project-only-cycle-four")
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_project_discovery_keeps_explicit_user_skill_namespace() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let user_skills = root.path().join("user-skills");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let project_skill = project.join(".kitsu/skills/shared");
        let user_skill = user_skills.join("shared");
        std::fs::create_dir_all(&project_skill).unwrap();
        std::fs::create_dir_all(&user_skill).unwrap();
        std::fs::write(
            project_skill.join("SKILL.md"),
            "---\nname: shared\ndescription: PROJECT\n---\nproject",
        )
        .unwrap();
        std::fs::write(
            user_skill.join("SKILL.md"),
            "---\nname: shared\ndescription: USER\n---\nuser",
        )
        .unwrap();

        let set = SkillSet::discover_from(&project, true, Some(&user_skills), &permissions());

        assert_eq!(set.get("shared").unwrap().description, "USER");
        assert_eq!(set.source("shared").unwrap().origin, SkillOrigin::User);
        assert!(set.duplicates().is_empty(), "project discovery is disabled");
    }

    #[cfg(unix)]
    #[test]
    fn user_skills_remain_available_inside_the_protected_default_state_root() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join(".kitsu");
        let user_skills = state.join("skills");
        let skill = user_skills.join("trusted-user-skill");
        let project = root.path().join("project");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::create_dir(&project).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: trusted-user-skill\ndescription: available\n---\nUSER BODY",
        )
        .unwrap();
        std::fs::write(skill.join("checklist.txt"), "USER RESOURCE").unwrap();
        let permissions = Permissions::for_roots(&[], &[]).unwrap();
        permissions
            .protect_read_path(&state.to_string_lossy())
            .unwrap();

        let set = SkillSet::discover_from(&project, true, Some(&user_skills), &permissions);

        let loaded = load_skill(set.get("trusted-user-skill").unwrap());
        assert!(loaded.contains("USER BODY"));
        assert!(loaded.contains("USER RESOURCE"));
        assert!(permissions.read_roots().is_empty());
        assert!(permissions.runtime_read_roots().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn user_source_wins_collision_and_duplicate_origins_are_retained() {
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        for (d, desc) in [(&d1, "USER"), (&d2, "PROJECT")] {
            let s = d.path().join("dup");
            std::fs::create_dir_all(&s).unwrap();
            std::fs::write(
                s.join("SKILL.md"),
                format!("---\nname: dup\ndescription: {desc}\n---\nb"),
            )
            .unwrap();
        }
        let mut collection = SkillCollection::default();
        collect_from(
            d1.path(),
            SkillOrigin::User,
            &mut collection,
            &permissions(),
            None,
        );
        collect_from(
            d2.path(),
            SkillOrigin::Project,
            &mut collection,
            &permissions(),
            None,
        );
        assert_eq!(
            collection.skills.len(),
            1,
            "the duplicate name is collected once"
        );
        assert_eq!(
            collection.skills[0].description, "USER",
            "trusted user source wins"
        );
        let set = collection.into_skill_set();
        assert_eq!(set.source("dup").unwrap().origin, SkillOrigin::User);
        assert_eq!(set.duplicates().len(), 1);
        assert_eq!(set.duplicates()[0].ignored.origin, SkillOrigin::Project);
        assert!(set.terminal_index("").contains("duplicate project ignored"));
    }

    #[cfg(unix)]
    #[test]
    fn project_always_body_requires_explicit_load() {
        let root = tempfile::tempdir().unwrap();
        let user_root = root.path().join("user");
        let project_root = root.path().join("project");
        for (dir, name) in [(&user_root, "user-auto"), (&project_root, "project-auto")] {
            let skill = dir.join(name);
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(
                skill.join("SKILL.md"),
                format!("---\nname: {name}\nalways: true\n---\n{name}-body"),
            )
            .unwrap();
        }
        let mut collection = SkillCollection::default();
        collect_from(
            &user_root,
            SkillOrigin::User,
            &mut collection,
            &permissions(),
            None,
        );
        collect_from(
            &project_root,
            SkillOrigin::Project,
            &mut collection,
            &permissions(),
            None,
        );
        let set = collection.into_skill_set();

        assert!(set.always_bodies().contains("user-auto-body"));
        assert!(!set.always_bodies().contains("project-auto-body"));
        assert!(load_skill(set.get("project-auto").unwrap()).contains("project-auto-body"));
        assert!(set
            .index("")
            .contains("project; always requires explicit load"));
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
            ..SkillSet::default()
        });
        let (desc, handler) = skill_tool(set);
        assert_eq!(desc.name, "skill");
        let ok = handler(r#"{"name":"greet"}"#.to_string()).await.unwrap();
        assert!(ok.contains("Say hello warmly."));
        assert!(handler(r#"{"name":"nope"}"#.to_string()).await.is_err());
        assert!(handler("not json".to_string()).await.is_err());
    }

    #[cfg(unix)]
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
        skill.resources = collect_resources(&skdir, &permissions(), None);
        let out = load_skill(&skill);
        assert!(out.contains("checklist.md"), "bundled file is listed");
        assert!(out.contains("1. check"), "bundled contents are captured");
        assert!(out.contains("https://example.com/spec"), "link surfaced");
        assert!(out.contains("NOT fetched"), "links are not fetched");
        // Loading does not widen filesystem authority; resources are served
        // from the bounded discovery snapshot instead.
        assert!(perms.check_read(&checklist).await.is_err());
    }

    #[cfg(not(unix))]
    #[test]
    fn filesystem_skill_discovery_fails_closed_without_secure_path_traversal() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let user_skills = root.path().join("user-skills");
        for (base, name) in [
            (project.join(".kitsu/skills"), "project-skill"),
            (user_skills.clone(), "user-skill"),
        ] {
            let skill = base.join(name);
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(
                skill.join("SKILL.md"),
                format!("---\nname: {name}\n---\nbody"),
            )
            .unwrap();
        }

        let set = SkillSet::discover_from(&project, false, Some(&user_skills), &permissions());
        assert!(set.is_empty());
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

        let set = SkillSet::discover_from(root.path(), false, None, &permissions());
        assert!(set.get("escaped-dir").is_none());
        assert!(set.get("linked-manifest").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_outside_hard_linked_manifests_and_resources() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let skills = project.join(".kitsu/skills");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(project.join(".git")).unwrap();

        let outside_manifest = outside.join("unrelated-manifest.txt");
        let outside_resource = outside.join("unrelated-resource.txt");
        std::fs::write(
            &outside_manifest,
            "---\nname: leaked-manifest\ndescription: secret\n---\nPRIVATE MANIFEST",
        )
        .unwrap();
        std::fs::write(&outside_resource, "PRIVATE RESOURCE").unwrap();
        let permissions = permissions();

        let linked = skills.join("linked");
        std::fs::create_dir(&linked).unwrap();
        std::fs::hard_link(&outside_manifest, linked.join("SKILL.md")).unwrap();

        let safe = skills.join("safe");
        std::fs::create_dir(&safe).unwrap();
        std::fs::write(
            safe.join("SKILL.md"),
            "---\nname: safe\ndescription: safe\n---\nSAFE BODY",
        )
        .unwrap();
        std::fs::hard_link(&outside_resource, safe.join("secret.txt")).unwrap();

        let set = SkillSet::discover_from(&project, false, None, &permissions);

        assert!(set.get("leaked-manifest").is_none());
        let loaded = load_skill(set.get("safe").unwrap());
        assert!(loaded.contains("SAFE BODY"));
        assert!(!loaded.contains("PRIVATE RESOURCE"));
        assert!(!loaded.contains("secret.txt"));
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

        let content =
            read_regular_utf8_no_follow_with_hook(&manifest, 1024, &permissions(), None, || {
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
