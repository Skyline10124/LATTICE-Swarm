use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::frontend::tui::app::App;
use crate::frontend::tui::state::{AppStatus, SlashSuggestion};
use crate::frontend::tui::theme::Theme;
use crate::frontend::tui::widgets::statusline::Statusline;

const PROMPT_PREFIX: &str = "❯ ";

pub(in crate::frontend::tui) fn input_height(app: &App) -> u16 {
    let line_count = app.input.lines().count().max(1) as u16;
    line_count.saturating_add(2).clamp(3, 7)
}

pub(in crate::frontend::tui) fn render_bottom(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    suggestions: &[SlashSuggestion],
) -> Rect {
    let help_height = if app.help_open { 9 } else { 0 };
    let suggestion_height = if suggestions.is_empty() {
        0
    } else {
        suggestions.len().min(6) as u16 + 2
    };
    let composer_height = input_height(app);

    let constraints = [
        Constraint::Length(help_height),
        Constraint::Length(suggestion_height),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ];
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    if help_height > 0 {
        render_help(f, chunks[0], theme);
    }
    if suggestion_height > 0 {
        render_suggestions(f, chunks[1], suggestions, app.suggestion_index, theme);
    }
    render_composer(f, chunks[2], app, theme);
    Statusline::new(*theme).render(chunks[3], f.buffer_mut(), app);

    chunks[2]
}

pub(in crate::frontend::tui) fn set_cursor(f: &mut Frame, input_area: Rect, app: &App) {
    if input_area.width < 2 || input_area.height < 2 {
        return;
    }

    let text_before = &app.input[..app.input_cursor];
    let line_index = text_before.bytes().filter(|b| *b == b'\n').count() as u16;
    let current_line = text_before
        .rsplit_once('\n')
        .map_or(text_before, |(_, tail)| tail);
    let prefix_width = if line_index == 0 {
        UnicodeWidthStr::width(PROMPT_PREFIX) as u16
    } else {
        2
    };
    let visual_x = UnicodeWidthStr::width(current_line) as u16;
    let cursor_x = input_area
        .x
        .saturating_add(prefix_width)
        .saturating_add(visual_x.min(input_area.width.saturating_sub(prefix_width + 1)));
    let cursor_y = input_area
        .y
        .saturating_add(1)
        .saturating_add(line_index.min(input_area.height.saturating_sub(2)));
    f.set_cursor_position((cursor_x, cursor_y));
}

fn render_help(f: &mut Frame, area: Rect, theme: &Theme) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .title(" shortcuts ")
        .borders(Borders::TOP)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = vec![
        help_row("Enter", "send", "Shift+Enter", "newline", theme),
        help_row("?", "toggle help", "Tab", "complete cmd", theme),
        help_row("Ctrl+N", "new session", "Ctrl+L", "clear transcript", theme),
        help_row("Ctrl+F", "find", "Ctrl+G", "next match", theme),
        help_row("Ctrl+O", "thinking", "Ctrl+E", "expand tool", theme),
        help_row("/permissions", "sandbox", "/plugins", "plugins", theme),
        help_row("Esc", "clear/close", "End", "jump bottom", theme),
        help_row("↑/↓", "scroll/history", "Ctrl+C", "quit", theme),
    ];

    f.render_widget(Paragraph::new(Text::from(rows)), inner);
}

fn help_row<'a>(
    left_key: &'static str,
    left_desc: &'static str,
    right_key: &'static str,
    right_desc: &'static str,
    theme: &Theme,
) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{left_key:<12}"),
            Style::default().fg(theme.highlight),
        ),
        Span::styled(
            format!("{left_desc:<18}"),
            Style::default().fg(theme.subtext),
        ),
        Span::styled(
            format!("{right_key:<12}"),
            Style::default().fg(theme.highlight),
        ),
        Span::styled(right_desc, Style::default().fg(theme.subtext)),
    ])
}

fn render_suggestions(
    f: &mut Frame,
    area: Rect,
    suggestions: &[SlashSuggestion],
    selected_index: usize,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .title(" commands ")
        .borders(Borders::TOP)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = suggestions
        .iter()
        .take(inner.height as usize)
        .enumerate()
        .map(|(idx, suggestion)| {
            let selected = idx == selected_index.min(suggestions.len().saturating_sub(1));
            let marker = if selected { "▶ " } else { "  " };
            let command_style = if selected {
                Style::default()
                    .fg(theme.bg)
                    .bg(theme.highlight)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD)
            };
            let desc_style = if selected {
                Style::default().fg(theme.bg).bg(theme.highlight)
            } else {
                Style::default().fg(theme.subtext)
            };
            Line::from(vec![
                Span::styled(format!("{marker}{:<15}", suggestion.command), command_style),
                Span::styled(suggestion.description, desc_style),
            ])
        })
        .collect::<Vec<_>>();

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_composer(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let placeholder = if app.status == AppStatus::Streaming {
        "Type now to queue the next prompt"
    } else if app.search.is_active() {
        "Esc clears search · /next and /prev navigate matches"
    } else {
        "Type a message or / for commands"
    };

    let input = if app.input.is_empty() {
        Text::from(Line::from(vec![
            Span::styled(PROMPT_PREFIX, Style::default().fg(theme.subtext)),
            Span::styled(placeholder, Style::default().fg(theme.subtext)),
        ]))
    } else {
        prefixed_input(&app.input, theme)
    };

    f.render_widget(
        Paragraph::new(input)
            .style(theme.input_style())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn prefixed_input<'a>(input: &'a str, theme: &Theme) -> Text<'a> {
    let mut lines = Vec::new();
    for (idx, line) in input.lines().enumerate() {
        if idx == 0 {
            lines.push(Line::from(vec![
                Span::styled(PROMPT_PREFIX, Style::default().fg(theme.assistant_accent)),
                Span::styled(line, Style::default().fg(theme.text)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(line, Style::default().fg(theme.text)),
            ]));
        }
    }
    Text::from(lines)
}
