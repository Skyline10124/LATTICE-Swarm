use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::frontend::tui::state::{MenuKind, MenuState};
use crate::frontend::tui::theme::Theme;

pub(in crate::frontend::tui) fn render_menu_overlay(
    f: &mut Frame,
    area: Rect,
    menu: &MenuState,
    theme: &Theme,
) {
    let title = match menu.kind {
        MenuKind::Model => " Select Model ",
        MenuKind::Provider => " Select Provider ",
        MenuKind::Permissions => " Select Permissions ",
        MenuKind::Plugin => " Select Plugin ",
    };
    let max_h = (menu.options.len() as u16 + 3).min(area.height.saturating_sub(4));
    let max_w = 50u16.min(area.width.saturating_sub(4));
    let menu_area = Rect {
        x: area
            .x
            .saturating_add((area.width.saturating_sub(max_w)) / 2),
        y: area
            .y
            .saturating_add((area.height.saturating_sub(max_h)) / 2),
        width: max_w,
        height: max_h,
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.surface));
    let inner = block.inner(menu_area);
    f.render_widget(Clear, menu_area);
    f.render_widget(block, menu_area);

    let visible_h = inner.height as usize;
    let start = menu
        .index
        .saturating_sub(visible_h.saturating_sub(1))
        .min(menu.options.len().saturating_sub(visible_h));
    let visible: Vec<&str> = menu
        .options
        .iter()
        .skip(start)
        .take(visible_h)
        .map(|s| s.as_str())
        .collect();

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let real_idx = start + i;
            if real_idx == menu.index {
                Line::from(vec![
                    Span::styled(
                        " ▶ ",
                        Style::default()
                            .fg(theme.highlight)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        *opt,
                        Style::default()
                            .fg(theme.text)
                            .bg(theme.highlight)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        " ".repeat(
                            inner
                                .width
                                .saturating_sub(3 + UnicodeWidthStr::width(*opt) as u16)
                                as usize,
                        ),
                        Style::default().bg(theme.highlight),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(*opt, Style::default().fg(theme.text)),
                ])
            }
        })
        .collect();

    f.render_widget(Paragraph::new(Text::from(lines)), inner);

    let hint = "↑↓ navigate  Enter select  Esc close";
    let hint_line = Line::from(vec![Span::styled(hint, Style::default().fg(theme.subtext))]);
    let hint_area = Rect {
        x: menu_area.x,
        y: menu_area
            .y
            .saturating_add(menu_area.height)
            .saturating_sub(1),
        width: menu_area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(hint_line).alignment(Alignment::Center),
        hint_area,
    );
}
