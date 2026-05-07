use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::markdown::render_markdown;
use super::super::state::{ChatMessage, ToolStatus};
use super::super::theme::Theme;

pub(in crate::frontend::tui) fn message_lines(
    msg: &ChatMessage,
    theme: &Theme,
    transcript_mode: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let (label, label_style, _body_style) = role_styles(msg, theme);
    let mut rows: Vec<Line<'static>> = Vec::new();

    let is_assistant = msg.role == lattice::core::types::Role::Assistant && !msg.content.is_empty();
    let is_tool = msg.role == lattice::core::types::Role::Tool;
    let use_md = is_assistant || (is_tool && !msg.content.is_empty());

    if msg.tool.is_some() {
        rows.extend(tool_lines(msg, theme, width));
    } else if is_tool && msg.collapsed {
        let usable_width = width.max(1) as usize;
        let prefix_width = UnicodeWidthStr::width(label) + 1;
        let content = display_content(msg);
        let total = content.lines().count();
        let preview: String = content.lines().take(3).collect::<Vec<_>>().join("\n");
        rows.extend(labelled_lines(
            label,
            label_style,
            _body_style,
            &preview,
            usable_width,
        ));
        rows.push(Line::from(vec![
            Span::raw(" ".repeat(prefix_width)),
            Span::styled(
                format!(
                    "... {} more lines · Ctrl+E to expand",
                    total.saturating_sub(3)
                ),
                Style::default()
                    .fg(theme.subtext)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    } else if use_md {
        let prefix_width = UnicodeWidthStr::width(label) + 1; // label + space
        let md_width = width.saturating_sub(prefix_width as u16).max(20);

        // Use cached render if content+width unchanged; otherwise compute + cache
        let md_lines = match msg.get_cache(msg.content.len(), md_width) {
            Some(cached) => (*cached).clone(),
            None => {
                let lines = render_markdown(&msg.content, theme, md_width);
                msg.set_cache(msg.content.len(), lines.clone(), md_width);
                lines
            }
        };

        let indent = " ".repeat(prefix_width);
        for (i, md_line) in md_lines.into_iter().enumerate() {
            let md_spans = md_line.spans;
            if i == 0 {
                let mut spans = vec![Span::styled(label, label_style), Span::raw(" ")];
                spans.extend(md_spans);
                rows.push(Line::from(spans));
            } else {
                let mut spans = vec![Span::raw(indent.clone())];
                spans.extend(md_spans);
                rows.push(Line::from(spans));
            }
        }
    } else {
        let usable_width = width.max(1) as usize;
        rows.extend(labelled_lines(
            label,
            label_style,
            _body_style,
            display_content(msg),
            usable_width,
        ));
    }

    if transcript_mode || !msg.reasoning_collapsed {
        if let Some(reasoning) = msg.reasoning.as_deref().filter(|r| !r.is_empty()) {
            let usable_width = width.max(1) as usize;
            let summary = format!(
                "thinking expanded · {} lines · Ctrl+O to collapse",
                reasoning.lines().count()
            );
            rows.extend(labelled_lines(
                "◌",
                theme.thinking_style().add_modifier(Modifier::BOLD),
                theme.thinking_style(),
                &summary,
                usable_width,
            ));
            rows.extend(labelled_lines(
                " ",
                theme.thinking_style().add_modifier(Modifier::BOLD),
                theme.thinking_style(),
                reasoning,
                usable_width,
            ));
        }
    } else if msg.reasoning.as_deref().is_some_and(|r| !r.is_empty()) {
        // Show a collapsed indicator when reasoning is available but hidden
        let hint = format!(
            "thought for {} lines · Ctrl+O or /trace to expand",
            msg.reasoning.as_deref().unwrap().lines().count()
        );
        rows.push(Line::from(vec![
            Span::styled("◌ ", theme.thinking_style().add_modifier(Modifier::BOLD)),
            Span::styled(hint, theme.thinking_style()),
        ]));
    }

    rows.push(Line::raw(""));

    // User messages get full-width background highlight
    if msg.role == lattice::core::types::Role::User {
        let user_bg = Style::default().bg(theme.surface);
        rows = rows
            .into_iter()
            .map(|line| {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let current_w = UnicodeWidthStr::width(text.as_str());
                let pad = (width as usize).saturating_sub(current_w);
                let mut spans = line.spans;
                if pad > 0 {
                    spans.push(Span::styled(" ".repeat(pad), user_bg));
                }
                Line::from(spans)
            })
            .collect();
    }

    rows
}

fn role_styles(msg: &ChatMessage, theme: &Theme) -> (&'static str, Style, Style) {
    match msg.role {
        lattice::core::types::Role::User => (
            "●",
            theme.user_style().add_modifier(Modifier::BOLD),
            Style::default().fg(theme.text),
        ),
        lattice::core::types::Role::Assistant => (
            "●",
            theme.assistant_style().add_modifier(Modifier::BOLD),
            theme.assistant_style(),
        ),
        lattice::core::types::Role::System => (
            "●",
            Style::default()
                .fg(theme.subtext)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(theme.subtext),
        ),
        lattice::core::types::Role::Tool => (
            "●",
            theme.tool_style().add_modifier(Modifier::BOLD),
            theme.tool_style(),
        ),
    }
}

fn display_content(msg: &ChatMessage) -> &str {
    if msg.role == lattice::core::types::Role::Assistant && msg.content.is_empty() {
        "Thinking..."
    } else {
        &msg.content
    }
}

fn tool_lines(msg: &ChatMessage, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let Some(tool) = msg.tool.as_ref() else {
        return Vec::new();
    };
    let usable_width = width.max(1) as usize;
    let elapsed = tool
        .finished_at
        .unwrap_or_else(std::time::Instant::now)
        .saturating_duration_since(tool.started_at);
    let dot_style = match tool.status {
        ToolStatus::Running => Style::default().fg(theme.subtext),
        ToolStatus::Done => Style::default().fg(theme.success),
        ToolStatus::Error => Style::default().fg(theme.error),
    };
    let name = tool_display_name(&tool.name, &tool.arguments);
    let summary_width = usable_width
        .saturating_sub(UnicodeWidthStr::width(name.as_str()))
        .saturating_sub(5);
    let summary = tool_argument_summary(&tool.name, &tool.arguments, summary_width);
    let mut header = vec![
        Span::styled("● ", dot_style),
        Span::styled(
            name,
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ];
    if !summary.is_empty() {
        header.push(Span::styled("(", Style::default().fg(theme.text)));
        header.push(Span::styled(summary, Style::default().fg(theme.text)));
        header.push(Span::styled(")", Style::default().fg(theme.text)));
    }

    let mut rows = vec![Line::from(header)];

    match tool.result.as_deref() {
        None => {
            rows.push(tool_response_line(
                "Running...",
                theme.thinking_style(),
                theme,
                usable_width,
            ));
        }
        Some(result) if result.trim().is_empty() => {
            let (message, style) = match tool.status {
                ToolStatus::Running => ("Running...".to_string(), theme.thinking_style()),
                ToolStatus::Done => (
                    format!("Done in {}", short_duration(elapsed)),
                    Style::default().fg(theme.success),
                ),
                ToolStatus::Error => (
                    format!("Error after {}", short_duration(elapsed)),
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD),
                ),
            };
            rows.push(tool_response_line(message, style, theme, usable_width));
        }
        Some(result) => {
            if matches!(tool.status, ToolStatus::Error) {
                rows.push(tool_response_line(
                    format!("Error after {}", short_duration(elapsed)),
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD),
                    theme,
                    usable_width,
                ));
            }

            let body_style = match tool.status {
                ToolStatus::Error => Style::default().fg(theme.error),
                _ => theme.tool_style(),
            };
            let result_lines: Vec<&str> = result.lines().collect();
            let visible = if msg.collapsed {
                result_lines.iter().take(4).copied().collect::<Vec<_>>()
            } else {
                result_lines.clone()
            };
            let body = visible.join("\n");
            rows.extend(tool_response_lines(&body, body_style, theme, usable_width));
            if msg.collapsed && result_lines.len() > visible.len() {
                rows.push(tool_response_line(
                    format!(
                        "… {} more lines · Ctrl+E to expand",
                        result_lines.len().saturating_sub(visible.len())
                    ),
                    Style::default()
                        .fg(theme.subtext)
                        .add_modifier(Modifier::ITALIC),
                    theme,
                    usable_width,
                ));
            }
            if let Some(diff) = tool.file_diff.as_ref() {
                rows.extend(tool_diff_lines(
                    &diff.path,
                    &diff.text,
                    msg.collapsed,
                    theme,
                    usable_width,
                ));
            }
        }
    }

    rows
}

fn tool_response_line(
    text: impl Into<String>,
    body_style: Style,
    theme: &Theme,
    usable_width: usize,
) -> Line<'static> {
    let text = text.into();
    let prefix = "  ⎿  ";
    let body_width = usable_width
        .saturating_sub(UnicodeWidthStr::width(prefix))
        .max(1);
    let body = truncate_to_width(&text, body_width);
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(theme.subtext)),
        Span::styled(body, body_style),
    ])
}

