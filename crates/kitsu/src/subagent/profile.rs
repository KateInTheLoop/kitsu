//! Validated, versioned reusable background-agent profiles.
//!
//! Profiles describe intent and requested authority. They never grant authority
//! by themselves: a frontend must intersect the requested tool/permission policy
//! with the invocation's effective policy before constructing a tool registry.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

pub const AGENT_PROFILE_SCHEMA_VERSION: u32 = 1;
pub const MAX_AGENT_PROFILES: usize = 64;
pub const MAX_AGENT_PROFILE_NAME_CHARS: usize = 64;
pub const MAX_AGENT_MODEL_CHARS: usize = 256;
pub const MAX_AGENT_INSTRUCTIONS_BYTES: usize = 16 * 1024;
pub const MAX_AGENT_TOOL_NAMES: usize = 64;
pub const MAX_AGENT_TOOL_NAME_CHARS: usize = 128;
pub const MAX_AGENT_PERMISSION_ROOTS: usize = 32;
pub const MAX_AGENT_PERMISSION_ROOT_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    #[serde(rename = "xhigh", alias = "x_high")]
    XHigh,
}

/// Tools a profile asks a frontend to expose.
///
/// `None` is an important first-class mode: reasoning-only agents are valid and
/// may run with an empty [`kitsu_tools::Registry`]. `Inherit` and `AllowList`
/// remain requests, not grants.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", content = "tools", rename_all = "snake_case")]
pub enum AgentToolPolicy {
    #[default]
    None,
    /// Explicitly request the caller's already-authorized registry. Frontends
    /// must still intersect it with invocation/session authority.
    Inherit,
    AllowList(Vec<String>),
}

impl AgentToolPolicy {
    pub fn is_tool_free(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Check a registry selected by the frontend without requiring it to be
    /// non-empty. An allow-list rejects advertised tools outside the profile;
    /// missing allow-listed tools are permitted because invocation authority may
    /// legitimately narrow a reusable profile.
    pub fn accepts_registry(&self, registry: &kitsu_tools::Registry) -> bool {
        match self {
            Self::None => registry.descriptors().is_empty(),
            Self::Inherit => true,
            Self::AllowList(allowed) => registry
                .descriptors()
                .iter()
                .all(|descriptor| allowed.iter().any(|name| name == &descriptor.name)),
        }
    }
}

/// Permission defaults requested by a profile.
///
/// These values are inert configuration. Loading a profile must never restore
/// prior session grants or bypass the normal filesystem/command policy.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentPermissionDefaults {
    pub inherit_session: bool,
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    pub developer_commands: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: AgentReasoningEffort,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub tool_policy: AgentToolPolicy,
    #[serde(default)]
    pub permission_defaults: AgentPermissionDefaults,
}

