use kitsu_core::{
    ContextSourcesSnapshot, CurrencyAmount, CurrentSessionSnapshot, EffectiveAuthorityMode,
    TelemetryProvenance, TelemetryValue, TruthClass,
};
use kitsu_providers::{ProviderAdapterId, ProviderCapability, ProviderWireRevision};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TelemetryDeckDestination {
    StyledTerminal,
    NoColor,
    Plain,
    ScreenReader,
    Redirected,
}

impl TelemetryDeckDestination {
    fn styled(self) -> bool {
        matches!(self, Self::StyledTerminal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TelemetryDeckOptions {
    pub destination: TelemetryDeckDestination,
    pub columns: usize,
}

/// One immutable command-local observation for the full `/status` deck.
///
/// Version and cwd are presentation facts that do not belong in the portable
/// current-session telemetry schema, but retaining them as typed values keeps
/// their truth class and provenance explicit alongside that snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrentStatusObservation {
    snapshot: CurrentSessionSnapshot,
    version: TelemetryValue<String>,
    cwd: TelemetryValue<String>,
}

impl CurrentStatusObservation {
    pub(crate) const fn new(
        snapshot: CurrentSessionSnapshot,
        version: TelemetryValue<String>,
        cwd: TelemetryValue<String>,
    ) -> Self {
        Self {
            snapshot,
            version,
            cwd,
        }
    }

    pub(crate) const fn snapshot(&self) -> &CurrentSessionSnapshot {
        &self.snapshot
    }

    pub(crate) const fn version(&self) -> &TelemetryValue<String> {
        &self.version
    }

    pub(crate) const fn cwd(&self) -> &TelemetryValue<String> {
        &self.cwd
    }
}

#[derive(Debug)]
struct Fact {
    label: &'static str,
    value: String,
    truth: TruthClass,
    provenance: TelemetryProvenance,
}

#[derive(Debug)]
struct Section {
    title: &'static str,
    narrow_title: &'static str,
    prefix: &'static str,
    facts: Vec<Fact>,
}

pub(crate) fn render_current_status_observation(
    observation: &CurrentStatusObservation,
    options: TelemetryDeckOptions,
) -> String {
    render_sections(
        sections(
            observation.snapshot(),
            Some((observation.version(), observation.cwd())),
            options.destination.styled(),
        ),
        options,
    )
}

pub(crate) fn render_current_session_usage(
    snapshot: &CurrentSessionSnapshot,
    options: TelemetryDeckOptions,
) -> String {
    let usage = sections(snapshot, None, options.destination.styled())
        .pop()
        .expect("current-session telemetry always has a usage section");
    render_sections(vec![usage], options)
}

pub(crate) fn render_current_session_context(
    snapshot: &CurrentSessionSnapshot,
    options: TelemetryDeckOptions,
) -> String {
    let context = sections(snapshot, None, options.destination.styled())
        .into_iter()
        .nth(1)
        .expect("current-session telemetry always has a context section");
    render_sections(vec![context], options)
}

pub(crate) fn render_context_sources(
    snapshot: &ContextSourcesSnapshot,
    options: TelemetryDeckOptions,
) -> String {
    render_sections(context_source_sections(snapshot), options)
}

fn context_source_sections(snapshot: &ContextSourcesSnapshot) -> Vec<Section> {
    let sources = vec![
        fact(
            "core instructions",
            &snapshot.core_instructions,
            grouped_count,
        ),
        fact("project context", &snapshot.project_context, grouped_count),
        fact(
            "skill index + preloads",
            &snapshot.skill_context,
            grouped_count,
        ),
        fact("tool schemas", &snapshot.tool_schemas, grouped_count),
        fact(
            "runtime system messages",
            &snapshot.runtime_system_messages,
            grouped_count,
        ),
        fact("user messages", &snapshot.user_messages, grouped_count),
        fact(
            "assistant messages",
            &snapshot.assistant_messages,
            grouped_count,
        ),
        fact("tool results", &snapshot.tool_results, grouped_count),
        unavailable_fact(
            "attachments",
            &snapshot.attachments,
            "included in user messages",
        ),
        unavailable_fact(
            "on-demand skill bodies",
            &snapshot.on_demand_skill_bodies,
            "included in user messages",
        ),
        unavailable_fact(
            "memory",
            &snapshot.memory,
            "no dedicated Kitsu memory bucket",
        ),
        unavailable_fact(
            "response schema",
            &snapshot.response_schema,
            "not separately itemized",
        ),
    ];
    let budget = vec![
        fact("prompt budget", &snapshot.prompt_budget, grouped_count),
        fact("output reserve", &snapshot.output_reserve, grouped_count),
        fact("trim target", &snapshot.trim_target, grouped_count),
        fact("estimate buffer", &snapshot.estimate_buffer, grouped_count),
        fact("compactions", &snapshot.compactions, grouped_count),
        fact(
            "exchanges removed",
            &snapshot.exchanges_removed,
            grouped_count,
        ),
        fact(
            "tool results elided",
            &snapshot.tool_results_elided,
            grouped_count,
        ),
        fact(
            "tokens saved",
            &snapshot.compaction_tokens_saved,
            grouped_count,
        ),
    ];
    vec![
        Section {
            title: "context sources",
            narrow_title: "sources",
            prefix: "source",
            facts: sources,
        },
        Section {
            title: "budget & compaction",
            narrow_title: "budget",
            prefix: "context",
            facts: budget,
        },
    ]
}

fn render_sections(sections: Vec<Section>, options: TelemetryDeckOptions) -> String {
    if !options.destination.styled() {
        return render_plain(&sections);
    }
    let columns = options.columns.max(1);
    if columns >= 72 {
        render_wide(&sections, columns)
    } else if columns >= 40 {
        render_stacked(&sections, columns)
    } else {
        render_narrow(&sections, columns)
    }
}

fn sections(
    snapshot: &CurrentSessionSnapshot,
    presentation: Option<(&TelemetryValue<String>, &TelemetryValue<String>)>,
    visual: bool,
) -> Vec<Section> {
    let route = &snapshot.status.route;
    let authority = &snapshot.status.authority;
    let mut status = Vec::new();
    if let Some((version, cwd)) = presentation {
        status.extend([
            fact("version", version, |value| clean_inline(value)),
            fact("cwd", cwd, |value| clean_inline(value)),
        ]);
    }
    status.extend([
        fact("adapter", &route.adapter, adapter_label),
        fact("model", &route.model, |value| clean_inline(value)),
        fact("profile", &route.profile, |value| clean_inline(value)),
        fact("generation", &route.generation, grouped_count),
        fact("wire revision", &route.wire_revision, wire_revision_label),
        fact("capabilities", &route.capabilities, |values| {
            capability_labels(values)
        }),
        fact("session", &snapshot.status.session_id, |value| {
            clean_inline(value)
        }),
        fact("run", &snapshot.status.run_id, |value| clean_inline(value)),
        fact("authority", &authority.mode, authority_mode_label),
        fact("commands", &authority.commands_enabled, |enabled| {
            if *enabled {
                "enabled".to_string()
            } else {
                "disabled".to_string()
            }
        }),
        fact("read roots", &authority.read_roots, grouped_count),
        fact("write roots", &authority.write_roots, grouped_count),
        fact("protected roots", &authority.protected_roots, grouped_count),
        fact(
            "command grants",
            &authority.exact_command_grants,
            grouped_count,
        ),
    ]);

    let context = &snapshot.context;
    let mut context_facts = Vec::new();
    if let (Some(used), Some(effective)) = (
        context.used_tokens.value().copied(),
        context.effective_tokens.value().copied(),
    ) {
        let percent = if effective == 0 {
            0
        } else {
            used.saturating_mul(100).checked_div(effective).unwrap_or(0)
        };
        let mut value = format!(
            "{} / {}, {percent}%",
            grouped_count(&used),
            grouped_count(&effective)
        );
        if visual {
            const CELLS: u64 = 12;
            let filled = if effective == 0 {
                0
            } else {
                used.saturating_mul(CELLS)
                    .checked_div(effective)
                    .unwrap_or(0)
                    .min(CELLS)
            };
            let bar: String = (0..CELLS)
                .map(|index| if index < filled { '■' } else { '□' })
                .collect();
            value.push_str(&format!(" [{bar}]"));
        }
        context_facts.push(Fact {
            label: "occupancy",
            value,
            truth: context.used_tokens.truth_class(),
            provenance: context.used_tokens.provenance(),
        });
    }
    context_facts.extend([
        fact("window tokens", &context.window_tokens, grouped_count),
        fact("effective tokens", &context.effective_tokens, grouped_count),
        fact("used tokens", &context.used_tokens, grouped_count),
        fact("available tokens", &context.available_tokens, grouped_count),
    ]);

    let usage = &snapshot.usage;
    let mut usage_facts = Vec::new();
    if let (Some(reported), Some(missing)) = (
        usage.responses_reporting_usage.value().copied(),
        usage.responses_missing_usage.value().copied(),
    ) {
        usage_facts.push(Fact {
            label: "coverage",
            value: format!(
                "{} reported, {} missing",
                grouped_count(&reported),
                grouped_count(&missing)
            ),
            truth: usage.responses_reporting_usage.truth_class(),
            provenance: usage.responses_reporting_usage.provenance(),
        });
    }
    usage_facts.extend([
        fact("turns", &usage.turns, grouped_count),
        fact(
            "responses reported",
            &usage.responses_reporting_usage,
            grouped_count,
        ),
        fact(
            "responses missing",
            &usage.responses_missing_usage,
            grouped_count,
        ),
        fact("prompt tokens", &usage.prompt_tokens, grouped_count),
        fact("completion tokens", &usage.completion_tokens, grouped_count),
        fact("total tokens", &usage.total_tokens, grouped_count),
        fact(
            "cached prompt tokens",
            &usage.cached_prompt_tokens,
            grouped_count,
        ),
    ]);
    if usage.monetary_cost.value().is_some() {
        usage_facts.push(fact("monetary cost", &usage.monetary_cost, currency_amount));
    }

    vec![
        Section {
            title: "status",
            narrow_title: "status",
            prefix: "status",
            facts: status,
        },
        Section {
            title: "context",
            narrow_title: "context",
            prefix: "context",
            facts: context_facts,
        },
        Section {
            title: "current-session usage",
            narrow_title: "usage",
            prefix: "usage",
            facts: usage_facts,
        },
    ]
}

fn fact<T>(
    label: &'static str,
    value: &TelemetryValue<T>,
    format_value: impl FnOnce(&T) -> String,
) -> Fact {
    Fact {
        label,
        value: value
            .value()
            .map_or_else(|| "unavailable".to_string(), format_value),
        truth: value.truth_class(),
        provenance: value.provenance(),
    }
}

fn unavailable_fact(
    label: &'static str,
    value: &TelemetryValue<u64>,
    explanation: &'static str,
) -> Fact {
    debug_assert!(value.value().is_none());
    Fact {
        label,
        value: format!("unavailable ({explanation})"),
        truth: value.truth_class(),
        provenance: value.provenance(),
    }
}

fn render_plain(sections: &[Section]) -> String {
    let mut lines = vec!["current session".to_string()];
    for section in sections {
        lines.push(format!("[{}]", section.narrow_title));
        lines.extend(section.facts.iter().map(|fact| {
            format!(
                "{}.{}: {} [{}; {}]",
                section.prefix,
                fact.label,
                fact.value,
                truth_label(fact.truth),
                provenance_label(fact.provenance)
            )
        }));
    }
    lines.join("\n")
}

fn render_wide(sections: &[Section], columns: usize) -> String {
    let mut lines = vec![style_heading(&border("╭", "╮", "current session", columns))];
    for section in sections {
        lines.push(style_heading(&border("├", "┤", section.title, columns)));
        for fact in &section.facts {
            lines.extend(wide_fact_rows(fact, columns));
        }
    }
    lines.push(style_heading(&border("╰", "╯", "", columns)));
    lines.join("\n")
}

fn render_stacked(sections: &[Section], columns: usize) -> String {
    let mut lines = vec![style_heading(&fit_middle("◆ current session", columns))];
    for section in sections {
        lines.push(style_heading(&fit_middle(
            &format!("── {} ──", section.title),
            columns,
        )));
        for fact in &section.facts {
            lines.extend(stacked_fact_rows(fact, columns));
        }
    }
    lines.join("\n")
}

fn render_narrow(sections: &[Section], columns: usize) -> String {
    let mut lines = vec![style_heading(&fit_middle("current session", columns))];
    for section in sections {
        lines.push(style_heading(&fit_middle(
            &format!("[{}]", section.narrow_title),
            columns,
        )));
        for fact in &section.facts {
            lines.push(fit_middle(&format!("{}:", fact.label), columns));
            let indent_width = if columns >= 3 { 2 } else { 0 };
            let indent = " ".repeat(indent_width);
            let value_width = columns.saturating_sub(indent_width);
            lines.extend(
                wrap_text(&fact.value, value_width)
                    .into_iter()
                    .map(|line| format!("{indent}{line}")),
            );
            let evidence = format!(
                "[{}; {}]",
                truth_label(fact.truth),
                provenance_label(fact.provenance)
            );
            lines.extend(
                wrap_text(&evidence, value_width)
                    .into_iter()
                    .map(|line| format!("{indent}{line}")),
            );
        }
    }
    lines.join("\n")
}

fn border(left: &str, right: &str, title: &str, columns: usize) -> String {
    let inner = columns.saturating_sub(2);
    let title = if title.is_empty() {
        String::new()
    } else {
        format!("─ {title} ")
    };
    let title = fit_middle(&title, inner);
    let fill = "─".repeat(inner.saturating_sub(UnicodeWidthStr::width(title.as_str())));
    format!("{left}{title}{fill}{right}")
}

fn wide_fact_rows(fact: &Fact, columns: usize) -> Vec<String> {
    let content_width = columns.saturating_sub(4);
    const LABEL_WIDTH: usize = 21;
    let label_width = LABEL_WIDTH.min(content_width);
    let value_width = content_width.saturating_sub(label_width.saturating_add(1));
    if value_width == 0 {
        return vec![wide_row(fact.label, "", content_width, label_width)];
    }
    let mut rows = Vec::new();
    for (index, value) in wrap_text(&fact.value, value_width).into_iter().enumerate() {
        rows.push(wide_row(
            if index == 0 { fact.label } else { "" },
            &value,
            content_width,
            label_width,
        ));
    }
    let evidence = format!(
        "[{}; {}]",
        truth_label(fact.truth),
        provenance_label(fact.provenance)
    );
    for evidence in wrap_text(&evidence, value_width) {
        rows.push(wide_row("", &evidence, content_width, label_width));
    }
    rows
}

fn wide_row(label: &str, value: &str, content_width: usize, label_width: usize) -> String {
    let label = fit_middle(label, label_width);
    let body = format!("{label:<label_width$} {value}");
    let padding = " ".repeat(content_width.saturating_sub(UnicodeWidthStr::width(body.as_str())));
    format!("│ {body}{padding} │")
}

fn stacked_fact_rows(fact: &Fact, columns: usize) -> Vec<String> {
    let mut rows = vec![fit_middle(&format!("{}:", fact.label), columns)];
    let content_width = columns.saturating_sub(2);
    rows.extend(
        wrap_text(&fact.value, content_width)
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    let evidence = format!(
        "[{}; {}]",
        truth_label(fact.truth),
        provenance_label(fact.provenance)
    );
    rows.extend(
        wrap_text(&evidence, content_width)
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    rows
}

fn wrap_text(value: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in value.split_whitespace() {
        let word_width = UnicodeWidthStr::width(word);
        if word_width > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            lines.extend(split_width(word, width));
            continue;
        }
        let separator = usize::from(!current.is_empty());
        if UnicodeWidthStr::width(current.as_str())
            .saturating_add(separator)
            .saturating_add(word_width)
            > width
        {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn split_width(value: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for ch in value.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if char_width > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                used = 0;
            }
            lines.push(fit_middle(&ch.to_string(), width));
            continue;
        }
        if !current.is_empty() && used.saturating_add(char_width) > width {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(ch);
        used = used.saturating_add(char_width);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn style_heading(line: &str) -> String {
    let (r, g, b) = kitsu_render::theme().highlight;
    format!("\u{1b}[1;38;2;{r};{g};{b}m{line}\u{1b}[0m")
}

fn fit_middle(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }

    let left_width = (width - 1) / 2;
    let right_width = width - 1 - left_width;
    let left = take_width(value.chars(), left_width);
    let right_reversed = take_width(value.chars().rev(), right_width);
    let right: String = right_reversed.chars().rev().collect();
    format!("{left}…{right}")
}

fn take_width(chars: impl Iterator<Item = char>, width: usize) -> String {
    let mut used = 0usize;
    chars
        .take_while(|ch| {
            let char_width = UnicodeWidthChar::width(*ch).unwrap_or(0);
            if used.saturating_add(char_width) > width {
                false
            } else {
                used = used.saturating_add(char_width);
                true
            }
        })
        .collect()
}

fn clean_inline(value: &str) -> String {
    kitsu_render::sanitize_terminal_text(value)
        .chars()
        .map(|ch| if matches!(ch, '\n' | '\t') { ' ' } else { ch })
        .collect()
}

fn grouped_count(value: &u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

fn adapter_label(value: &ProviderAdapterId) -> String {
    match value {
        ProviderAdapterId::OpenAiCompatible => "openai_compatible",
        ProviderAdapterId::AnthropicMessages => "anthropic_messages",
    }
    .to_string()
}

fn wire_revision_label(value: &ProviderWireRevision) -> String {
    match value {
        ProviderWireRevision::OpenAiChatCompletionsV1 => "openai_chat_completions_v1",
        ProviderWireRevision::AnthropicMessages2023_06_01 => "anthropic_messages_2023_06_01",
    }
    .to_string()
}

fn capability_labels(values: &[ProviderCapability]) -> String {
    values
        .iter()
        .map(|value| match value {
            ProviderCapability::Streaming => "streaming",
            ProviderCapability::Tools => "tools",
            ProviderCapability::ImageInput => "image input",
            ProviderCapability::StructuredOutput => "structured output",
            ProviderCapability::Usage => "usage",
            ProviderCapability::ModelCatalog => "model catalog",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn authority_mode_label(value: &EffectiveAuthorityMode) -> String {
    match value {
        EffectiveAuthorityMode::Ask => "ask",
        EffectiveAuthorityMode::Scoped => "scoped",
        EffectiveAuthorityMode::AutoAllow => "auto allow",
        EffectiveAuthorityMode::Deny => "deny",
    }
    .to_string()
}

fn currency_amount(value: &CurrencyAmount) -> String {
    let currency = clean_inline(&value.currency_code);
    if value.decimal_places == 0 {
        return format!("{currency} {}", value.minor_units);
    }
    let places = usize::from(value.decimal_places);
    let mut digits = value.minor_units.to_string();
    if digits.len() <= places {
        digits = format!("{}{}", "0".repeat(places + 1 - digits.len()), digits);
    }
    let split = digits.len() - places;
    format!("{currency} {}.{}", &digits[..split], &digits[split..])
}

fn truth_label(value: TruthClass) -> &'static str {
    match value {
        TruthClass::Authoritative => "authoritative",
        TruthClass::ProviderReported => "provider reported",
        TruthClass::Derived => "derived",
        TruthClass::Estimated => "estimated",
        TruthClass::Unavailable => "unavailable",
    }
}

fn provenance_label(value: TelemetryProvenance) -> String {
    match value {
        TelemetryProvenance::ProviderRouteReceipt => "provider route receipt".to_string(),
        TelemetryProvenance::RouteModelUnavailable => "route model unavailable".to_string(),
        TelemetryProvenance::RuntimeIdentity => "runtime identity".to_string(),
        TelemetryProvenance::SessionIdentityUnavailable => {
            "session identity unavailable".to_string()
        }
        TelemetryProvenance::RunIdentityUnavailable => "run identity unavailable".to_string(),
        TelemetryProvenance::EffectiveAuthorityInput => "effective authority input".to_string(),
        TelemetryProvenance::ContextWindow(source) => {
            format!("context window: {}", context_window_source_label(source))
        }
        TelemetryProvenance::ContextBudgetArithmetic => "context budget arithmetic".to_string(),
        TelemetryProvenance::ProviderAnchoredContextMeter => {
            "provider-anchored context meter".to_string()
        }
        TelemetryProvenance::ContextMeterEstimate => "context meter estimate".to_string(),
        TelemetryProvenance::RequestAssemblyEstimate => "request assembly estimate".to_string(),
        TelemetryProvenance::ConversationMessageEstimate => {
            "conversation message estimate".to_string()
        }
        TelemetryProvenance::ContextSourceNotSeparatelyTracked => {
            "context source not separately tracked".to_string()
        }
        TelemetryProvenance::ProviderUsageReports => "provider usage reports".to_string(),
        TelemetryProvenance::ProviderUsageNotReported => "provider usage not reported".to_string(),
        TelemetryProvenance::ProviderUsageNotItemized => "provider usage not itemized".to_string(),
        TelemetryProvenance::SessionAccounting => "session accounting".to_string(),
        TelemetryProvenance::PortableCostUnavailable => "portable cost unavailable".to_string(),
        TelemetryProvenance::ExplicitCostObservation => "explicit cost observation".to_string(),
    }
}

fn context_window_source_label(value: kitsu_core::ContextWindowSource) -> &'static str {
    match value {
        kitsu_core::ContextWindowSource::CommandLine => "command line",
        kitsu_core::ContextWindowSource::GatewayProfile => "gateway profile",
        kitsu_core::ContextWindowSource::DiskCache => "disk cache",
        kitsu_core::ContextWindowSource::LiveProviderDiscovery => "live provider discovery",
        kitsu_core::ContextWindowSource::RuntimeServedWindow => "runtime-served window",
        kitsu_core::ContextWindowSource::ConservativeDefault => "conservative default",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        render_context_sources, render_current_session_context, render_current_session_usage,
        render_current_status_observation, CurrentStatusObservation, TelemetryDeckDestination,
        TelemetryDeckOptions,
    };
    use kitsu_core::{
        ContextSnapshotInput, ContextSourcesSnapshot, ContextSourcesSnapshotInput,
        ContextWindowSource, CurrencyAmount, CurrentSessionSnapshot, CurrentSessionSnapshotInput,
        EffectiveAuthorityInput, EffectiveAuthorityMode, RunId, SessionCostInput, SessionId,
        SessionUsageInput, TelemetryProvenance, TelemetryValue,
    };
    use kitsu_providers::ProviderRouteReceipt;
    use unicode_width::UnicodeWidthStr;

    fn route_receipt() -> ProviderRouteReceipt {
        serde_json::from_value(serde_json::json!({
            "receipt_revision": 1,
            "route": {
                "contract_revision": 1,
                "adapter": "anthropic_messages",
                "profile": "profile-v1-0123456789abcdef0123456789abcdef",
                "generation": 7,
                "wire_revision": "anthropic_messages_2023_06_01",
                "capabilities": {
                    "revision": 1,
                    "supported": ["streaming", "tools", "usage"]
                }
            },
            "model": "claude-fixture",
            "auth_source": "environment",
            "policy": {
                "inference_retries": 1,
                "catalog_retries": 0,
                "inference_timeout": {"seconds": 30, "nanoseconds": 0},
                "catalog_timeout": {"seconds": 10, "nanoseconds": 0},
                "advisory_discovery_timeout": {"seconds": 2, "nanoseconds": 0},
                "catalog": "unavailable",
                "retry_generation": "bound_generation",
                "semantic_replay_generation": "current_generation",
                "fallback": "disabled"
            }
        }))
        .expect("provider-free route receipt fixture")
    }

    fn snapshot(cost: SessionCostInput) -> CurrentSessionSnapshot {
        CurrentSessionSnapshot::from_input(CurrentSessionSnapshotInput {
            provider_route: route_receipt(),
            session_id: Some(SessionId::new("session-42").unwrap()),
            run_id: Some(RunId::new("run-9").unwrap()),
            authority: EffectiveAuthorityInput {
                mode: EffectiveAuthorityMode::Scoped,
                read_roots: 2,
                write_roots: 1,
                protected_roots: 1,
                exact_command_grants: 3,
                commands_enabled: true,
            },
            context: ContextSnapshotInput {
                window_tokens: TelemetryValue::authoritative(
                    32_768,
                    TelemetryProvenance::ContextWindow(ContextWindowSource::GatewayProfile),
                ),
                effective_tokens: TelemetryValue::derived(
                    30_720,
                    TelemetryProvenance::ContextBudgetArithmetic,
                ),
                used_tokens: TelemetryValue::estimated(
                    12_345,
                    TelemetryProvenance::ProviderAnchoredContextMeter,
                ),
            },
            usage: SessionUsageInput::provider_reported(2, 1, 2, 120, 30, 150, Some(20), cost),
        })
    }

    fn render(
        snapshot: &CurrentSessionSnapshot,
        destination: TelemetryDeckDestination,
        columns: usize,
    ) -> String {
        let observation = CurrentStatusObservation::new(
            snapshot.clone(),
            TelemetryValue::authoritative(
                "kitsu test".to_string(),
                TelemetryProvenance::RuntimeIdentity,
            ),
            TelemetryValue::authoritative(
                "/work/repository".to_string(),
                TelemetryProvenance::RuntimeIdentity,
            ),
        );
        render_current_status_observation(
            &observation,
            TelemetryDeckOptions {
                destination,
                columns,
            },
        )
    }

    fn render_usage(
        snapshot: &CurrentSessionSnapshot,
        destination: TelemetryDeckDestination,
        columns: usize,
    ) -> String {
        render_current_session_usage(
            snapshot,
            TelemetryDeckOptions {
                destination,
                columns,
            },
        )
    }

    fn render_context(
        snapshot: &CurrentSessionSnapshot,
        destination: TelemetryDeckDestination,
        columns: usize,
    ) -> String {
        render_current_session_context(
            snapshot,
            TelemetryDeckOptions {
                destination,
                columns,
            },
        )
    }

    fn context_sources() -> ContextSourcesSnapshot {
        ContextSourcesSnapshot::from_input(ContextSourcesSnapshotInput {
            core_instructions_tokens: 1_200,
            project_context_tokens: 0,
            skill_context_tokens: 320,
            tool_schema_tokens: 640,
            runtime_system_message_tokens: 18,
            user_message_tokens: 2_100,
            assistant_message_tokens: 900,
            tool_result_tokens: 450,
            prompt_budget_tokens: 30_720,
            output_reserve_tokens: 2_048,
            trim_target_tokens: 28_672,
            estimate_buffer_tokens: 2_048,
            compactions: 2,
            exchanges_removed: 3,
            tool_results_elided: 4,
            compaction_tokens_saved: 5_000,
        })
    }

    #[test]
    fn context_sources_reuse_adaptive_accessible_rendering_without_inventing_sub_buckets() {
        let sources = context_sources();
        for columns in [24, 40, 60, 80, 120] {
            let output = render_context_sources(
                &sources,
                TelemetryDeckOptions {
                    destination: TelemetryDeckDestination::StyledTerminal,
                    columns,
                },
            );
            let plain = kitsu_render::sanitize_terminal_text(&output);
            assert!(plain.contains("core instructions"), "{plain}");
            assert!(plain.contains("attachments"), "{plain}");
            assert!(plain.contains("on-demand"), "{plain}");
            assert!(plain.matches("unavailable").count() >= 4, "{plain}");
            assert!(plain.contains("output reserve"), "{plain}");
            assert!(plain.contains("tokens saved"), "{plain}");
            assert_lines_fit(&output, columns);
        }

        let expected = render_context_sources(
            &sources,
            TelemetryDeckOptions {
                destination: TelemetryDeckDestination::Redirected,
                columns: 24,
            },
        );
        for destination in [
            TelemetryDeckDestination::Redirected,
            TelemetryDeckDestination::Plain,
            TelemetryDeckDestination::NoColor,
            TelemetryDeckDestination::ScreenReader,
        ] {
            for columns in [24, 120] {
                let output = render_context_sources(
                    &sources,
                    TelemetryDeckOptions {
                        destination,
                        columns,
                    },
                );
                assert_eq!(output, expected, "{destination:?} changed at {columns}");
                assert!(!output.contains('\u{1b}'));
            }
        }
        assert!(
            expected.contains("source.project context: 0 [estimated; request assembly estimate]")
        );
        assert!(expected.contains(
            "source.attachments: unavailable (included in user messages) [unavailable; context source not separately tracked]"
        ));
        assert!(expected.contains(
            "source.on-demand skill bodies: unavailable (included in user messages) [unavailable; context source not separately tracked]"
        ));
        assert!(expected.contains(
            "source.memory: unavailable (no dedicated Kitsu memory bucket) [unavailable; context source not separately tracked]"
        ));
        assert!(expected.contains(
            "source.response schema: unavailable (not separately itemized) [unavailable; context source not separately tracked]"
        ));
        assert!(
            expected.contains("context.tokens saved: 5,000 [estimated; context meter estimate]")
        );
        assert!(!expected.contains("source total"));
    }

    #[test]
    fn status_observation_adds_authoritative_version_and_cwd_to_every_destination() {
        let observation = CurrentStatusObservation::new(
            snapshot(SessionCostInput::unavailable()),
            TelemetryValue::authoritative(
                "kitsu 9.8.7".to_string(),
                TelemetryProvenance::RuntimeIdentity,
            ),
            TelemetryValue::authoritative(
                "/work/repository".to_string(),
                TelemetryProvenance::RuntimeIdentity,
            ),
        );

        for (destination, columns) in [
            (TelemetryDeckDestination::StyledTerminal, 80),
            (TelemetryDeckDestination::StyledTerminal, 40),
            (TelemetryDeckDestination::StyledTerminal, 24),
            (TelemetryDeckDestination::StyledTerminal, 1),
            (TelemetryDeckDestination::Redirected, 80),
            (TelemetryDeckDestination::Plain, 80),
            (TelemetryDeckDestination::NoColor, 80),
            (TelemetryDeckDestination::ScreenReader, 80),
        ] {
            let output = render_current_status_observation(
                &observation,
                TelemetryDeckOptions {
                    destination,
                    columns,
                },
            );
            let plain = kitsu_render::sanitize_terminal_text(&output);
            assert!(plain.contains("version") || columns == 1, "{plain}");
            assert!(plain.contains("cwd") || columns == 1, "{plain}");
            if columns > 1 {
                assert!(plain.contains("kitsu 9.8.7"), "{plain}");
                assert!(plain.contains("/work/repository"), "{plain}");
                assert!(plain.contains("authoritative"), "{plain}");
                assert!(plain.contains("runtime identity"), "{plain}");
            }
            if destination == TelemetryDeckDestination::StyledTerminal {
                assert_lines_fit(&output, columns);
            } else {
                assert!(!output.contains('\u{1b}'), "{output}");
            }
        }
    }

    fn assert_lines_fit(output: &str, columns: usize) {
        for line in output.lines() {
            let plain = kitsu_render::sanitize_terminal_text(line);
            assert!(
                UnicodeWidthStr::width(plain.as_str()) <= columns,
                "line wider than {columns} cells: {plain:?}"
            );
        }
    }

    #[test]
    fn wide_styled_deck_is_grouped_colored_and_bounded() {
        let output = render(
            &snapshot(SessionCostInput::unavailable()),
            TelemetryDeckDestination::StyledTerminal,
            80,
        );
        let plain = kitsu_render::sanitize_terminal_text(&output);

        assert!(
            output.contains("\u{1b}["),
            "styled output needs SGR: {output}"
        );
        assert!(plain.contains("╭─ current session"), "{plain}");
        assert!(plain.contains("├─ status"), "{plain}");
        assert!(plain.contains("├─ context"), "{plain}");
        assert!(plain.contains("├─ current-session usage"), "{plain}");
        assert!(plain.contains("claude-fixture"), "{plain}");
        assert!(plain.contains("40%"), "{plain}");
        assert!(plain.contains("2 reported"), "{plain}");
        assert!(plain.contains("1 missing"), "{plain}");
        assert!(!plain.to_ascii_lowercase().contains("cost"), "{plain}");
        assert_lines_fit(&output, 80);
    }

    #[test]
    fn styled_layout_adapts_at_medium_and_narrow_widths_without_losing_sections() {
        for columns in [60, 40] {
            let output = render(
                &snapshot(SessionCostInput::unavailable()),
                TelemetryDeckDestination::StyledTerminal,
                columns,
            );
            let plain = kitsu_render::sanitize_terminal_text(&output);
            assert!(!plain.contains('╭'), "medium deck should stack: {plain}");
            for section in ["status", "context", "current-session usage"] {
                assert!(
                    plain.contains(section),
                    "missing {section} at {columns}: {plain}"
                );
            }
            assert_lines_fit(&output, columns);
        }

        let output = render(
            &snapshot(SessionCostInput::unavailable()),
            TelemetryDeckDestination::StyledTerminal,
            24,
        );
        let plain = kitsu_render::sanitize_terminal_text(&output);
        assert!(plain.contains("[status]"), "{plain}");
        assert!(plain.contains("[context]"), "{plain}");
        assert!(plain.contains("[usage]"), "{plain}");
        assert!(plain.contains("adapter"), "{plain}");
        assert!(plain.contains("responses missing"), "{plain}");
        assert_lines_fit(&output, 24);
    }

    #[test]
    fn tiny_styled_widths_bound_indentation_and_double_width_scalars() {
        let mut snapshot = snapshot(SessionCostInput::unavailable());
        snapshot.status.route.model = TelemetryValue::authoritative(
            "狐-model".to_string(),
            TelemetryProvenance::ProviderRouteReceipt,
        );

        for columns in [1, 2, 3] {
            let output = render(&snapshot, TelemetryDeckDestination::StyledTerminal, columns);
            assert!(!output.is_empty());
            assert_lines_fit(&output, columns);
        }
    }

    #[test]
    fn redirected_plain_no_color_and_screen_reader_share_stable_escape_free_output() {
        let snapshot = snapshot(SessionCostInput::unavailable());
        let expected = render(&snapshot, TelemetryDeckDestination::Redirected, 24);

        for destination in [
            TelemetryDeckDestination::Redirected,
            TelemetryDeckDestination::Plain,
            TelemetryDeckDestination::NoColor,
            TelemetryDeckDestination::ScreenReader,
        ] {
            for columns in [24, 120] {
                let output = render(&snapshot, destination, columns);
                assert_eq!(output, expected, "{destination:?} changed at {columns}");
                assert!(!output.contains('\u{1b}'));
                assert!(output.is_ascii(), "fixture output must be ASCII: {output}");
            }
        }
        assert!(expected.contains("status.adapter: anthropic_messages"));
        assert!(expected.contains("context.used tokens: 12,345"));
        assert!(expected.contains("estimated; provider-anchored context meter"));
        assert!(expected.contains("usage.responses missing: 1"));
    }

    #[test]
    fn unavailable_values_stay_explicit_and_do_not_invent_account_or_cost_facts() {
        let mut snapshot = snapshot(SessionCostInput::unavailable());
        snapshot.status.route.model =
            TelemetryValue::unavailable(TelemetryProvenance::RouteModelUnavailable);
        snapshot.context.window_tokens = TelemetryValue::unavailable(
            TelemetryProvenance::ContextWindow(ContextWindowSource::ConservativeDefault),
        );
        snapshot.context.effective_tokens =
            TelemetryValue::unavailable(TelemetryProvenance::ContextBudgetArithmetic);
        snapshot.context.used_tokens =
            TelemetryValue::unavailable(TelemetryProvenance::ContextMeterEstimate);
        snapshot.context.available_tokens =
            TelemetryValue::unavailable(TelemetryProvenance::ContextBudgetArithmetic);
        snapshot.usage.prompt_tokens =
            TelemetryValue::unavailable(TelemetryProvenance::ProviderUsageNotReported);
        snapshot.usage.completion_tokens =
            TelemetryValue::unavailable(TelemetryProvenance::ProviderUsageNotReported);
        snapshot.usage.total_tokens =
            TelemetryValue::unavailable(TelemetryProvenance::ProviderUsageNotReported);
        snapshot.usage.cached_prompt_tokens =
            TelemetryValue::unavailable(TelemetryProvenance::ProviderUsageNotItemized);

        let output = render(&snapshot, TelemetryDeckDestination::Redirected, 80);
        let lower = output.to_ascii_lowercase();
        assert!(output.contains("status.model: unavailable [unavailable; route model unavailable]"));
        assert!(output.contains(
            "usage.prompt tokens: unavailable [unavailable; provider usage not reported]"
        ));
        for forbidden in ["credits", "rate limit", "monetary cost", "usd", "$0"] {
            assert!(!lower.contains(forbidden), "invented {forbidden}: {output}");
        }
    }

    #[test]
    fn provider_reported_currency_is_rendered_only_when_present_in_the_snapshot() {
        let output = render(
            &snapshot(SessionCostInput::provider_reported(CurrencyAmount {
                currency_code: "USD".to_string(),
                minor_units: 123,
                decimal_places: 2,
            })),
            TelemetryDeckDestination::Redirected,
            80,
        );

        assert!(output.contains("usage.monetary cost: USD 1.23"), "{output}");
        assert!(output.contains("provider reported; explicit cost observation"));
    }

    #[test]
    fn usage_only_mount_reuses_adaptive_styled_plain_and_redirected_rendering() {
        let snapshot = snapshot(SessionCostInput::unavailable());
        for (destination, columns) in [
            (TelemetryDeckDestination::StyledTerminal, 80),
            (TelemetryDeckDestination::StyledTerminal, 24),
            (TelemetryDeckDestination::Plain, 80),
            (TelemetryDeckDestination::Redirected, 24),
        ] {
            let output = render_usage(&snapshot, destination, columns);
            let plain = kitsu_render::sanitize_terminal_text(&output);
            assert!(plain.contains("current-session usage") || plain.contains("[usage]"));
            assert!(plain.contains("2 reported"), "{plain}");
            assert!(plain.contains("1 missing"), "{plain}");
            assert!(!plain.contains("[status]"), "{plain}");
            assert!(!plain.contains("[context]"), "{plain}");
            assert!(!plain.contains("status.adapter"), "{plain}");
            assert!(!plain.contains("context.used tokens"), "{plain}");
            if destination != TelemetryDeckDestination::StyledTerminal {
                assert!(!output.contains('\u{1b}'), "{output}");
            }
            if destination == TelemetryDeckDestination::StyledTerminal {
                assert_lines_fit(&output, columns);
            }
        }
    }

    #[test]
    fn context_only_mount_reuses_every_adaptive_and_accessible_destination() {
        let snapshot = snapshot(SessionCostInput::unavailable());
        for columns in [24, 40, 60, 80, 120] {
            let output =
                render_context(&snapshot, TelemetryDeckDestination::StyledTerminal, columns);
            let plain = kitsu_render::sanitize_terminal_text(&output);
            assert!(plain.contains("context"), "{plain}");
            assert!(plain.contains("12,345"), "{plain}");
            assert!(plain.contains("40%"), "{plain}");
            assert!(!plain.contains("[status]"), "{plain}");
            assert!(!plain.contains("[usage]"), "{plain}");
            assert!(!plain.contains("status.adapter"), "{plain}");
            assert!(!plain.contains("usage.prompt tokens"), "{plain}");
            assert_lines_fit(&output, columns);
        }

        let expected = render_context(&snapshot, TelemetryDeckDestination::Redirected, 24);
        for destination in [
            TelemetryDeckDestination::Redirected,
            TelemetryDeckDestination::Plain,
            TelemetryDeckDestination::NoColor,
            TelemetryDeckDestination::ScreenReader,
        ] {
            for columns in [24, 120] {
                let output = render_context(&snapshot, destination, columns);
                assert_eq!(output, expected, "{destination:?} changed at {columns}");
                assert!(!output.contains('\u{1b}'), "{output}");
                assert!(output.is_ascii(), "fixture output must be ASCII: {output}");
            }
        }
        assert!(expected.contains("[context]"), "{expected}");
        assert!(
            expected.contains("context.window tokens: 32,768"),
            "{expected}"
        );
        assert!(
            expected.contains("authoritative; context window: gateway profile"),
            "{expected}"
        );
        assert!(
            expected.contains("context.available tokens: 18,375"),
            "{expected}"
        );
        assert!(!expected.contains("[status]"), "{expected}");
        assert!(!expected.contains("[usage]"), "{expected}");
    }
}
