//! Saved-session inventory and resume selection.

use crate::chat::state::SessionInfo;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::session;
use crate::terminal::input::read_assistive_stdin_line;

pub(crate) fn filter_sessions(
    sessions: &[session::SessionSummary],
    filter: &str,
) -> Vec<session::SessionSummary> {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        sessions.to_vec()
    } else {
        sessions
            .iter()
            .filter(|session| {
                session.id.to_lowercase().contains(&filter)
                    || session.preview.to_lowercase().contains(&filter)
                    || session
                        .name
                        .as_ref()
                        .is_some_and(|name| name.to_lowercase().contains(&filter))
            })
            .cloned()
            .collect()
    }
}

pub(crate) fn format_sessions(
    dir: &std::path::Path,
    current: Option<&str>,
    filter: &str,
) -> Result<String, String> {
    let sessions = session::SessionStore::list(dir)?;
    Ok(format_session_summaries(&sessions, current, filter))
}

pub(crate) fn session_catalog_payload(
    dir: &std::path::Path,
    current: Option<&str>,
    filter: &str,
) -> Result<serde_json::Value, String> {
    let sessions = session::SessionStore::list(dir)?;
    let filtered = filter_sessions(&sessions, filter);
    Ok(serde_json::json!({
        "kind": "sessions",
        "current_session_id": current,
        "count": sessions.len(),
        "filtered_count": filtered.len(),
        "sessions": filtered.clone(),
        "text": format_session_summaries(&filtered, current, filter),
    }))
}

/// Interactive chooser for bare `/resume`: numbered list of saved sessions
/// (name-first), then one line of input — a number, a name, an id, `last`,
/// `auto`, or Enter for `last`. Empty list or read failure cancels quietly.
pub(crate) fn resume_picker(session: &SessionInfo) -> Option<String> {
    let sessions_dir = session.state_dir.join("sessions");
    let summaries = match session::SessionStore::list(&sessions_dir) {
        Ok(summaries) if !summaries.is_empty() => summaries,
        Ok(_) => {
            eprintln!(
                "no saved sessions — start one with kitsu chat --session (add --name <name> for a memorable handle) or /new"
            );
            return None;
        }
        Err(error) => {
            eprintln!("{error}");
            return None;
        }
    };
    let active = session.session_id.borrow().clone();
    eprintln!("saved sessions:");
    for (index, saved) in summaries.iter().enumerate().take(9) {
        let marker = if active.as_deref() == Some(saved.id.as_str()) {
            "*"
        } else {
            " "
        };
        let label = match &saved.name {
            Some(name) => format!("{} ({})", terminal_inline(name), terminal_inline(&saved.id)),
            None => terminal_inline(&saved.id),
        };
        eprintln!(
            " {marker}[{}] {label}  {}  {} turn(s)\n      {}",
            index + 1,
            terminal_inline(&saved.updated_at),
            saved.turns,
            terminal_inline(&saved.preview),
        );
    }
    if summaries.len() > 9 {
        eprintln!("  … {} more — /sessions lists all", summaries.len() - 9);
    }
    let line = match read_assistive_stdin_line(
        "resume which? [1-9, name, id, last, auto; Enter = last, q = cancel] ",
    ) {
        Ok(Some(line)) => line.trim().to_string(),
        _ => return None,
    };
    match line.as_str() {
        "" => Some("last".to_string()),
        "q" | "quit" | "cancel" => None,
        choice => match choice.parse::<usize>() {
            Ok(number) if (1..=summaries.len().min(9)).contains(&number) => {
                Some(summaries[number - 1].id.clone())
            }
            Ok(_) => {
                eprintln!("no session numbered {choice}");
                None
            }
            Err(_) => Some(choice.to_string()),
        },
    }
}

pub(crate) fn format_session_summaries(
    sessions: &[session::SessionSummary],
    current: Option<&str>,
    filter: &str,
) -> String {
    if sessions.is_empty() {
        let filter = filter.trim();
        if filter.is_empty() {
            return "no saved sessions — start one with /new or restore an archived session with /restore <exact-id>".to_string();
        } else {
            return format!(
                "no sessions matched '{}' — try /sessions to see all saved sessions",
                terminal_inline(filter)
            );
        }
    }
    let mut out = String::from("saved sessions:\n");
    for saved in sessions {
        let marker = if current == Some(saved.id.as_str()) {
            "*"
        } else {
            " "
        };
        let id = match &saved.name {
            Some(name) => format!("{} ({})", terminal_inline(name), terminal_inline(&saved.id)),
            None => terminal_inline(&saved.id),
        };
        let updated_at = terminal_inline(&saved.updated_at);
        let gateway = terminal_inline(&saved.gateway);
        let model = terminal_inline(&saved.model);
        let preview = terminal_inline(&saved.preview);
        out.push_str(&format!(
            " {marker} {}  {}  {} turn(s)  {}/{}\n     {}\n     {}\n",
            id,
            updated_at,
            saved.turns,
            gateway,
            model,
            shorten_home(std::path::Path::new(&saved.directory)),
            preview
        ));
    }
    out.push_str(
        "resume with /resume <id|name> or `kitsu chat --resume <id|name|last|auto>`; name one with `kitsu sessions rename <id> <name>`",
    );
    out
}
