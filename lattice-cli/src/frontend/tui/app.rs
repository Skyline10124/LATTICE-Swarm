use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::Result;
use base64::Engine as _;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use lattice_agent::{prompt::SystemPromptDelta, Agent, LoopEvent};
use lattice_core::types::{Role, ToolDefinition};
use lattice_plugin::registry::PluginRegistry;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex;

use crate::coding_agent::{self, CodingAgentBuildOptions};

use super::event::Event;

/// Helper: find byte index of the character just before `cursor`.
fn prev_char_boundary(s: &str, cursor: usize) -> usize {
    s[..cursor]
        .char_indices()
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Helper: byte length of the character starting at `cursor`.
fn char_byte_len(s: &str, cursor: usize) -> usize {
    s[cursor..]
        .chars()
        .next()
        .map(|c| c.len_utf8())
        .unwrap_or(1)
}

/// Pack an error string into a terminal StreamToken event.
fn pack_error(turn_id: u64, msg: String) -> Event {
    Event::StreamToken {
        turn_id,
        content: msg.clone(),
        reasoning: None,
        done: true,
        error: Some(msg),
    }
}

fn dispatch_loop_event(
    tx: &UnboundedSender<Event>,
    turn_id: u64,
    event: LoopEvent,
    errored: &std::sync::atomic::AtomicBool,
) {
    match event {
        LoopEvent::Token { text } => {
            let _ = tx.send(Event::StreamToken {
                turn_id,
                content: text,
                reasoning: None,
                done: false,
                error: None,
            });
        }
        LoopEvent::Reasoning { text } => {
            let _ = tx.send(Event::StreamToken {
                turn_id,
                content: String::new(),
                reasoning: Some(text),
                done: false,
                error: None,
            });
        }
        LoopEvent::ToolCallRequired { calls } => {
            for call in calls {
                let _ = tx.send(Event::ToolOutput {
                    turn_id,
                    call_id: call.id,
                    name: call.function.name,
                    arguments: call.function.arguments,
                    result: None,
                });
            }
        }
        LoopEvent::ToolResult { call, result } => {
            let _ = tx.send(Event::ToolOutput {
                turn_id,
                call_id: call.id,
                name: call.function.name,
                arguments: call.function.arguments,
                result: Some(result),
            });
        }
        LoopEvent::Done { .. } => {}
        LoopEvent::Error { message, .. } => {
            errored.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(Event::StreamToken {
                turn_id,
                content: message.clone(),
                reasoning: None,
                done: true,
                error: Some(message),
            });
        }
    }
}

/// A single message in the chat.
pub(super) struct ChatMessage {
    pub(super) role: Role,
    pub(super) content: String,
    pub(super) reasoning: Option<String>,
    pub(super) collapsed: bool,
    pub(super) reasoning_collapsed: bool,
    pub(super) tool: Option<ToolDisplay>,
    cached_lines:
        std::cell::RefCell<Option<(usize, std::sync::Arc<Vec<ratatui::text::Line<'static>>>)>>,
    cached_width: std::cell::Cell<Option<u16>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ToolStatus {
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone)]
pub(super) struct ToolDisplay {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) arguments: String,
    pub(super) result: Option<String>,
    pub(super) status: ToolStatus,
    pub(super) started_at: std::time::Instant,
    pub(super) finished_at: Option<std::time::Instant>,
}

impl Clone for ChatMessage {
    fn clone(&self) -> Self {
        Self {
            role: self.role.clone(),
            content: self.content.clone(),
            reasoning: self.reasoning.clone(),
            collapsed: self.collapsed,
            reasoning_collapsed: self.reasoning_collapsed,
            tool: self.tool.clone(),
            cached_lines: std::cell::RefCell::new(None),
            cached_width: std::cell::Cell::new(None),
        }
    }
}

impl std::fmt::Debug for ChatMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatMessage")
            .field("role", &self.role)
            .field("content", &self.content)
            .field("reasoning", &self.reasoning)
            .field("collapsed", &self.collapsed)
            .field("reasoning_collapsed", &self.reasoning_collapsed)
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}

impl ChatMessage {
    pub(super) fn new(role: Role, content: String) -> Self {
        Self {
            role,
            content,
            reasoning: None,
            collapsed: false,
            reasoning_collapsed: true,
            tool: None,
            cached_lines: std::cell::RefCell::new(None),
            cached_width: std::cell::Cell::new(None),
        }
    }

    pub(super) fn tool_call(call_id: String, name: String, arguments: String) -> Self {
        let mut message = Self::new(Role::Tool, String::new());
        message.collapsed = true;
        message.tool = Some(ToolDisplay {
            call_id,
            name,
            arguments,
            result: None,
            status: ToolStatus::Running,
            started_at: std::time::Instant::now(),
            finished_at: None,
        });
        message
    }

    pub(super) fn set_cache(
        &self,
        content_len: usize,
        lines: Vec<ratatui::text::Line<'static>>,
        width: u16,
    ) {
        self.cached_lines
            .replace(Some((content_len, std::sync::Arc::new(lines))));
        self.cached_width.set(Some(width));
    }

    pub(super) fn get_cache(
        &self,
        content_len: usize,
        width: u16,
    ) -> Option<std::sync::Arc<Vec<ratatui::text::Line<'static>>>> {
        if self.cached_width.get() == Some(width) {
            self.cached_lines
                .borrow()
                .as_ref()
                .filter(|(cl, _)| *cl == content_len)
                .map(|(_, lines)| lines.clone())
        } else {
            None
        }
    }

    pub(super) fn invalidate_cache(&self) {
        self.cached_lines.replace(None);
        self.cached_width.set(None);
    }
}

/// Send text to the terminal clipboard via OSC 52.
/// Works in most modern terminals (kitty, wezterm, foot, iterm2, windows terminal).
fn write_osc52_clipboard(text: &str) {
    use std::io::Write;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let osc = format!("\x1b]52;c;{}\x07", encoded);
    let _ = std::io::stdout().write_all(osc.as_bytes());
    let _ = std::io::stdout().flush();
}

type ClipboardSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub(super) struct SlashSuggestion {
    pub(super) command: &'static str,
    pub(super) description: &'static str,
}

const SLASH_COMMANDS: &[SlashSuggestion] = &[
    SlashSuggestion {
        command: "/help",
        description: "show keyboard shortcuts",
    },
    SlashSuggestion {
        command: "/clear",
        description: "clear visible transcript",
    },
    SlashSuggestion {
        command: "/new",
        description: "start a fresh agent context",
    },
    SlashSuggestion {
        command: "/model <name>",
        description: "switch model or show current",
    },
    SlashSuggestion {
        command: "/provider <name>",
        description: "set provider override or show",
    },
    SlashSuggestion {
        command: "/status",
        description: "show current session state",
    },
    SlashSuggestion {
        command: "/permissions <mode>",
        description: "switch sandbox mode for later turns",
    },
    SlashSuggestion {
        command: "/plugins",
        description: "list available runtime plugins",
    },
    SlashSuggestion {
        command: "/plugin <name>",
        description: "run a prompt through a plugin",
    },
    SlashSuggestion {
        command: "/tokens",
        description: "show token breakdown",
    },
    SlashSuggestion {
        command: "/find <text>",
        description: "search the visible transcript",
    },
    SlashSuggestion {
        command: "/next",
        description: "jump to next search match",
    },
    SlashSuggestion {
        command: "/prev",
        description: "jump to previous search match",
    },
    SlashSuggestion {
        command: "/effort <off|low..max>",
        description: "set thinking effort level",
    },
    SlashSuggestion {
        command: "/trace",
        description: "toggle thinking trace on/off",
    },
    SlashSuggestion {
        command: "/expand",
        description: "expand last collapsed tool output",
    },
    SlashSuggestion {
        command: "/copy",
        description: "copy last assistant response",
    },
    SlashSuggestion {
        command: "/save",
        description: "force-save current session",
    },
    SlashSuggestion {
        command: "/queue",
        description: "show or clear queued prompts",
    },
    SlashSuggestion {
        command: "/sessions restore <id>",
        description: "list or restore a session",
    },
    SlashSuggestion {
        command: "/quit",
        description: "exit the TUI",
    },
];

impl SlashSuggestion {
    pub(super) fn name(&self) -> &'static str {
        self.command
            .split_whitespace()
            .next()
            .unwrap_or(self.command)
    }

    fn completion(&self) -> String {
        match self.command.find('<') {
            Some(idx) => self.command[..idx].trim_end().to_string() + " ",
            None => self.name().to_string(),
        }
    }
}