impl AgentProfile {
    pub fn reasoning_only(name: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            model: None,
            reasoning_effort: AgentReasoningEffort::Medium,
            instructions: instructions.into(),
            tool_policy: AgentToolPolicy::None,
            permission_defaults: AgentPermissionDefaults::default(),
        }
    }

    pub fn validate(&self) -> Result<(), AgentProfileError> {
        validate_profile_name(&self.name)?;
        if let Some(model) = &self.model {
            validate_bounded_inline(
                model,
                "model",
                MAX_AGENT_MODEL_CHARS,
                AgentProfileErrorKind::InvalidModel,
            )?;
        }
        if self.instructions.len() > MAX_AGENT_INSTRUCTIONS_BYTES {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::InstructionsTooLarge,
                format!(
                    "profile instructions are {} bytes; maximum is {}",
                    self.instructions.len(),
                    MAX_AGENT_INSTRUCTIONS_BYTES
                ),
            ));
        }
        if self
            .instructions
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::InvalidInstructions,
                "profile instructions contain unsupported control characters",
            ));
        }
        validate_tool_policy(&self.tool_policy)?;
        validate_permission_defaults(&self.permission_defaults)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentProfileErrorKind {
    InvalidName,
    InvalidModel,
    InvalidInstructions,
    InstructionsTooLarge,
    TooManyTools,
    InvalidToolName,
    DuplicateTool,
    TooManyPermissionRoots,
    InvalidPermissionRoot,
    CatalogFull,
    DuplicateProfile,
    UnsupportedSchema,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentProfileError {
    pub kind: AgentProfileErrorKind,
    message: String,
}

impl AgentProfileError {
    fn new(kind: AgentProfileErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for AgentProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentProfileError {}

#[derive(Clone, Debug, Default)]
pub struct AgentProfileCatalog {
    profiles: BTreeMap<String, AgentProfile>,
}

impl AgentProfileCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, profile: AgentProfile) -> Result<(), AgentProfileError> {
        profile.validate()?;
        if self.profiles.contains_key(&profile.name) {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::DuplicateProfile,
                format!("agent profile '{}' already exists", profile.name),
            ));
        }
        if self.profiles.len() == MAX_AGENT_PROFILES {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::CatalogFull,
                format!("agent profile catalog is full (limit {MAX_AGENT_PROFILES})"),
            ));
        }
        self.profiles.insert(profile.name.clone(), profile);
        Ok(())
    }

    #[allow(dead_code)] // Reserved for persisted user-profile management.
    pub fn replace(
        &mut self,
        profile: AgentProfile,
    ) -> Result<Option<AgentProfile>, AgentProfileError> {
        profile.validate()?;
        if !self.profiles.contains_key(&profile.name) && self.profiles.len() == MAX_AGENT_PROFILES {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::CatalogFull,
                format!("agent profile catalog is full (limit {MAX_AGENT_PROFILES})"),
            ));
        }
        Ok(self.profiles.insert(profile.name.clone(), profile))
    }

    pub fn get(&self, name: &str) -> Option<&AgentProfile> {
        self.profiles.get(name)
    }

    #[allow(dead_code)] // Reserved for persisted user-profile management.
    pub fn remove(&mut self, name: &str) -> Option<AgentProfile> {
        self.profiles.remove(name)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &AgentProfile> {
        self.profiles.values()
    }

    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    pub fn to_dto(&self) -> AgentProfileCatalogDto {
        AgentProfileCatalogDto {
            schema_version: AGENT_PROFILE_SCHEMA_VERSION,
            profiles: self.profiles.values().cloned().collect(),
        }
    }

    pub fn from_dto(dto: AgentProfileCatalogDto) -> Result<Self, AgentProfileError> {
        if dto.schema_version != AGENT_PROFILE_SCHEMA_VERSION {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::UnsupportedSchema,
                format!(
                    "unsupported agent profile schema {}; expected {}",
                    dto.schema_version, AGENT_PROFILE_SCHEMA_VERSION
                ),
            ));
        }
        if dto.profiles.len() > MAX_AGENT_PROFILES {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::CatalogFull,
                format!(
                    "agent profile catalog has {} entries; maximum is {}",
                    dto.profiles.len(),
                    MAX_AGENT_PROFILES
                ),
            ));
        }
        let mut catalog = Self::new();
        for profile in dto.profiles {
            catalog.insert(profile)?;
        }
        Ok(catalog)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfileCatalogDto {
    pub schema_version: u32,
    pub profiles: Vec<AgentProfile>,
}

fn validate_profile_name(name: &str) -> Result<(), AgentProfileError> {
    let count = name.chars().count();
    let valid = (1..=MAX_AGENT_PROFILE_NAME_CHARS).contains(&count)
        && name.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' => true,
            b'0'..=b'9' | b'_' | b'-' => index > 0,
            _ => false,
        });
    if valid {
        Ok(())
    } else {
        Err(AgentProfileError::new(
            AgentProfileErrorKind::InvalidName,
            format!(
                "agent profile name must be 1-{MAX_AGENT_PROFILE_NAME_CHARS} characters and match [a-z][a-z0-9_-]*"
            ),
        ))
    }
}

fn validate_bounded_inline(
    value: &str,
    label: &str,
    max_chars: usize,
    kind: AgentProfileErrorKind,
) -> Result<(), AgentProfileError> {
    let count = value.chars().count();
    if value.trim() != value
        || count == 0
        || count > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(AgentProfileError::new(
            kind,
            format!("agent profile {label} must be 1-{max_chars} inline characters"),
        ));
    }
    Ok(())
}

fn validate_tool_policy(policy: &AgentToolPolicy) -> Result<(), AgentProfileError> {
    let AgentToolPolicy::AllowList(tools) = policy else {
        return Ok(());
    };
    if tools.len() > MAX_AGENT_TOOL_NAMES {
        return Err(AgentProfileError::new(
            AgentProfileErrorKind::TooManyTools,
            format!(
                "agent profile requests {} tools; maximum is {}",
                tools.len(),
                MAX_AGENT_TOOL_NAMES
            ),
        ));
    }
    let mut seen = HashSet::with_capacity(tools.len());
    for tool in tools {
        validate_bounded_inline(
            tool,
            "tool name",
            MAX_AGENT_TOOL_NAME_CHARS,
            AgentProfileErrorKind::InvalidToolName,
        )?;
        if !seen.insert(tool) {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::DuplicateTool,
                format!("agent profile repeats tool '{tool}'"),
            ));
        }
    }
    Ok(())
}