fn tool_response_lines(
    content: &str,
    body_style: Style,
    theme: &Theme,
    usable_width: usize,
) -> Vec<Line<'static>> {
    let prefix = "  ⎿  ";
    let prefix_width = UnicodeWidthStr::width(prefix);
    let body_width = usable_width.saturating_sub(prefix_width).max(1);
    let indent = " ".repeat(prefix_width);
    let mut lines = Vec::new();

    for (logical_idx, line) in normalized_lines(content).enumerate() {
        for (chunk_idx, chunk) in wrap_line(line, body_width).into_iter().enumerate() {
            if logical_idx == 0 && chunk_idx == 0 {
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(theme.subtext)),
                    Span::styled(chunk, body_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(chunk, body_style),
                ]));
            }
        }
    }

    lines
}

fn tool_diff_lines(
    path: &str,
    diff: &str,
    collapsed: bool,
    theme: &Theme,
    usable_width: usize,
) -> Vec<Line<'static>> {
    const COLLAPSED_DIFF_LINES: usize = 12;

    let diff_lines: Vec<&str> = diff.lines().collect();
    if diff_lines.is_empty() {
        return Vec::new();
    }

    let mut rows = vec![tool_response_line(
        format!("Diff {path}"),
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        theme,
        usable_width,
    )];
    let visible = if collapsed {
        diff_lines
            .iter()
            .take(COLLAPSED_DIFF_LINES)
            .copied()
            .collect::<Vec<_>>()
    } else {
        diff_lines.clone()
    };
    rows.extend(diff_block_lines(&visible.join("\n"), theme, usable_width));

    if collapsed && diff_lines.len() > visible.len() {
        rows.push(tool_response_line(
            format!(
                "… {} more diff lines · Ctrl+E to expand",
                diff_lines.len().saturating_sub(visible.len())
            ),
            Style::default()
                .fg(theme.subtext)
                .add_modifier(Modifier::ITALIC),
            theme,
            usable_width,
        ));
    }

    rows
}