/// Application state.
pub(super) struct App {
    pub(super) should_quit: bool,
    pub(super) messages: Vec<ChatMessage>,
    pub(super) input: String,
    pub(super) input_cursor: usize,
    pub(super) status: AppStatus,
    pub(super) current_model: String,
    pub(super) previous_model: String,
    pub(super) current_provider: String,
    pub(super) token_count: usize,
    pub(super) scroll_offset: usize,
    pub(super) help_open: bool,
    pub(super) transcript_mode: bool,
    pub(super) spinner_index: usize,
    pub(super) provider_override: Option<String>,
    pub(super) credentials: HashMap<String, String>,
    pub(super) workdir: std::path::PathBuf,
    pub(super) save_sessions: bool,
    pub(super) security: crate::security::RuntimeSecurity,
    pub(super) security_config: crate::config::SecurityConfig,
    pub(super) plugin_registry: Option<Arc<PluginRegistry>>,
    pub(super) session: Option<crate::session::Session>,
    pub(super) pending_user: Option<String>,
    pub(super) event_tx: Option<UnboundedSender<Event>>,
    pub(super) agent: Option<Arc<Mutex<Agent>>>,
    pub(super) active_turn_id: Option<u64>,
    pub(super) next_turn_id: u64,
    pub(super) active_assistant_index: Option<usize>,
    pub(super) queued_inputs: VecDeque<String>,
    pub(super) suggestion_index: usize,
    prepared_submission: Option<PreparedSubmission>,
    input_history: Vec<String>,
    history_index: Option<usize>,
    draft_before_history: String,
    pub(super) click_zones: std::cell::RefCell<Vec<ClickZone>>,
    pub(super) stream_started: Option<std::time::Instant>,
    pub(super) reasoning_started: Option<std::time::Instant>,
    pub(super) reasoning_duration: Option<std::time::Duration>,
    pub(super) thinking_effort: Option<String>,
    pub(super) context_limit: u32,
    pub(super) selection: Option<TextSelection>,
    /// Text of each visible transcript row (populated during render, used for copy-on-select)
    pub(super) visible_rows: std::cell::RefCell<Vec<String>>,
    pub(super) visible_rows_origin: std::cell::Cell<u16>,
    pub(super) menu: Option<MenuState>,
    pub(super) search: SearchState,
    active_plugin: Option<RunPluginContext>,
    clipboard: ClipboardSink,
}

#[derive(Debug, Clone)]
pub(super) struct MenuState {
    pub(super) kind: MenuKind,
    pub(super) options: Vec<String>,
    pub(super) index: usize,
}

#[derive(Debug, Clone)]
pub(super) enum MenuKind {
    Model,
    Provider,
    Permissions,
    Plugin,
}

#[derive(Debug, Clone, Default)]
pub(super) struct SearchState {
    pub(super) query: String,
    pub(super) matches: Vec<usize>,
    pub(super) index: Option<usize>,
    pub(super) target_message: Option<usize>,
}

impl SearchState {
    pub(super) fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    pub(super) fn position(&self) -> Option<(usize, usize)> {
        self.index
            .filter(|_| !self.matches.is_empty())
            .map(|idx| (idx + 1, self.matches.len()))
    }
}

#[derive(Debug, Clone)]
struct PreparedSubmission {
    display: String,
    prompt: String,
    raw_prompt: Option<String>,
    plugin: Option<RunPluginContext>,
}

#[derive(Debug, Clone)]
struct RunPluginContext {
    name: String,
    system_prompt: String,
    output_contract_delta: Option<SystemPromptDelta>,
    tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone)]
pub(super) struct TextSelection {
    pub(super) start_row: u16,
    pub(super) start_col: u16,
    pub(super) end_row: u16,
    pub(super) end_col: u16,
    pub(super) active: bool, // true while mouse button held
}

#[derive(Debug, Clone)]
pub(super) struct ClickZone {
    pub(super) rect: (u16, u16, u16, u16), // x, y, w, h
    pub(super) action: ClickAction,
}

#[derive(Debug, Clone)]
pub(super) enum ClickAction {
    JumpToBottom,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum AppStatus {
    Ready,
    Streaming,
    Error(String),
}

impl App {
    pub(super) fn new() -> Self {
        Self {
            should_quit: false,
            messages: vec![],
            input: String::new(),
            input_cursor: 0,
            status: AppStatus::Ready,
            current_model: "deepseek-v4-flash".into(),
            previous_model: String::new(),
            current_provider: "".into(),
            token_count: 0,
            scroll_offset: 0,
            help_open: false,
            transcript_mode: false,
            spinner_index: 0,
            provider_override: None,
            credentials: HashMap::new(),
            workdir: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            save_sessions: false,
            security_config: crate::config::SecurityConfig::default(),
            security: crate::security::default_runtime_security(
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .expect("default runtime security should build"),
            plugin_registry: crate::plugins::build_plugin_registry(None).ok(),
            session: None,
            pending_user: None,
            event_tx: None,
            agent: None,
            active_turn_id: None,
            next_turn_id: 1,
            active_assistant_index: None,
            queued_inputs: VecDeque::new(),
            suggestion_index: 0,
            prepared_submission: None,
            input_history: Vec::new(),
            history_index: None,
            draft_before_history: String::new(),
            click_zones: std::cell::RefCell::new(Vec::new()),
            menu: None,
            stream_started: None,
            reasoning_started: None,
            reasoning_duration: None,
            thinking_effort: None,
            context_limit: 128000,
            selection: None,
            visible_rows: std::cell::RefCell::new(Vec::new()),
            visible_rows_origin: std::cell::Cell::new(0),
            search: SearchState::default(),
            active_plugin: None,
            clipboard: Arc::new(write_osc52_clipboard),
        }
    }

    pub(super) fn load_session(&mut self, session: crate::session::Session) {
        self.cancel_active_turn();
        self.messages = session
            .messages
            .iter()
            .map(|msg| ChatMessage::new(role_from_session(&msg.role), msg.content.clone()))
            .collect();
        self.current_provider = session.provider.clone();
        self.session = Some(session);
        self.agent = None;
        self.previous_model.clear();
        self.scroll_offset = 0;
        self.queued_inputs.clear();
        self.clear_search();
        self.prepared_submission = None;
        self.active_plugin = None;
    }

    fn begin_turn(&mut self, user: String) -> u64 {
        let turn_id = self.next_turn_id;
        self.next_turn_id = self.next_turn_id.wrapping_add(1).max(1);
        self.active_turn_id = Some(turn_id);
        self.active_assistant_index = Some(self.messages.len());
        self.pending_user = Some(user);
        turn_id
    }

    fn cancel_active_turn(&mut self) {
        self.active_turn_id = None;
        self.active_assistant_index = None;
        self.pending_user = None;
        if self.status == AppStatus::Streaming {
            self.status = AppStatus::Ready;
        }
        self.stream_started = None;
        self.reasoning_started = None;
        self.reasoning_duration = None;
    }

    pub(super) fn accepts_turn(&self, turn_id: u64) -> bool {
        self.active_turn_id == Some(turn_id)
    }

    #[cfg(test)]
    fn set_clipboard_sink(&mut self, sink: ClipboardSink) {
        self.clipboard = sink;
    }

    fn copy_to_clipboard(&self, text: &str) {
        (self.clipboard)(text);
    }

    fn toggle_recent_reasoning(&mut self) {
        if let Some(msg) = self.messages.iter_mut().rev().find(|msg| {
            msg.role == Role::Assistant && msg.reasoning.as_deref().is_some_and(|r| !r.is_empty())
        }) {
            msg.reasoning_collapsed = !msg.reasoning_collapsed;
            msg.invalidate_cache();
        } else {
            self.transcript_mode = !self.transcript_mode;
        }
    }

    pub(super) fn tick(&mut self) {
        self.spinner_index = self.spinner_index.wrapping_add(1);
    }

    pub(super) async fn reap_audit(&self) {
        crate::security::reap_audit(&self.security).await;
    }

    pub(super) async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // Menu navigation: intercept when menu is open
        if self.menu.is_some() {
            match key.code {
                KeyCode::Up => {
                    if let Some(ref mut menu) = self.menu {
                        if menu.index > 0 {
                            menu.index -= 1;
                        }
                    }
                    return Ok(());
                }
                KeyCode::Down => {
                    if let Some(ref mut menu) = self.menu {
                        if menu.index + 1 < menu.options.len() {
                            menu.index += 1;
                        }
                    }
                    return Ok(());
                }
                KeyCode::Enter => {
                    let action = if let Some(ref menu) = self.menu {
                        menu.options
                            .get(menu.index)
                            .cloned()
                            .map(|s| (menu.kind.clone(), s))
                    } else {
                        None
                    };
                    if let Some((kind, selected)) = action {
                        self.menu = None;
                        match kind {
                            MenuKind::Model => {
                                self.cancel_active_turn();
                                self.current_model = selected.clone();
                                self.previous_model.clear();
                                self.current_provider.clear();
                                self.agent = None;
                                self.push_system_message(&format!("Model: {selected}"));
                            }
                            MenuKind::Provider => {
                                self.cancel_active_turn();
                                self.provider_override = Some(selected.clone());
                                self.current_provider = selected.clone();
                                self.agent = None;
                                self.push_system_message(&format!("Provider: {selected}"));
                            }
                            MenuKind::Permissions => {
                                self.switch_permissions(&selected);
                            }
                            MenuKind::Plugin => {
                                self.input = format!("/plugin {selected} ");
                                self.input_cursor = self.input.len();
                                self.suggestion_index = 0;
                            }
                        }
                    }
                    return Ok(());
                }
                KeyCode::Esc | KeyCode::Char(' ') => {
                    self.menu = None;
                    return Ok(());
                }
                _ => {}
            }
        }

        let suggestions = self.slash_suggestions();
        if !suggestions.is_empty() {
            match key.code {
                KeyCode::Tab => {
                    self.accept_selected_suggestion();
                    return Ok(());
                }
                KeyCode::Up => {
                    self.suggestion_index = self.suggestion_index.saturating_sub(1);
                    return Ok(());
                }
                KeyCode::Down => {
                    if self.suggestion_index + 1 < suggestions.len() {
                        self.suggestion_index += 1;
                    }
                    return Ok(());
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancel_active_turn();
                self.messages.clear();
                self.queued_inputs.clear();
                self.clear_search();
                self.help_open = false;
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.reset_conversation();
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_recent_reasoning();
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input = "/find ".into();
                self.input_cursor = self.input.len();
                self.exit_history_mode();
                self.help_open = false;
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.jump_search_next();
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_tool_expand();
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.copy_last_assistant();
            }
            KeyCode::Char('?') if self.input.is_empty() => {
                self.help_open = !self.help_open;
            }
            KeyCode::Char(c) => {
                self.help_open = false;
                self.exit_history_mode();
                self.input.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();
                self.clamp_suggestion_index();
            }
            KeyCode::Backspace if self.input_cursor > 0 => {
                let prev = prev_char_boundary(&self.input, self.input_cursor);
                self.input.remove(prev);
                self.input_cursor = prev;
                self.clamp_suggestion_index();
            }
            KeyCode::Delete
                if self.input_cursor < self.input.len()
                    && self.input.is_char_boundary(self.input_cursor) =>
            {
                self.input.remove(self.input_cursor);
                self.clamp_suggestion_index();
            }
            KeyCode::Left if self.input_cursor > 0 => {
                self.input_cursor = prev_char_boundary(&self.input, self.input_cursor);
            }
            KeyCode::Right if self.input_cursor < self.input.len() => {
                let len = char_byte_len(&self.input, self.input_cursor);
                self.input_cursor = (self.input_cursor + len).min(self.input.len());
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => {
                if self.input.is_empty() && self.scroll_offset > 0 {
                    self.scroll_offset = 0;
                } else {
                    self.input_cursor = self.input.len();
                }
            }
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert(self.input_cursor, '\n');
                    self.input_cursor += '\n'.len_utf8();
                } else {
                    self.submit().await?;
                }
            }
            KeyCode::Esc => {
                if self.history_index.is_some() {
                    self.exit_history_mode();
                    self.input.clear();
                    self.input_cursor = 0;
                } else if self.help_open {
                    self.help_open = false;
                } else if self.search.is_active() && self.input.is_empty() {
                    self.clear_search();
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.input_cursor = 0;
                    self.suggestion_index = 0;
                }
            }
            KeyCode::Up => {
                if self.input.is_empty() && !self.input_history.is_empty() {
                    self.navigate_history_prev();
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_add(1);
                }
            }
            KeyCode::Down => {
                if self.history_index.is_some() {
                    self.navigate_history_next();
                } else if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(8);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(8);
            }
            _ => {}
        }
        Ok(())
    }

