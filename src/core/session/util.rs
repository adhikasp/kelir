//! Free helpers for result events, prompt rendering and task input.

use super::*;

/// a model id on the current provider, while `openrouter/deepseek/deepseek-chat` names a provider.
pub fn print_result_event(
    outcome: &str,
    exit_code: u8,
    final_text: &str,
    steps: usize,
    session: Option<(&str, &Path)>,
    error: Option<&str>,
) {
    let mut value = json!({
        "type": "result",
        "outcome": outcome,
        "exit_code": exit_code,
        "final_text": final_text,
        "steps": steps,
        "kelir_version": VERSION,
    });
    if let Some((id, dir)) = session {
        value["session"] = json!(id);
        value["session_dir"] = json!(crate::util::path_string(dir));
    }
    if let Some(message) = error {
        value["error"] = json!(message);
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{value}");
    let _ = lock.flush();
}

/// The head of a settled call in headless output: every native tool's human-readable `description` when
/// it supplied one, otherwise the command, path or focus.
pub fn compact_input(args: &Value) -> String {
    if let Some(label) = crate::tools_view::description(args) {
        return label;
    }
    let text = args
        .get("command")
        .and_then(|value| value.as_str())
        .or_else(|| args.get("path").and_then(|value| value.as_str()))
        .or_else(|| args.get("focus").and_then(|value| value.as_str()))
        .map(str::to_string)
        .unwrap_or_else(|| args.to_string());
    crate::util::truncate_chars(&text.replace('\n', " ↵ "), 60)
}

/// The last non-empty line of a call's output file: a failed call's tail row.
/// Reads only the end of the file, so a 200 MB log stays cheap.
pub fn call_tail(outcome: &ToolOutcome) -> Option<String> {
    if outcome.out_path.is_empty() {
        return None;
    }
    let text = crate::tui::read_tail(std::path::Path::new(&outcome.out_path), 4096).ok()?;
    let line = crate::tui::last_line(&text);
    (!line.is_empty()).then_some(line)
}

/// The last non-empty line of a result that never reached a file (a stopped or
/// disabled call's message).
pub fn last_result_line(result: &str) -> Option<String> {
    let line = crate::tui::last_line(result);
    (!line.is_empty()).then_some(line)
}

/// One line for the UI: newlines folded, length bounded.
pub fn new_session_id() -> String {
    format!("ses_{}", ulid::Ulid::generate().to_string().to_lowercase())
}

/// Render the system prompt once, from the config and the cwd's sources.
pub fn render_system(
    cfg: &Config,
    cwd: &Path,
    herdr: Option<&str>,
    id: &str,
) -> crate::sources::Rendered {
    let mut places =
        crate::sources::places(cwd, &cfg.skills_dirs, Some(crate::paths::config_dir()));
    places.memory_dir = cfg
        .memory_dir
        .as_deref()
        .map(|dir| crate::memory::resolve(dir, cwd));
    let date = today();
    crate::sources::render_system(&crate::sources::Inputs {
        herdr,
        base: &cfg.system_prompt,
        base_path: cfg.prompt_path.as_deref(),
        places: &places,
        cwd,
        date: &date,
        session_id: Some(id),
    })
}

/// The date goes in, never the time: the system prefix must be stable for the
/// life of a session (and across a resume).
pub fn today() -> String {
    // `KELIR_TODAY=YYYY-MM-DD` stands in for the clock (the date-change tests).
    if let Some(day) = crate::envs::var("KELIR_TODAY") {
        let day = day.trim();
        if day.len() == 10 && day.as_bytes()[4] == b'-' && day.as_bytes()[7] == b'-' {
            return day.to_string();
        }
    }
    time::OffsetDateTime::now_utc().date().to_string()
}

pub async fn task_text(parts: &[String]) -> Result<String, String> {
    if !parts.is_empty() {
        return Ok(parts.join(" "));
    }
    let mut buffer = String::new();
    tokio::io::stdin()
        .read_to_string(&mut buffer)
        .await
        .map_err(|error| format!("cannot read the task from stdin: {error}"))?;
    let trimmed = buffer.trim().to_string();
    if trimmed.is_empty() {
        return Err("no task: pass it as an argument or on stdin".to_string());
    }
    Ok(trimmed)
}