fn diff_block_lines(content: &str, theme: &Theme, usable_width: usize) -> Vec<Line<'static>> {
    let border_prefix = "     ";
    let body_prefix = "  │  ";
    let prefix_width = UnicodeWidthStr::width(body_prefix);
    let body_width = usable_width.saturating_sub(prefix_width).max(1);
    let separator = truncate_to_width(
        &"╌".repeat(usable_width.saturating_sub(UnicodeWidthStr::width(border_prefix))),
        usable_width.saturating_sub(UnicodeWidthStr::width(border_prefix)),
    );
    let mut rows = vec![Line::from(vec![
        Span::raw(border_prefix),
        Span::styled(separator.clone(), Style::default().fg(theme.border)),
    ])];

    for line in normalized_lines(content) {
        let style = diff_line_style(line, theme);
        let chunks = wrap_line(line, body_width);
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let prefix = if idx == 0 {
                body_prefix.to_string()
            } else {
                " ".repeat(prefix_width)
            };
            rows.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(theme.border)),
                Span::styled(chunk, style),
            ]));
        }
    }

    rows.push(Line::from(vec![
        Span::raw(border_prefix),
        Span::styled(separator, Style::default().fg(theme.border)),
    ]));
    rows
}

fn diff_line_style(line: &str, theme: &Theme) -> Style {
    if line.starts_with("+++") || line.starts_with("---") {
        Style::default()
            .fg(theme.subtext)
            .add_modifier(Modifier::BOLD)
    } else if line.starts_with("@@") {
        Style::default().fg(theme.highlight)
    } else if line.starts_with('+') {
        Style::default().fg(theme.success)
    } else if line.starts_with('-') {
        Style::default().fg(theme.error)
    } else {
        Style::default().fg(theme.text)
    }
}