    /// Insert text at the cursor (e.g. from paste or IME commit).
    pub(super) fn insert_text(&mut self, text: &str) {
        self.exit_history_mode();
        for c in text.chars() {
            self.input.insert(self.input_cursor, c);
            self.input_cursor += c.len_utf8();
        }
        self.clamp_suggestion_index();
    }

    fn add_to_history(&mut self, text: String) {
        // Don't duplicate consecutive identical entries
        if self.input_history.last().map(|s| s.as_str()) != Some(text.as_str()) {
            self.input_history.push(text);
        }
        self.history_index = None;
    }

    fn navigate_history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            None => {
                self.draft_before_history = std::mem::take(&mut self.input);
                self.input_history.len().saturating_sub(1)
            }
            Some(i) if i > 0 => i - 1,
            _ => return,
        };
        self.history_index = Some(idx);
        self.input = self.input_history[idx].clone();
        self.input_cursor = self.input.len();
    }

    fn navigate_history_next(&mut self) {
        match self.history_index {
            Some(i) if i + 1 < self.input_history.len() => {
                self.history_index = Some(i + 1);
                self.input = self.input_history[i + 1].clone();
                self.input_cursor = self.input.len();
            }
            Some(_) => {
                self.history_index = None;
                self.input = std::mem::take(&mut self.draft_before_history);
                self.input_cursor = self.input.len();
            }
            None => {}
        }
    }

    fn exit_history_mode(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.draft_before_history.clear();
        }
    }

    pub(super) async fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        if self.menu.is_some() {
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(3);
            }
            MouseEventKind::ScrollDown if self.scroll_offset > 0 => {
                self.scroll_offset = self.scroll_offset.saturating_sub(3);
            }
            MouseEventKind::Down(_) => {
                let (col, row) = (mouse.column, mouse.row);
                // Check click zones first
                let action = self.click_zones.borrow().iter().find_map(|zone| {
                    let (zx, zy, zw, zh) = zone.rect;
                    if col >= zx && col < zx + zw && row >= zy && row < zy + zh {
                        Some(zone.action.clone())
                    } else {
                        None
                    }
                });
                if let Some(action) = action {
                    match action {
                        ClickAction::JumpToBottom => self.scroll_offset = 0,
                    }
                } else {
                    // Start text selection
                    self.selection = Some(TextSelection {
                        start_row: row,
                        start_col: col,
                        end_row: row,
                        end_col: col,
                        active: true,
                    });
                }
            }
            MouseEventKind::Drag(_) => {
                if let Some(ref mut sel) = self.selection {
                    if sel.active {
                        sel.end_row = mouse.row;
                        sel.end_col = mouse.column;
                    }
                }
            }
            MouseEventKind::Up(_) => {
                if let Some(sel) = self.selection.take() {
                    if sel.active && (sel.start_row != sel.end_row || sel.start_col != sel.end_col)
                    {
                        self.copy_selection(&sel);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn copy_selection(&self, sel: &TextSelection) {
        let (top, bot) = if sel.start_row <= sel.end_row {
            (sel.start_row, sel.end_row)
        } else {
            (sel.end_row, sel.start_row)
        };
        let rows = self.visible_rows.borrow();
        let origin = self.visible_rows_origin.get();
        let Some(start_row) = top.checked_sub(origin) else {
            return;
        };
        let Some(end_row) = bot.checked_sub(origin) else {
            return;
        };
        let start = (start_row as usize).min(rows.len());
        let end = (end_row as usize + 1).min(rows.len());
        if start < end {
            let text: String = rows[start..end]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                self.copy_to_clipboard(&trimmed);
            }
        }
    }

    async fn submit(&mut self) -> Result<()> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        self.add_to_history(text.clone());
        if self.handle_slash_command(&text) {
            if let Some(prepared) = self.prepared_submission.take() {
                return self.submit_prepared(prepared).await;
            }
            self.input.clear();
            self.input_cursor = 0;
            self.scroll_offset = 0;
            self.suggestion_index = 0;
            return Ok(());
        }
        if self.active_turn_id.is_some() || self.status == AppStatus::Streaming {
            self.queue_input(text);
            self.input.clear();
            self.input_cursor = 0;
            self.suggestion_index = 0;
            return Ok(());
        }

        // Bash mode: !<command>
        if let Some(cmd) = text.strip_prefix('!') {
            self.messages
                .push(ChatMessage::new(Role::User, text.clone()));
            self.input.clear();
            self.input_cursor = 0;
            self.suggestion_index = 0;
            self.scroll_offset = 0;
            self.status = AppStatus::Streaming;
            self.stream_started = Some(std::time::Instant::now());

            let output = match coding_agent::run_bash_tool(&self.workdir, &self.security, cmd).await
            {
                Ok(output) if output.trim().is_empty() => "(no output)".to_string(),
                Ok(output) => output.trim().to_string(),
                Err(err) => format!("bash: {err}"),
            };

            let collapsed = output.lines().count() > 6;
            let mut msg = ChatMessage::new(Role::Tool, output);
            msg.collapsed = collapsed;
            self.messages.push(msg);
            self.status = AppStatus::Ready;
            self.stream_started = None;
            return Ok(());
        }

        self.submit_model_turn(text.clone(), text, None).await
    }

    async fn submit_prepared(&mut self, prepared: PreparedSubmission) -> Result<()> {
        if self.active_turn_id.is_some() || self.status == AppStatus::Streaming {
            let queued = match prepared.raw_prompt.as_deref() {
                Some(raw) => prepared
                    .plugin
                    .as_ref()
                    .map(|plugin| format!("/plugin {} {}", plugin.name, raw))
                    .unwrap_or_else(|| prepared.display.clone()),
                None => prepared.display.clone(),
            };
            self.queue_input(queued);
            self.input.clear();
            self.input_cursor = 0;
            self.suggestion_index = 0;
            return Ok(());
        }
        self.submit_model_turn(prepared.display, prepared.prompt, prepared.plugin)
            .await
    }

    async fn submit_model_turn(
        &mut self,
        display_text: String,
        prompt_text: String,
        plugin: Option<RunPluginContext>,
    ) -> Result<()> {
        self.messages
            .push(ChatMessage::new(Role::User, display_text.clone()));
        self.token_count +=
            lattice_core::tokens::TokenEstimator::estimate_text(&display_text) as usize;
        self.input.clear();
        self.input_cursor = 0;
        self.suggestion_index = 0;
        self.scroll_offset = 0;
        self.status = AppStatus::Streaming;
        self.stream_started = Some(std::time::Instant::now());
        self.reasoning_started = None;
        self.reasoning_duration = None;
        let turn_id = self.begin_turn(display_text);

        self.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));

        let tx = match self.event_tx.clone() {
            Some(tx) => tx,
            None => {
                self.status = AppStatus::Error("Event channel not initialized".into());
                self.cancel_active_turn();
                return Ok(());
            }
        };

        let needs_rebuild = self.agent.is_none() || self.current_model != self.previous_model;

        if needs_rebuild {
            let prior_messages = if let Some(arc) = self.agent.as_ref() {
                let agent = arc.lock().await;
                agent.messages().to_vec()
            } else {
                vec![]
            };

            let build_options = CodingAgentBuildOptions {
                model: self.current_model.clone(),
                provider_override: self.provider_override.clone(),
                workdir: self.workdir.clone(),
                credentials: self.credentials.clone(),
                previous_session: self.session.clone(),
                prior_messages,
                thinking_effort: self.thinking_effort.clone(),
                security: self.security.clone(),
            };

            let built = match tokio::task::spawn_blocking(move || {
                coding_agent::build_coding_agent(build_options)
            })
            .await
            {
                Ok(Ok(built)) => built,
                Ok(Err(e)) => {
                    let _ = tx.send(pack_error(turn_id, format!("agent build failed: {e}")));
                    self.status = AppStatus::Ready;
                    return Ok(());
                }
                Err(_) => {
                    let _ = tx.send(pack_error(turn_id, "agent build task panicked".into()));
                    self.status = AppStatus::Ready;
                    return Ok(());
                }
            };

            self.context_limit = built.context_limit;
            self.current_model = built.model.clone();
            self.current_provider = built.provider.clone();
            self.active_plugin = None;
            let _ = tx.send(Event::ModelInfo {
                turn_id,
                model: built.model.clone(),
                provider: built.provider.clone(),
            });

            self.agent = Some(Arc::new(Mutex::new(built.agent)));
            self.previous_model = self.current_model.clone();
        }

        self.apply_requested_plugin_context(plugin.clone()).await;

        let agent_arc = self.agent.clone().unwrap();
        tokio::spawn(async move {
            let mut agent = agent_arc.lock().await;
            let errored = std::sync::atomic::AtomicBool::new(false);
            let _events = agent
                .run_streaming(&prompt_text, 10, |event| {
                    dispatch_loop_event(&tx, turn_id, event, &errored);
                })
                .await;

            if !errored.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tx.send(Event::StreamToken {
                    turn_id,
                    content: String::new(),
                    reasoning: None,
                    done: true,
                    error: None,
                });
            }
        });

        Ok(())
    }

    async fn apply_requested_plugin_context(&mut self, plugin: Option<RunPluginContext>) {
        let Some(agent_arc) = self.agent.as_ref() else {
            return;
        };
        if self.active_plugin.as_ref().map(|p| p.name.as_str())
            == plugin.as_ref().map(|p| p.name.as_str())
        {
            return;
        }

        let mut agent = agent_arc.lock().await;
        match plugin.as_ref() {
            Some(plugin) => {
                agent.set_system_prompt(&plugin_augmented_system_prompt(
                    &self.workdir,
                    &plugin.system_prompt,
                ));
                agent.set_output_contract_delta(plugin.output_contract_delta.clone());
                if !plugin.tools.is_empty() {
                    agent.add_tools(plugin.tools.clone());
                }
            }
            None => {
                agent.set_system_prompt(&coding_agent::coding_system_prompt(&self.workdir));
                agent.set_output_contract_delta(None);
            }
        }
        self.active_plugin = plugin;
    }

    fn handle_slash_command(&mut self, text: &str) -> bool {
        if !text.starts_with('/') {
            return false;
        }

        let mut parts = text.split_whitespace();
        let command = parts.next().unwrap_or_default();
        match command {
            "/help" => {
                self.help_open = true;
                true
            }
            "/clear" => {
                self.cancel_active_turn();
                self.messages.clear();
                self.help_open = false;
                true
            }
            "/new" => {
                self.reset_conversation();
                self.push_system_message("Started a fresh agent context.");
                true
            }
            "/model" => {
                let model = parts.collect::<Vec<_>>().join(" ");
                if model.is_empty() {
                    let models = coding_agent::authenticated_models(&self.credentials);
                    let idx = models
                        .iter()
                        .position(|m| *m == self.current_model)
                        .unwrap_or(0);
                    self.menu = Some(MenuState {
                        kind: MenuKind::Model,
                        options: models,
                        index: idx,
                    });
                } else {
                    self.cancel_active_turn();
                    self.current_model = model.clone();
                    self.previous_model.clear();
                    self.current_provider.clear();
                    self.status = AppStatus::Ready;
                    self.agent = None;
                    self.push_system_message(&format!("Model switched to {model}."));
                }
                true
            }
            "/status" => {
                let provider = if self.current_provider.is_empty() {
                    "unresolved"
                } else {
                    &self.current_provider
                };
                self.push_system_message(&format!(
                    "model={} provider={} messages={} tokens={} transcript={} sandbox={} plugins={}",
                    self.current_model,
                    provider,
                    self.messages.len(),
                    self.token_count,
                    if self.transcript_mode { "on" } else { "off" },
                    self.security.mode_label,
                    self.plugin_count(),
                ));
                true
            }
            "/permissions" | "/permission" => {
                let mode = parts.next().unwrap_or("");
                if mode.is_empty() {
                    let modes = permission_modes();
                    let idx = modes
                        .iter()
                        .position(|m| m == &self.security.mode_label)
                        .unwrap_or(0);
                    self.menu = Some(MenuState {
                        kind: MenuKind::Permissions,
                        options: modes,
                        index: idx,
                    });
                } else {
                    self.switch_permissions(mode);
                }
                true
            }
            "/plugins" => {
                let names = self.plugin_names();
                if names.is_empty() {
                    self.push_system_message("No plugins loaded.");
                } else {
                    let lines = names
                        .iter()
                        .filter_map(|name| self.plugin_summary(name))
                        .map(|(name, description)| format!("  {name:<16} {description}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.push_system_message(&format!(
                        "{} plugin(s) loaded · /plugin <name> <prompt>:\n{}",
                        names.len(),
                        lines
                    ));
                }
                true
            }
            "/plugin" => {
                let name = parts.next().unwrap_or("");
                let prompt = parts.collect::<Vec<_>>().join(" ");
                if name.is_empty() {
                    let names = self.plugin_names();
                    if names.is_empty() {
                        self.push_system_message("No plugins loaded.");
                    } else {
                        self.menu = Some(MenuState {
                            kind: MenuKind::Plugin,
                            options: names,
                            index: 0,
                        });
                    }
                } else if prompt.trim().is_empty() {
                    if self.has_plugin(name) {
                        self.input = format!("/plugin {name} ");
                        self.input_cursor = self.input.len();
                    } else {
                        self.push_system_message(&format!(
                            "Plugin not found: {name}. Use /plugins to list loaded plugins."
                        ));
                    }
                } else {
                    match self.prepare_plugin_prompt(name, prompt) {
                        Ok(()) => {}
                        Err(err) => self.push_system_message(&format!("Plugin failed: {err}")),
                    }
                }
                true
            }
            "/provider" => {
                let arg = parts.collect::<Vec<_>>().join(" ");
                if arg.is_empty() {
                    let providers = coding_agent::authenticated_providers(&self.credentials);
                    if providers.is_empty() {
                        let p = if self.current_provider.is_empty() {
                            "auto"
                        } else {
                            &self.current_provider
                        };
                        self.push_system_message(&format!("Current provider: {p}"));
                    } else {
                        let current = if self.current_provider.is_empty() {
                            "auto"
                        } else {
                            &self.current_provider
                        };
                        let idx = providers.iter().position(|p| p == current).unwrap_or(0);
                        self.menu = Some(MenuState {
                            kind: MenuKind::Provider,
                            options: providers,
                            index: idx,
                        });
                    }
                } else {
                    self.cancel_active_turn();
                    self.provider_override = Some(arg.clone());
                    self.current_provider = arg;
                    self.agent = None;
                    self.push_system_message(
                        "Provider override set. Agent will rebuild next turn.",
                    );
                }
                true
            }
            "/tokens" => {
                use lattice_core::tokens::TokenEstimator;
                let content_tokens: usize = self
                    .messages
                    .iter()
                    .map(|m| TokenEstimator::estimate_text(&m.content) as usize)
                    .sum();
                let reasoning_tokens: usize = self
                    .messages
                    .iter()
                    .filter_map(|m| {
                        m.reasoning
                            .as_ref()
                            .map(|r| TokenEstimator::estimate_text(r) as usize)
                    })
                    .sum();
                self.push_system_message(&format!(
                    "tokens: {} content + {} reasoning = {} total ({} messages)",
                    content_tokens,
                    reasoning_tokens,
                    content_tokens + reasoning_tokens,
                    self.messages.len(),
                ));
                true
            }
            "/find" | "/search" => {
                let query = parts.collect::<Vec<_>>().join(" ");
                if query.is_empty() {
                    if self.search.is_active() {
                        let status = match self.search.position() {
                            Some((current, total)) => {
                                format!(
                                    "Search: '{}' ({current}/{total}). Use /next, /prev, Esc to clear.",
                                    self.search.query
                                )
                            }
                            None => {
                                format!("Search: '{}' (no matches).", self.search.query)
                            }
                        };
                        self.push_system_message(&status);
                    } else {
                        self.push_system_message("Usage: /find <text>");
                    }
                } else {
                    self.start_search(query);
                }
                true
            }
            "/next" => {
                self.jump_search_next();
                true
            }
            "/prev" | "/previous" => {
                self.jump_search_prev();
                true
            }
            "/trace" => {
                self.transcript_mode = !self.transcript_mode;
                self.push_system_message(&format!(
                    "Trace {}. {}",
                    if self.transcript_mode { "on" } else { "off" },
                    if self.transcript_mode {
                        "Reasoning will be shown inline."
                    } else {
                        "Reasoning hidden by default · Ctrl+O expands the latest block."
                    },
                ));
                true
            }
            "/effort" => {
                let level = parts.next().unwrap_or("");
                match level {
                    "off" | "none" => {
                        self.cancel_active_turn();
                        self.thinking_effort = None;
                        self.agent = None;
                        self.push_system_message(
                            "Thinking effort: auto (model default). Agent will rebuild.",
                        );
                    }
                    "low" | "medium" | "high" | "xhigh" | "max" => {
                        self.cancel_active_turn();
                        self.thinking_effort = Some(level.to_string());
                        self.agent = None;
                        self.push_system_message(&format!(
                            "Thinking effort: {level}. Agent will rebuild next turn."
                        ));
                    }
                    "" => {
                        let current = self.thinking_effort.as_deref().unwrap_or("auto");
                        self.push_system_message(&format!(
                            "Thinking effort: {current}. Use /effort <off|low|medium|high|xhigh|max>."
                        ));
                    }
                    _ => {
                        self.push_system_message("Usage: /effort <off|low|medium|high|xhigh|max>");
                    }
                }
                true
            }
            "/expand" => {
                self.toggle_tool_expand();
                true
            }
            "/copy" => {
                self.copy_last_assistant();
                true
            }
            "/save" => {
                if let Some(ref session) = self.session {
                    let manager = crate::session::SessionManager::new();
                    match manager.save(session) {
                        Ok(_) => self.push_system_message(&format!(
                            "Session saved ({} messages, {} tok).",
                            session.messages.len(),
                            self.token_count,
                        )),
                        Err(e) => self.push_system_message(&format!("Save failed: {e}")),
                    }
                } else {
                    self.push_system_message("No session to save yet. Send a message first.");
                }
                true
            }
            "/queue" => {
                let sub = parts.next().unwrap_or("list");
                match sub {
                    "clear" => {
                        let count = self.queued_inputs.len();
                        self.queued_inputs.clear();
                        self.push_system_message(&format!("Cleared {count} queued prompt(s)."));
                    }
                    _ => {
                        if self.queued_inputs.is_empty() {
                            self.push_system_message("Queue is empty.");
                        } else {
                            let lines = self
                                .queued_inputs
                                .iter()
                                .enumerate()
                                .map(|(idx, queued)| {
                                    format!("  {}. {}", idx + 1, summarize_prompt(queued, 80))
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            self.push_system_message(&format!(
                                "{} queued prompt(s) · /queue clear to drop:\n{}",
                                self.queued_inputs.len(),
                                lines
                            ));
                        }
                    }
                }
                true
            }
            "/sessions" => {
                let sub = parts.next().unwrap_or("list");
                let manager = crate::session::SessionManager::new();
                match sub {
                    "restore" | "load" => {
                        let id = parts.next().unwrap_or("");
                        if id.is_empty() {
                            self.push_system_message("Usage: /sessions restore <id>");
                        } else {
                            // Try exact match first, then prefix match
                            let full_id = match manager.load(id) {
                                Ok(Some(_)) => Some(id.to_string()),
                                _ => {
                                    // Prefix search
                                    manager.list().ok().and_then(|sessions| {
                                        sessions
                                            .iter()
                                            .find(|s| s.id.starts_with(id))
                                            .map(|s| s.id.clone())
                                    })
                                }
                            };
                            match full_id {
                                Some(fid) => match manager.load(&fid) {
                                    Ok(Some(session)) => {
                                        let model = session.model.clone();
                                        let msgs = session.messages.len();
                                        self.load_session(session);
                                        self.push_system_message(&format!(
                                            "Restored session {} ({model}, {msgs} messages).",
                                            &fid[..fid.len().min(8)],
                                        ));
                                    }
                                    _ => self.push_system_message(&format!("Failed to load: {id}")),
                                },
                                None => {
                                    self.push_system_message(&format!("Session not found: {id}"));
                                }
                            }
                        }
                    }
                    _ => {
                        match manager.list() {
                            Ok(sessions) => {
                                if sessions.is_empty() {
                                    self.push_system_message("No saved sessions. Use /sessions restore <id> to load one.");
                                } else {
                                    let lines: Vec<String> = sessions
                                        .iter()
                                        .map(|s| {
                                            format!(
                                                "  {}  {} msgs  {}  {}",
                                                &s.id[..s.id.len().min(8)],
                                                s.message_count,
                                                s.model,
                                                &s.created_at[..s.created_at.len().min(16)],
                                            )
                                        })
                                        .collect();
                                    self.push_system_message(&format!(
                                        "{} session(s) · /sessions restore <id> to load:\n{}",
                                        sessions.len(),
                                        lines.join("\n"),
                                    ));
                                }
                            }
                            Err(e) => self.push_system_message(&format!("Failed: {e}")),
                        }
                    }
                }
                true
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
                true
            }
            _ => {
                self.push_system_message(&format!(
                    "Unknown command: {command}. Type /help for shortcuts."
                ));
                true
            }
        }
    }

    fn reset_conversation(&mut self) {
        self.messages.clear();
        self.input.clear();
        self.input_cursor = 0;
        self.scroll_offset = 0;
        self.help_open = false;
        self.token_count = 0;
        self.queued_inputs.clear();
        self.clear_search();
        self.suggestion_index = 0;
        self.agent = None;
        self.previous_model.clear();
        self.session = None;
        self.pending_user = None;
        self.prepared_submission = None;
        self.active_plugin = None;
        self.status = AppStatus::Ready;
    }

    fn push_system_message(&mut self, content: &str) {
        self.messages
            .push(ChatMessage::new(Role::System, content.to_string()));
    }

    fn switch_permissions(&mut self, mode: &str) {
        let normalized = mode.trim().to_ascii_lowercase();
        if !permission_modes()
            .iter()
            .any(|candidate| candidate == &normalized)
        {
            self.push_system_message("Usage: /permissions <project|strict|permissive|off>");
            return;
        }

        let mut next_config = self.security_config.clone();
        next_config.sandbox_mode = normalized.clone();
        match crate::security::build_runtime_security(&next_config, &self.workdir) {
            Ok(security) => {
                self.cancel_active_turn();
                self.security_config = next_config;
                self.security = security;
                self.agent = None;
                self.previous_model.clear();
                self.active_plugin = None;
                self.push_system_message(&format!(
                    "Permissions switched to '{}'. Agent will rebuild next turn.",
                    self.security.mode_label
                ));
            }
            Err(err) => {
                self.push_system_message(&format!("Permission switch failed: {err}"));
            }
        }
    }

    pub(super) fn plugin_count(&self) -> usize {
        self.plugin_registry
            .as_ref()
            .map_or(0, |registry| registry.len())
    }

    pub(super) fn active_plugin_name(&self) -> Option<&str> {
        self.active_plugin
            .as_ref()
            .map(|plugin| plugin.name.as_str())
    }

    fn plugin_names(&self) -> Vec<String> {
        self.plugin_registry
            .as_ref()
            .map(|registry| crate::plugins::sorted_plugin_names(registry))
            .unwrap_or_default()
    }

    fn has_plugin(&self, name: &str) -> bool {
        self.find_plugin_name(name).is_some()
    }

    fn plugin_summary(&self, name: &str) -> Option<(String, String)> {
        let registry = self.plugin_registry.as_ref()?;
        let canonical = self.find_plugin_name(name)?;
        let bundle = registry.get(&canonical)?;
        Some((bundle.meta.name.clone(), bundle.meta.description.clone()))
    }

    fn prepare_plugin_prompt(&mut self, name: &str, prompt: String) -> Result<()> {
        let registry = self
            .plugin_registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("plugin registry is not initialized"))?;
        let canonical = self
            .find_plugin_name(name)
            .ok_or_else(|| anyhow::anyhow!("plugin '{name}' is not loaded"))?;
        let bundle = registry
            .get(&canonical)
            .ok_or_else(|| anyhow::anyhow!("plugin '{name}' is not loaded"))?;
        let plugin_prompt = bundle
            .plugin
            .to_prompt_json(&plugin_context(&bundle.meta.name, &prompt, &self.workdir))
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let display = format!("@{} {}", bundle.meta.name, prompt);
        self.prepared_submission = Some(PreparedSubmission {
            display,
            prompt: plugin_prompt,
            raw_prompt: Some(prompt),
            plugin: Some(RunPluginContext {
                name: bundle.meta.name.clone(),
                system_prompt: bundle.plugin.system_prompt().to_string(),
                output_contract_delta: output_contract_delta(bundle.plugin.output_schema()),
                tools: bundle
                    .plugin
                    .tools()
                    .iter()
                    .cloned()
                    .chain(bundle.default_tools.iter().cloned())
                    .collect(),
            }),
        });
        Ok(())
    }

    fn find_plugin_name(&self, query: &str) -> Option<String> {
        let query = query.trim();
        self.plugin_names()
            .into_iter()
            .find(|name| name == query || name.eq_ignore_ascii_case(query))
    }

    pub(super) fn slash_suggestions(&self) -> Vec<SlashSuggestion> {
        let Some(query) = self.input.strip_prefix('/') else {
            return Vec::new();
        };
        if self.input.chars().any(char::is_whitespace) {
            return Vec::new();
        }

        let query = query.to_ascii_lowercase();
        SLASH_COMMANDS
            .iter()
            .copied()
            .filter(|s| s.command.trim_start_matches('/').starts_with(&query))
            .collect()
    }

    fn clamp_suggestion_index(&mut self) {
        let len = self.slash_suggestions().len();
        if len == 0 {
            self.suggestion_index = 0;
        } else if self.suggestion_index >= len {
            self.suggestion_index = len - 1;
        }
    }

    fn accept_selected_suggestion(&mut self) {
        let suggestions = self.slash_suggestions();
        let Some(suggestion) = suggestions.get(self.suggestion_index).copied() else {
            return;
        };
        let replacement = suggestion.completion();
        self.input = replacement;
        self.input_cursor = self.input.len();
        self.suggestion_index = 0;
    }

    fn queue_input(&mut self, text: String) {
        self.queued_inputs.push_back(text);
        let count = self.queued_inputs.len();
        self.push_system_message(&format!(
            "Queued prompt #{count}. It will run after the current response."
        ));
    }

    pub(super) async fn submit_next_queued(&mut self) -> Result<()> {
        if self.status != AppStatus::Ready || self.active_turn_id.is_some() {
            return Ok(());
        }
        let Some(next) = self.queued_inputs.pop_front() else {
            return Ok(());
        };
        self.input = next;
        self.input_cursor = self.input.len();
        self.submit().await
    }

    fn start_search(&mut self, query: String) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.clear_search();
            return;
        }

        let lowered = query.to_ascii_lowercase();
        let matches: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(idx, msg)| {
                let content = if self.transcript_mode {
                    match msg.reasoning.as_deref() {
                        Some(reasoning) if !reasoning.is_empty() => {
                            format!("{}\n{}", msg.content, reasoning)
                        }
                        _ => msg.content.clone(),
                    }
                } else {
                    msg.content.clone()
                };
                content
                    .to_ascii_lowercase()
                    .contains(&lowered)
                    .then_some(idx)
            })
            .collect();

        self.search = SearchState {
            query: query.clone(),
            target_message: matches.first().copied(),
            index: (!matches.is_empty()).then_some(0),
            matches,
        };

        if self.search.matches.is_empty() {
            self.scroll_offset = 0;
            self.push_system_message(&format!("No matches for '{query}'."));
        } else {
            self.scroll_to_search_target();
            if let Some((current, total)) = self.search.position() {
                self.push_system_message(&format!("Search: '{query}' ({current}/{total})."));
            }
        }
    }

    fn jump_search_next(&mut self) {
        if self.search.matches.is_empty() {
            if self.search.is_active() {
                self.push_system_message(&format!("No matches for '{}'.", self.search.query));
            }
            return;
        }
        let next = self
            .search
            .index
            .map(|idx| (idx + 1) % self.search.matches.len())
            .unwrap_or(0);
        self.search.index = Some(next);
        self.search.target_message = self.search.matches.get(next).copied();
        self.scroll_to_search_target();
    }

    fn jump_search_prev(&mut self) {
        if self.search.matches.is_empty() {
            if self.search.is_active() {
                self.push_system_message(&format!("No matches for '{}'.", self.search.query));
            }
            return;
        }
        let prev = self
            .search
            .index
            .map(|idx| {
                if idx == 0 {
                    self.search.matches.len() - 1
                } else {
                    idx - 1
                }
            })
            .unwrap_or(0);
        self.search.index = Some(prev);
        self.search.target_message = self.search.matches.get(prev).copied();
        self.scroll_to_search_target();
    }

    fn scroll_to_search_target(&mut self) {
        let Some(target) = self.search.target_message else {
            return;
        };
        let rows_after_target: usize = self
            .messages
            .iter()
            .skip(target.saturating_add(1))
            .map(estimated_message_rows)
            .sum();
        self.scroll_offset = rows_after_target.saturating_add(2);
    }

    fn refresh_search_matches(&mut self) {
        if !self.search.is_active() {
            return;
        }
        let query = self.search.query.clone();
        let previous_target = self.search.target_message;
        let lowered = query.to_ascii_lowercase();
        let matches: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(idx, msg)| {
                message_search_content(msg, self.transcript_mode)
                    .to_ascii_lowercase()
                    .contains(&lowered)
                    .then_some(idx)
            })
            .collect();

        let index = previous_target
            .and_then(|target| matches.iter().position(|m| *m == target))
            .or_else(|| (!matches.is_empty()).then_some(0));
        self.search.matches = matches;
        self.search.index = index;
        self.search.target_message = index.and_then(|idx| self.search.matches.get(idx).copied());
    }

    fn clear_search(&mut self) {
        self.search = SearchState::default();
    }

    pub(super) fn mode_label(&self) -> &'static str {
        if self.input.starts_with('!') {
            "bash"
        } else {
            "prompt"
        }
    }

    pub(super) fn spinner(&self) -> &'static str {
        const FRAMES: &[&str] = &["-", "\\", "|", "/"];
        FRAMES[self.spinner_index % FRAMES.len()]
    }

    /// Apply a streaming token to the last assistant message.
    pub(super) fn apply_stream_token(
        &mut self,
        turn_id: u64,
        content: String,
        reasoning: Option<String>,
        done: bool,
        error: Option<String>,
    ) {
        if !self.accepts_turn(turn_id) {
            return;
        }

        let has_payload = !content.is_empty() || reasoning.is_some() || error.is_some();
        let assistant_index = match self.active_assistant_index {
            Some(idx) if idx < self.messages.len() => idx,
            _ => {
                if has_payload {
                    self.messages
                        .push(ChatMessage::new(Role::Assistant, String::new()));
                    let idx = self.messages.len().saturating_sub(1);
                    self.active_assistant_index = Some(idx);
                    idx
                } else {
                    return;
                }
            }
        };

        if self
            .messages
            .get(assistant_index)
            .is_none_or(|msg| msg.role != Role::Assistant)
        {
            return;
        }

        if !content.is_empty() {
            if let Some(last) = self.messages.get_mut(assistant_index) {
                last.content.push_str(&content);
            }
            self.token_count +=
                lattice_core::tokens::TokenEstimator::estimate_text(&content) as usize;
        }
        if let Some(r) = reasoning {
            self.token_count += lattice_core::tokens::TokenEstimator::estimate_text(&r) as usize;
            if self.reasoning_started.is_none() {
                self.reasoning_started = Some(std::time::Instant::now());
            }
            if let Some(last) = self.messages.get_mut(assistant_index) {
                match last.reasoning {
                    Some(ref mut existing) => existing.push_str(&r),
                    None => last.reasoning = Some(r),
                }
            }
        } else if self.reasoning_started.is_some() && self.reasoning_duration.is_none() {
            // Reasoning phase ended — record duration
            if let Some(start) = self.reasoning_started {
                self.reasoning_duration = Some(start.elapsed());
            }
        }
        if let Some(ref msg) = error {
            let error_text = format!("Error: {}", msg);
            if let Some(last) = self.messages.get_mut(assistant_index) {
                if last.content.is_empty() {
                    last.content = error_text.clone();
                } else if !last.content.contains(&error_text) {
                    if !last.content.ends_with('\n') && !last.content.is_empty() {
                        last.content.push('\n');
                    }
                    last.content.push_str(&error_text);
                }
            }
        }

        if let Some(msg) = error {
            self.status = AppStatus::Error(msg);
            self.active_turn_id = None;
            self.active_assistant_index = None;
            self.pending_user = None;
            self.stream_started = None;
            self.reasoning_started = None;
            self.reasoning_duration = None;
        } else if done {
            self.status = AppStatus::Ready;
            // Recalculate total token count from all messages for accuracy
            self.recount_tokens();
            self.refresh_search_matches();
            // Cache is populated lazily on next render at message_lines()
            self.save_pending_turn();
            self.active_assistant_index = None;
            self.active_turn_id = None;
            self.pending_user = None;
            self.stream_started = None;
            self.reasoning_started = None;
            self.reasoning_duration = None;
        }
    }

    pub(super) fn apply_tool_output(
        &mut self,
        turn_id: u64,
        call_id: String,
        name: String,
        arguments: String,
        result: Option<String>,
    ) {
        if !self.accepts_turn(turn_id) {
            return;
        }

        if let Some(idx) = self.active_assistant_index {
            if matches!(
                self.messages.get(idx),
                Some(last)
                    if last.role == Role::Assistant
                        && last.content.is_empty()
                        && last.reasoning.as_deref().unwrap_or("").is_empty()
            ) {
                self.messages.remove(idx);
                self.active_assistant_index = None;
            }
        }

        if let Some(result) = result {
            if let Some(msg) = self.messages.iter_mut().find(|msg| {
                msg.tool
                    .as_ref()
                    .is_some_and(|tool| tool.call_id == call_id)
            }) {
                if let Some(tool) = msg.tool.as_mut() {
                    tool.result = Some(result);
                    tool.status = tool_result_status(tool.result.as_deref());
                    tool.finished_at = Some(std::time::Instant::now());
                    msg.content = tool_display_content(tool);
                    msg.collapsed = msg.content.lines().count() > 6;
                    msg.invalidate_cache();
                }
            } else {
                let mut msg = ChatMessage::tool_call(call_id, name, arguments);
                if let Some(tool) = msg.tool.as_mut() {
                    tool.result = Some(result);
                    tool.status = tool_result_status(tool.result.as_deref());
                    tool.finished_at = Some(std::time::Instant::now());
                    msg.content = tool_display_content(tool);
                    msg.collapsed = msg.content.lines().count() > 6;
                }
                self.messages.push(msg);
            }
        } else if !self.messages.iter().any(|msg| {
            msg.tool
                .as_ref()
                .is_some_and(|tool| tool.call_id == call_id)
        }) {
            let mut msg = ChatMessage::tool_call(call_id, name, arguments);
            if let Some(tool) = msg.tool.as_ref() {
                msg.content = tool_display_content(tool);
            }
            self.messages.push(msg);
        }
        self.scroll_offset = 0;
        self.refresh_search_matches();
    }

    pub(super) fn toggle_tool_expand(&mut self) {
        if let Some(last) = self.messages.last_mut() {
            if last.role == Role::Tool {
                last.collapsed = !last.collapsed;
            }
        }
    }

    fn recount_tokens(&mut self) {
        use lattice_core::tokens::TokenEstimator;
        let mut total = 0usize;
        for msg in &self.messages {
            total += TokenEstimator::estimate_text(&msg.content) as usize;
            if let Some(ref r) = msg.reasoning {
                total += TokenEstimator::estimate_text(r) as usize;
            }
        }
        self.token_count = total;
    }

    pub(super) fn copy_last_assistant(&mut self) {
        let text = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && !m.content.is_empty())
            .map(|m| m.content.clone())
            .unwrap_or_default();
        if !text.is_empty() {
            self.copy_to_clipboard(&text);
            self.push_system_message("Copied to clipboard");
        }
    }

    fn save_pending_turn(&mut self) {
        if !self.save_sessions {
            self.pending_user = None;
            return;
        }

        let Some(user) = self.pending_user.take() else {
            return;
        };
        let assistant = self
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == Role::Assistant && !msg.content.trim().is_empty())
            .map(|msg| msg.content.clone())
            .unwrap_or_default();
        if assistant.is_empty() {
            return;
        }

        let session = crate::session::finalize_session_turn(
            self.session.take(),
            self.current_model.clone(),
            self.current_provider.clone(),
            user,
            assistant,
        );
        let manager = crate::session::SessionManager::new();
        if let Err(err) = manager.save(&session) {
            self.messages.push(ChatMessage::new(
                Role::System,
                format!("Failed to save session: {err}"),
            ));
        }
        self.session = Some(session);
    }
}

