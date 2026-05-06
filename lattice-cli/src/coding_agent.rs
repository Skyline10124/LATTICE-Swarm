use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use colored::Colorize;
use lattice_agent::{
    default_tool_definitions, Agent, DefaultToolExecutor, LoopEvent, ToolExecutor,
};
use lattice_core::router::ModelRouter;
use lattice_core::types::{FunctionCall, Message, ToolCall};

pub(crate) struct CodingAgentOptions {
    pub(crate) prompt: String,
    pub(crate) model: String,
    pub(crate) provider_override: Option<String>,
    pub(crate) workdir: PathBuf,
    pub(crate) max_turns: u32,
    pub(crate) stream_output: bool,
    pub(crate) verbose: bool,
    pub(crate) json: bool,
    pub(crate) credentials: HashMap<String, String>,
    pub(crate) save_session: bool,
    pub(crate) previous_session: Option<crate::session::Session>,
}

pub(crate) struct CodingAgentBuildOptions {
    pub(crate) model: String,
    pub(crate) provider_override: Option<String>,
    pub(crate) workdir: PathBuf,
    pub(crate) credentials: HashMap<String, String>,
    pub(crate) previous_session: Option<crate::session::Session>,
    pub(crate) prior_messages: Vec<Message>,
    pub(crate) thinking_effort: Option<String>,
}

pub(crate) struct BuiltCodingAgent {
    pub(crate) agent: Agent,
    pub(crate) model: String,
    pub(crate) provider: String,
    pub(crate) context_limit: u32,
}

pub(crate) async fn run(options: CodingAgentOptions) -> Result<()> {
    let built = build_coding_agent(CodingAgentBuildOptions {
        model: options.model.clone(),
        provider_override: options.provider_override.clone(),
        workdir: options.workdir.clone(),
        credentials: options.credentials.clone(),
        previous_session: options.previous_session.clone(),
        prior_messages: Vec::new(),
        thinking_effort: None,
    })?;
    let mut agent = built.agent;
    let resolved_model = built.model;
    let resolved_provider = built.provider;

    if options.verbose {
        eprintln!(
            "{}",
            format!(
                "coding agent: {}@{} in {}",
                resolved_model,
                resolved_provider,
                options.workdir.display()
            )
            .cyan()
        );
    }

    let events = if options.json || !options.stream_output {
        let events = agent.run(&options.prompt, options.max_turns).await;
        render_events(
            &events,
            Some(agent.token_usage()),
            options.verbose,
            options.json,
        )?;
        events
    } else {
        let mut renderer = CodingEventRenderer::new(options.verbose, false);
        let mut render_error = None;
        let events = agent
            .run_streaming(&options.prompt, options.max_turns, |event| {
                if render_error.is_none() {
                    if let Err(err) = renderer.render(&event) {
                        render_error = Some(err);
                    }
                }
            })
            .await;
        if let Some(err) = render_error {
            return Err(err);
        }
        renderer.finish(Some(agent.token_usage()))?;
        events
    };

    if options.save_session {
        let content = extract_content(&events);
        let session = crate::session::finalize_session_turn(
            options.previous_session,
            resolved_model,
            resolved_provider,
            options.prompt,
            content,
        );
        crate::session::SessionManager::new().save(&session)?;
        if options.verbose {
            eprintln!("{}", format!("session saved: {}", session.id).dimmed());
        }
    }

    Ok(())
}

pub(crate) fn build_coding_agent(options: CodingAgentBuildOptions) -> Result<BuiltCodingAgent> {
    let resolved = resolve_model(
        &options.model,
        options.provider_override.as_deref(),
        options.credentials,
    )?;
    let model = resolved.canonical_id.clone();
    let provider = resolved.provider.clone();
    let context_limit = if resolved.context_length > 0 {
        resolved.context_length
    } else {
        128000
    };
    let mut agent = Agent::new(resolved)
        .with_tools(default_tool_definitions())
        .with_tool_executor(Box::new(
            DefaultToolExecutor::new(options.workdir.to_string_lossy().as_ref())
                .map_err(anyhow::Error::msg)?,
        ))
        .with_thinking_effort(options.thinking_effort);
    agent.set_system_prompt(&coding_system_prompt(&options.workdir));
    if !options.prior_messages.is_empty() {
        agent.seed_messages(options.prior_messages);
    } else if let Some(session) = options.previous_session.as_ref() {
        agent.seed_messages(crate::session::messages_for_agent(session));
    }

    Ok(BuiltCodingAgent {
        agent,
        model,
        provider,
        context_limit,
    })
}

pub(crate) fn authenticated_models(credentials: &HashMap<String, String>) -> Vec<String> {
    let router = ModelRouter::with_credentials(credentials.clone());
    router.list_authenticated_models()
}

pub(crate) fn authenticated_providers(credentials: &HashMap<String, String>) -> Vec<String> {
    let mut providers = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for model in authenticated_models(credentials) {
        if let Some((provider, _)) = model.split_once('/') {
            if seen.insert(provider.to_string()) {
                providers.push(provider.to_string());
            }
        }
    }
    providers
}