fn tool_display_name(name: &str, raw_args: &str) -> String {
    let args = parse_tool_args(raw_args);
    match name {
        "read_file" => "Read".into(),
        "grep" => "Search".into(),
        "write_file" => {
            if string_arg(args.as_ref(), "content").is_some_and(|content| content.is_empty()) {
                "Create".into()
            } else {
                "Write".into()
            }
        }
        "list_directory" => "List".into(),
        "bash" => "Bash".into(),
        "patch" => "Edit".into(),
        "web_search" => "Fetch".into(),
        "bus:fetch" => "Fetch".into(),
        _ => title_case_tool_name(name),
    }
}

fn tool_argument_summary(name: &str, raw_args: &str, max_width: usize) -> String {
    let Some(args) = parse_tool_args(raw_args) else {
        return truncate_to_width(
            &raw_args.split_whitespace().collect::<Vec<_>>().join(" "),
            max_width,
        );
    };

    let summary = match name {
        "bash" => string_arg(Some(&args), "command"),
        "read_file" | "write_file" | "list_directory" => string_arg(Some(&args), "path"),
        "patch" => string_arg(Some(&args), "file_path").or_else(|| string_arg(Some(&args), "path")),
        "web_search" => string_arg(Some(&args), "url"),
        "bus:fetch" => string_arg(Some(&args), "key"),
        "grep" => grep_summary(&args),
        _ => generic_tool_summary(&args),
    };

    truncate_to_width(&summary.unwrap_or_else(|| args.to_string()), max_width)
}

fn parse_tool_args(raw: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(raw.trim()).ok()
}

fn string_arg(args: Option<&serde_json::Value>, key: &str) -> Option<String> {
    let value = args?.get(key)?;
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Null => None,
        value => Some(value.to_string()),
    }
}

fn grep_summary(args: &serde_json::Value) -> Option<String> {
    let pattern = string_arg(Some(args), "pattern")?;
    match string_arg(Some(args), "path") {
        Some(path) if !path.is_empty() => Some(format!("{pattern} in {path}")),
        _ => Some(pattern),
    }
}

fn generic_tool_summary(args: &serde_json::Value) -> Option<String> {
    for key in [
        "path",
        "file_path",
        "command",
        "query",
        "pattern",
        "url",
        "name",
    ] {
        if let Some(value) = string_arg(Some(args), key).filter(|value| !value.is_empty()) {
            return Some(value);
        }
    }
    None
}

fn title_case_tool_name(name: &str) -> String {
    name.split(['_', '-', ':'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut word = String::new();
                    word.extend(first.to_uppercase());
                    word.push_str(chars.as_str());
                    word
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width > max_width {
            if max_width > 1 {
                out.push('…');
            }
            break;
        }
        out.push(ch);
        width += char_width;
    }
    out
}

fn short_duration(duration: std::time::Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", duration.as_secs_f32())
    }
}