fn role_from_session(role: &str) -> Role {
    match role.to_ascii_lowercase().as_str() {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn compact_json(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => value.to_string(),
        Err(_) => trimmed.to_string(),
    }
}

fn trim_tool_result(result: &str) -> String {
    const LIMIT: usize = 4000;
    if result.len() <= LIMIT {
        return result.to_string();
    }

    let mut end = LIMIT;
    while !result.is_char_boundary(end) {
        end -= 1;
    }
    let mut trimmed = result[..end].to_string();
    trimmed.push_str("\n...[tool output truncated]");
    trimmed
}

fn tool_result_status(result: Option<&str>) -> ToolStatus {
    let result = result.unwrap_or_default().trim_start().to_ascii_lowercase();
    if result.starts_with("error")
        || result.starts_with("sandbox violation")
        || result.contains("permission denied")
        || result.contains("timed out")
    {
        ToolStatus::Error
    } else {
        ToolStatus::Done
    }
}

fn tool_display_content(tool: &ToolDisplay) -> String {
    let mut content = format!("{} {}", tool.name, compact_json(&tool.arguments));
    let elapsed = tool
        .finished_at
        .unwrap_or_else(std::time::Instant::now)
        .saturating_duration_since(tool.started_at);
    let status = match tool.status {
        ToolStatus::Running => "running".to_string(),
        ToolStatus::Done => format!("done in {}", format_short_duration(elapsed)),
        ToolStatus::Error => format!("error after {}", format_short_duration(elapsed)),
    };
    content.push_str(&format!("\nstatus: {status}"));
    if let Some(result) = tool.result.as_deref() {
        content.push('\n');
        content.push_str(&trim_tool_result(result));
    }
    content
}

fn format_short_duration(duration: std::time::Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        let secs = duration.as_secs_f32();
        format!("{secs:.1}s")
    }
}

