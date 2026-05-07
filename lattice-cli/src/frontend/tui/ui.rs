use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::Clear,
    Frame,
};

use super::app::App;
use super::theme::Theme;
use super::widgets::{
    composer::{input_height, render_bottom, set_cursor},
    menu::render_menu_overlay,
    transcript::render_transcript,
};

pub(super) fn draw(f: &mut Frame, app: &App) {
    let theme = Theme::catppuccin_mocha();
    let size = f.area();
    f.render_widget(Clear, size);
    app.click_zones.borrow_mut().clear();

    if let Some(ref menu) = app.menu {
        render_menu_overlay(f, size, menu, &theme);
        return;
    }

    let suggestions = app.slash_suggestions();
    let bottom_height = bottom_height(app, suggestions.len(), size.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(bottom_height)])
        .split(size);

    render_transcript(f, chunks[0], app, &theme);
    let input_area = render_bottom(f, chunks[1], app, &theme, &suggestions);
    set_cursor(f, input_area, app);
}

fn bottom_height(app: &App, suggestion_count: usize, terminal_height: u16) -> u16 {
    let help: u16 = if app.help_open { 10 } else { 0 };
    let suggestions = if suggestion_count > 0 {
        suggestion_count.min(6) as u16 + 2
    } else {
        0
    };
    let composer = input_height(app).saturating_add(1);
    help.saturating_add(suggestions)
        .saturating_add(composer)
        .min(terminal_height.saturating_sub(1).max(4))
}