fn validate_permission_defaults(
    permissions: &AgentPermissionDefaults,
) -> Result<(), AgentProfileError> {
    let total = permissions.read_roots.len() + permissions.write_roots.len();
    if total > MAX_AGENT_PERMISSION_ROOTS {
        return Err(AgentProfileError::new(
            AgentProfileErrorKind::TooManyPermissionRoots,
            format!(
                "agent profile requests {total} permission roots; maximum is {MAX_AGENT_PERMISSION_ROOTS}"
            ),
        ));
    }
    for root in permissions
        .read_roots
        .iter()
        .chain(&permissions.write_roots)
    {
        if root.trim() != root
            || root.is_empty()
            || root.len() > MAX_AGENT_PERMISSION_ROOT_BYTES
            || root.chars().any(char::is_control)
        {
            return Err(AgentProfileError::new(
                AgentProfileErrorKind::InvalidPermissionRoot,
                format!(
                    "agent profile permission root must be 1-{MAX_AGENT_PERMISSION_ROOT_BYTES} bytes without controls or surrounding whitespace"
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_only_profile_accepts_an_empty_registry() {
        let profile = AgentProfile::reasoning_only("reviewer", "Inspect the evidence only.");
        profile.validate().unwrap();
        assert!(profile.tool_policy.is_tool_free());
        assert!(profile
            .tool_policy
            .accepts_registry(&kitsu_tools::Registry::new()));
        assert_eq!(
            profile.permission_defaults,
            AgentPermissionDefaults::default()
        );
    }

    #[test]
    fn omitted_capabilities_default_to_tool_free_and_no_inheritance() {
        let profile: AgentProfile = serde_json::from_str(r#"{"name":"thinker"}"#).unwrap();
        assert_eq!(profile.tool_policy, AgentToolPolicy::None);
        assert!(!profile.permission_defaults.inherit_session);
        assert!(profile.permission_defaults.read_roots.is_empty());
        assert!(profile.permission_defaults.write_roots.is_empty());
        assert!(profile
            .tool_policy
            .accepts_registry(&kitsu_tools::Registry::new()));
    }

    #[test]
    fn profile_validation_rejects_unsafe_names_duplicates_and_limits() {
        let mut invalid = AgentProfile::reasoning_only("../Owner", "safe");
        assert_eq!(
            invalid.validate().unwrap_err().kind,
            AgentProfileErrorKind::InvalidName
        );

        invalid.name = "worker".to_string();
        invalid.tool_policy = AgentToolPolicy::AllowList(vec!["read".into(), "read".into()]);
        assert_eq!(
            invalid.validate().unwrap_err().kind,
            AgentProfileErrorKind::DuplicateTool
        );

        invalid.tool_policy = AgentToolPolicy::None;
        invalid.instructions = "x".repeat(MAX_AGENT_INSTRUCTIONS_BYTES + 1);
        assert_eq!(
            invalid.validate().unwrap_err().kind,
            AgentProfileErrorKind::InstructionsTooLarge
        );
    }

    #[test]
    fn catalog_dto_round_trip_revalidates_and_rejects_duplicates() {
        let mut catalog = AgentProfileCatalog::new();
        catalog
            .insert(AgentProfile::reasoning_only("reviewer", "Review."))
            .unwrap();
        let encoded = serde_json::to_string(&catalog.to_dto()).unwrap();
        let decoded: AgentProfileCatalogDto = serde_json::from_str(&encoded).unwrap();
        let restored = AgentProfileCatalog::from_dto(decoded).unwrap();
        assert_eq!(restored.get("reviewer").unwrap().instructions, "Review.");

        let duplicate = AgentProfile::reasoning_only("same", "one");
        let error = AgentProfileCatalog::from_dto(AgentProfileCatalogDto {
            schema_version: AGENT_PROFILE_SCHEMA_VERSION,
            profiles: vec![duplicate.clone(), duplicate],
        })
        .unwrap_err();
        assert_eq!(error.kind, AgentProfileErrorKind::DuplicateProfile);
    }
}