fn labelled_lines(
    label: &'static str,
    label_style: Style,
    body_style: Style,
    content: &str,
    usable_width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = UnicodeWidthStr::width(label) + 1;
    let body_width = usable_width.saturating_sub(prefix_width).max(1);
    let indent = " ".repeat(prefix_width);
    let mut lines = Vec::new();

    for (logical_idx, line) in normalized_lines(content).enumerate() {
        for (chunk_idx, chunk) in wrap_line(line, body_width).into_iter().enumerate() {
            if logical_idx == 0 && chunk_idx == 0 {
                lines.push(Line::from(vec![
                    Span::styled(label, label_style),
                    Span::raw(" "),
                    Span::styled(chunk, body_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(chunk, body_style),
                ]));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(label, label_style),
            Span::raw(" "),
        ]));
    }

    lines
}

fn wrap_line(line: &str, max_width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in line.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && current_width.saturating_add(char_width) > max_width {
            chunks.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width = current_width.saturating_add(char_width);
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn normalized_lines(content: &str) -> impl Iterator<Item = &str> {
    let content = if content.is_empty() { " " } else { content };
    content.split('\n')
}

#[cfg(test)]
mod tests {
    use super::super::super::state::FileDiffDisplay;
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn tool_header_uses_cc_shape_for_bash() {
        let msg = ChatMessage::tool_call(
            "call_1".into(),
            "bash".into(),
            serde_json::json!({ "command": "cargo test --all" }).to_string(),
        );

        let lines = tool_lines(&msg, &Theme::default(), 80);

        assert_eq!(line_text(&lines[0]), "● Bash(cargo test --all)");
        assert_eq!(line_text(&lines[1]), "  ⎿  Running...");
    }

    #[test]
    fn tool_header_uses_friendly_name_and_key_arg() {
        let msg = ChatMessage::tool_call(
            "call_1".into(),
            "read_file".into(),
            serde_json::json!({ "path": "lattice-cli/src/main.rs" }).to_string(),
        );

        let lines = tool_lines(&msg, &Theme::default(), 80);

        assert_eq!(line_text(&lines[0]), "● Read(lattice-cli/src/main.rs)");
    }

    #[test]
    fn tool_result_uses_response_prefix_and_collapse_hint() {
        let mut msg = ChatMessage::tool_call(
            "call_1".into(),
            "grep".into(),
            serde_json::json!({ "pattern": "Runtime", "path": "lattice-cli/src" }).to_string(),
        );
        msg.collapsed = true;
        let tool = msg.tool.as_mut().unwrap();
        tool.status = ToolStatus::Done;
        tool.finished_at = Some(tool.started_at + std::time::Duration::from_millis(25));
        tool.result = Some(["one", "two", "three", "four", "five"].join("\n"));

        let lines = tool_lines(&msg, &Theme::default(), 80);

        assert_eq!(line_text(&lines[0]), "● Search(Runtime in lattice-cli/src)");
        assert_eq!(line_text(&lines[1]), "  ⎿  one");
        assert_eq!(line_text(&lines[4]), "     four");
        assert_eq!(
            line_text(&lines[5]),
            "  ⎿  … 1 more lines · Ctrl+E to expand"
        );
    }

    #[test]
    fn tool_result_renders_file_diff_block() {
        let mut msg = ChatMessage::tool_call(
            "call_1".into(),
            "write_file".into(),
            serde_json::json!({ "path": "notes.txt", "content": "new\nsame\n" }).to_string(),
        );
        msg.collapsed = false;
        let tool = msg.tool.as_mut().unwrap();
        tool.status = ToolStatus::Done;
        tool.finished_at = Some(tool.started_at + std::time::Duration::from_millis(25));
        tool.result = Some("Wrote 9 bytes to notes.txt".into());
        tool.file_diff = Some(FileDiffDisplay {
            path: "notes.txt".into(),
            text: "--- a/notes.txt\n+++ b/notes.txt\n@@ -1,2 +1,2 @@\n-old\n+new\n same".into(),
        });

        let lines = tool_lines(&msg, &Theme::default(), 80);
        let text: Vec<String> = lines.iter().map(line_text).collect();

        assert!(text.contains(&"  ⎿  Diff notes.txt".to_string()));
        assert!(text.iter().any(|line| line.starts_with("     ╌")));
        assert!(text.contains(&"  │  -old".to_string()));
        assert!(text.contains(&"  │  +new".to_string()));
    }
}