pub(crate) async fn run_bash_tool(workdir: &Path, command: &str) -> Result<String> {
    let executor =
        DefaultToolExecutor::new(workdir.to_string_lossy().as_ref()).map_err(anyhow::Error::msg)?;
    let call = ToolCall {
        id: "tui-bash".into(),
        function: FunctionCall {
            name: "bash".into(),
            arguments: serde_json::json!({ "command": command }).to_string(),
        },
    };
    Ok(executor.execute(&call).await)
}

fn resolve_model(
    model: &str,
    provider_override: Option<&str>,
    credentials: HashMap<String, String>,
) -> Result<lattice_core::ResolvedModel> {
    let router = ModelRouter::with_credentials(credentials);
    Ok(router.resolve(model, provider_override)?)
}

fn coding_system_prompt(workdir: &Path) -> String {
    let context = repo_context(workdir);
    format!(
        "You are LATTICE Swarm, a repo-aware coding agent.\n\
         Work like a senior engineer inside the user's checkout.\n\
         Prioritize correct, tested, minimal changes.\n\
         Use read_file, grep, list_directory, patch, write_file and bash tools to inspect and edit files.\n\
         Prefer targeted patches over rewriting whole files.\n\
         Never claim a change is verified unless you ran the relevant command.\n\
         Surface blockers directly and keep output concise.\n\n\
         Working directory: {}\n\n\
         Repository context:\n{}",
        workdir.display(),
        context
    )
}

fn repo_context(workdir: &Path) -> String {
    let mut lines = Vec::new();
    if let Ok(entries) = std::fs::read_dir(workdir) {
        let mut names: Vec<_> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| !matches!(name.as_str(), ".git" | "target" | "node_modules"))
            .collect();
        names.sort();
        for name in names.into_iter().take(40) {
            lines.push(format!("- {name}"));
        }
    }
    if lines.is_empty() {
        "- no readable top-level entries".into()
    } else {
        lines.join("\n")
    }
}

struct CodingEventRenderer {
    verbose: bool,
    json: bool,
    content_buf: String,
    reasoning_buf: String,
}

impl CodingEventRenderer {
    fn new(verbose: bool, json: bool) -> Self {
        Self {
            verbose,
            json,
            content_buf: String::new(),
            reasoning_buf: String::new(),
        }
    }

    fn render(&mut self, event: &LoopEvent) -> Result<()> {
        match event {
            LoopEvent::Token { text } => {
                if !self.json {
                    print!("{text}");
                    std::io::stdout().flush()?;
                }
                self.content_buf.push_str(text);
            }
            LoopEvent::Reasoning { text } => {
                self.reasoning_buf.push_str(text);
            }
            LoopEvent::ToolCallRequired { calls } => {
                self.flush_reasoning();
                if self.verbose && !self.json {
                    eprintln!("\n{} {} tool call(s)", "tools".dimmed(), calls.len());
                }
            }
            LoopEvent::ToolResult { call, result } => {
                if self.verbose && !self.json {
                    let preview: String = result.chars().take(240).collect();
                    eprintln!(
                        "\n{} {} -> {}",
                        "tool".dimmed(),
                        call.function.name,
                        preview
                    );
                }
            }
            LoopEvent::Done { usage } => {
                self.flush_reasoning();
                if self.verbose && !self.json {
                    if let Some(usage) = usage {
                        eprintln!("\n{}: {} tokens", "usage".dimmed(), usage.total_tokens);
                    }
                }
            }
            LoopEvent::Error { message, .. } => {
                self.flush_reasoning();
                eprintln!("{} {}", "error:".red(), message);
            }
        }
        Ok(())
    }

    fn finish(&mut self, total_tokens: Option<u64>) -> Result<()> {
        self.flush_reasoning();
        if self.json {
            let out = serde_json::json!({
                "content": self.content_buf,
                "total_tokens": total_tokens.unwrap_or_default(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!();
        }
        std::io::stdout().flush()?;
        Ok(())
    }

    fn flush_reasoning(&mut self) {
        if self.verbose && !self.json && !self.reasoning_buf.is_empty() {
            eprintln!("{} {}", "reasoning:".dimmed(), self.reasoning_buf.trim());
        }
        self.reasoning_buf.clear();
    }
}

fn render_events(
    events: &[LoopEvent],
    total_tokens: Option<u64>,
    verbose: bool,
    json: bool,
) -> Result<()> {
    let mut renderer = CodingEventRenderer::new(verbose, json);
    for event in events {
        renderer.render(event)?;
    }
    renderer.finish(total_tokens)
}

fn extract_content(events: &[LoopEvent]) -> String {
    let mut buf = String::new();
    for event in events {
        if let LoopEvent::Token { text } = event {
            buf.push_str(text);
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_context_lists_top_level_entries() {
        let root = std::env::temp_dir().join(format!("lattice-code-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();

        let context = repo_context(&root);

        assert!(context.contains("Cargo.toml"));
        assert!(context.contains("src"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn system_prompt_contains_workdir_and_coding_role() {
        let prompt = coding_system_prompt(Path::new("/tmp/project"));

        assert!(prompt.contains("repo-aware coding agent"));
        assert!(prompt.contains("/tmp/project"));
    }
}