fn message_search_content(msg: &ChatMessage, transcript_mode: bool) -> String {
    if transcript_mode {
        match msg.reasoning.as_deref() {
            Some(reasoning) if !reasoning.is_empty() => {
                format!("{}\n{}", msg.content, reasoning)
            }
            _ => msg.content.clone(),
        }
    } else {
        msg.content.clone()
    }
}

fn permission_modes() -> Vec<String> {
    ["project", "strict", "permissive", "off"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn plugin_context(name: &str, prompt: &str, workdir: &std::path::Path) -> serde_json::Value {
    match canonical_plugin_name(name).as_str() {
        "CodeReview" | "code-review" => serde_json::json!({
            "input": prompt,
            "file_path": "",
            "context_rules": []
        }),
        "Refactor" | "refactor" => serde_json::json!({
            "code": prompt,
            "review": null,
            "instructions": prompt
        }),
        "TestGen" | "test-gen" => serde_json::json!({
            "code": prompt,
            "language": "",
            "focus_areas": []
        }),
        "SecurityAudit" | "security-audit" => serde_json::json!({
            "code": prompt,
            "dependencies": [],
            "threat_model": ""
        }),
        "DocGen" | "doc-gen" => serde_json::json!({
            "code": prompt,
            "doc_type": "technical",
            "audience": "developers"
        }),
        "PlanGen" | "plan-gen" => serde_json::json!({
            "spec": prompt,
            "project_path": workdir.display().to_string(),
            "context_rules": []
        }),
        "DeepResearch" | "deep-research" => serde_json::json!({
            "query": prompt,
            "sources": [],
            "depth": "standard"
        }),
        "ImageGen" | "image-gen" => serde_json::json!({
            "prompt": prompt,
            "style": "",
            "dimensions": ""
        }),
        "KnowledgeBase" | "knowledge-base" => serde_json::json!({
            "query": prompt,
            "kb_sources": []
        }),
        "PptxGen" | "pptx-gen" => serde_json::json!({
            "topic": prompt,
            "outline": [],
            "template": ""
        }),
        "Verification" | "verification" => serde_json::json!({
            "changes": [],
            "plan_task_id": 0,
            "verification_steps": [prompt]
        }),
        _ => serde_json::json!({ "input": prompt, "request": prompt }),
    }
}

fn canonical_plugin_name(name: &str) -> String {
    name.trim().to_string()
}

fn output_contract_delta(schema: Option<serde_json::Value>) -> Option<SystemPromptDelta> {
    let schema = schema?;
    let output_schema =
        serde_json::to_string_pretty(&schema).unwrap_or_else(|_| schema.to_string());
    Some(SystemPromptDelta::contract(
        "Output contract:\nReturn only valid JSON matching this schema:\n{{output_schema}}",
        HashMap::from([("output_schema".to_string(), output_schema)]),
    ))
}

fn plugin_augmented_system_prompt(workdir: &std::path::Path, plugin_prompt: &str) -> String {
    format!(
        "{}\n\nPlugin mode:\n{}",
        coding_agent::coding_system_prompt(workdir),
        plugin_prompt
    )
}

fn estimated_message_rows(msg: &ChatMessage) -> usize {
    let content_rows = msg.content.lines().count().max(1);
    let reasoning_rows = msg.reasoning.as_ref().map_or(0, |r| r.lines().count());
    content_rows
        .saturating_add(reasoning_rows)
        .saturating_add(1)
}

fn summarize_prompt(text: &str, max_chars: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for (idx, ch) in one_line.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session() -> crate::session::Session {
        crate::session::Session {
            id: "session-1".into(),
            model: "old-model".into(),
            provider: "old-provider".into(),
            title: None,
            messages: vec![
                crate::session::SessionMessage {
                    role: "user".into(),
                    content: "old question".into(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                crate::session::SessionMessage {
                    role: "assistant".into(),
                    content: "old answer".into(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            created_at: "2026-05-05T00:00:00+08:00".into(),
            updated_at: "2026-05-05T00:00:00+08:00".into(),
        }
    }

    #[test]
    fn stream_error_sets_status_and_appends_visible_error() {
        let mut app = App::new();
        app.status = AppStatus::Streaming;
        app.active_turn_id = Some(7);
        app.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, "partial response".into()));

        app.apply_stream_token(
            7,
            String::new(),
            None,
            false,
            Some("provider failed".into()),
        );

        assert_eq!(app.status, AppStatus::Error("provider failed".into()));
        assert_eq!(
            app.messages.last().unwrap().content,
            "partial response\nError: provider failed"
        );
    }

    #[test]
    fn stale_turn_tokens_are_ignored() {
        let mut app = App::new();
        app.active_turn_id = Some(9);
        app.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));

        app.apply_stream_token(8, "ignored".into(), None, false, None);

        assert!(app.messages[0].content.is_empty());
    }

    #[test]
    fn slash_model_switch_updates_state() {
        let mut app = App::new();
        app.current_provider = "mock".into();
        app.previous_model = "old".into();

        let handled = app.handle_slash_command("/model llama-3.1").then_some(());

        assert!(handled.is_some());
        assert_eq!(app.current_model, "llama-3.1");
        assert!(app.current_provider.is_empty());
        assert_eq!(app.status, AppStatus::Ready);
        assert!(app.agent.is_none());
        assert_eq!(
            app.messages.last().unwrap().content,
            "Model switched to llama-3.1."
        );
    }

    #[test]
    fn slash_suggestions_filter_prefixes() {
        let mut app = App::new();
        app.input = "/st".into();

        let suggestions = app.slash_suggestions();

        assert!(suggestions.iter().any(|s| s.command == "/status"));
        assert!(!suggestions.iter().any(|s| s.command == "/clear"));
    }

    #[test]
    fn accept_selected_suggestion_completes_command_name() {
        let mut app = App::new();
        app.input = "/f".into();
        app.input_cursor = app.input.len();

        app.accept_selected_suggestion();

        assert_eq!(app.input, "/find ");
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[test]
    fn queue_input_records_prompt_for_later() {
        let mut app = App::new();

        app.queue_input("next task".into());

        assert_eq!(app.queued_inputs.len(), 1);
        assert_eq!(app.queued_inputs.front().unwrap(), "next task");
        assert_eq!(
            app.messages.last().unwrap().content,
            "Queued prompt #1. It will run after the current response."
        );
    }

    #[tokio::test]
    async fn submit_next_queued_waits_until_ready() {
        let mut app = App::new();
        app.status = AppStatus::Streaming;
        app.queued_inputs.push_back("/status".into());

        app.submit_next_queued().await.unwrap();

        assert_eq!(app.queued_inputs.len(), 1);
        assert!(app.messages.is_empty());
    }

    #[tokio::test]
    async fn submit_next_queued_runs_slash_command() {
        let mut app = App::new();
        app.queued_inputs.push_back("/status".into());

        app.submit_next_queued().await.unwrap();

        assert!(app.queued_inputs.is_empty());
        assert!(app.messages.last().unwrap().content.contains("model="));
    }

    #[test]
    fn slash_queue_clear_drops_pending_prompts() {
        let mut app = App::new();
        app.queued_inputs.push_back("one".into());
        app.queued_inputs.push_back("two".into());

        assert!(app.handle_slash_command("/queue clear"));

        assert!(app.queued_inputs.is_empty());
        assert_eq!(
            app.messages.last().unwrap().content,
            "Cleared 2 queued prompt(s)."
        );
    }

    #[test]
    fn slash_permissions_switches_runtime_security() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/permissions strict"));

        assert_eq!(app.security.mode_label, "strict");
        assert_eq!(app.security_config.sandbox_mode, "strict");
        assert!(app.agent.is_none());
        assert!(app
            .messages
            .last()
            .unwrap()
            .content
            .contains("Permissions switched"));
    }

    #[test]
    fn slash_permissions_without_mode_opens_menu() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/permissions"));

        let menu = app.menu.expect("permissions should open menu");
        assert!(matches!(menu.kind, MenuKind::Permissions));
        assert!(menu.options.iter().any(|mode| mode == "project"));
    }

    #[test]
    fn slash_plugins_lists_registry() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/plugins"));

        let msg = app.messages.last().unwrap().content.as_str();
        assert!(msg.contains("plugin(s) loaded"));
        assert!(msg.contains("/plugin <name> <prompt>"));
    }

    #[test]
    fn slash_plugin_prepares_plugin_submission() {
        let mut app = App::new();

        assert!(app.handle_slash_command("/plugin CodeReview inspect this diff"));

        let prepared = app
            .prepared_submission
            .as_ref()
            .expect("plugin command should prepare a submission");
        assert!(prepared.display.starts_with("@CodeReview "));
        assert!(prepared.prompt.contains("CODE TO REVIEW"));
        assert_eq!(prepared.plugin.as_ref().unwrap().name, "CodeReview");
    }

    #[test]
    fn slash_find_tracks_matches_and_target() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::User, "alpha".into()));
        app.messages
            .push(ChatMessage::new(Role::Assistant, "beta alpha".into()));

        assert!(app.handle_slash_command("/find alpha"));

        assert_eq!(app.search.query, "alpha");
        assert_eq!(app.search.matches, vec![0, 1]);
        assert_eq!(app.search.position(), Some((1, 2)));
        assert_eq!(app.search.target_message, Some(0));
    }

    #[test]
    fn search_navigation_wraps() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::User, "alpha".into()));
        app.messages
            .push(ChatMessage::new(Role::Assistant, "beta alpha".into()));
        app.start_search("alpha".into());

        app.jump_search_next();
        assert_eq!(app.search.position(), Some((2, 2)));
        assert_eq!(app.search.target_message, Some(1));

        app.jump_search_next();
        assert_eq!(app.search.position(), Some((1, 2)));
        assert_eq!(app.search.target_message, Some(0));
    }

    #[test]
    fn tool_output_adds_visible_tool_message() {
        let mut app = App::new();
        app.active_turn_id = Some(1);

        app.apply_tool_output(
            1,
            "call-1".into(),
            "list_directory".into(),
            r#"{"path":"."}"#.into(),
            Some("FILE Cargo.toml".into()),
        );

        let msg = app.messages.last().unwrap();
        assert_eq!(msg.role, Role::Tool);
        assert!(msg.content.contains("list_directory"));
        assert!(msg.content.contains("FILE Cargo.toml"));
        assert_eq!(msg.tool.as_ref().unwrap().status, ToolStatus::Done);
    }

    #[test]
    fn tool_output_updates_existing_call() {
        let mut app = App::new();
        app.active_turn_id = Some(1);

        app.apply_tool_output(
            1,
            "call-1".into(),
            "read_file".into(),
            r#"{"path":"Cargo.toml"}"#.into(),
            None,
        );
        app.apply_tool_output(
            1,
            "call-1".into(),
            "read_file".into(),
            r#"{"path":"Cargo.toml"}"#.into(),
            Some("package data".into()),
        );

        assert_eq!(app.messages.len(), 1);
        let msg = app.messages.last().unwrap();
        assert!(msg.content.contains("package data"));
        assert_eq!(msg.tool.as_ref().unwrap().status, ToolStatus::Done);
    }

    #[test]
    fn toggle_recent_reasoning_flips_latest_assistant_block() {
        let mut app = App::new();
        let mut msg = ChatMessage::new(Role::Assistant, "answer".into());
        msg.reasoning = Some("step one".into());
        app.messages.push(msg);

        app.toggle_recent_reasoning();

        assert!(!app.messages.last().unwrap().reasoning_collapsed);
    }

    #[test]
    fn stream_after_tool_output_creates_visible_assistant_message() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));
        app.active_turn_id = Some(1);
        app.active_assistant_index = Some(0);
        app.apply_tool_output(
            1,
            "call-1".into(),
            "read_file".into(),
            r#"{"path":"Cargo.toml"}"#.into(),
            None,
        );

        app.apply_stream_token(1, "final answer".into(), None, false, None);

        let msg = app.messages.last().unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content, "final answer");
    }

    #[test]
    fn selection_copy_uses_viewport_origin() {
        let mut app = App::new();
        app.set_clipboard_sink(Arc::new(|_| {}));
        app.visible_rows
            .replace(vec!["line two".into(), "line three".into()]);
        app.visible_rows_origin.set(10);
        let sel = TextSelection {
            start_row: 10,
            start_col: 0,
            end_row: 11,
            end_col: 0,
            active: false,
        };

        app.copy_selection(&sel);
    }

    #[test]
    fn loading_session_does_not_override_explicit_model() {
        let mut app = App::new();
        app.current_model = "explicit-model".into();
        app.previous_model = "old-agent-model".into();

        app.load_session(test_session());

        assert_eq!(app.current_model, "explicit-model");
        assert_eq!(app.current_provider, "old-provider");
        assert_eq!(app.messages.len(), 2);
        assert!(app.previous_model.is_empty());
        assert!(app.agent.is_none());
    }
}
