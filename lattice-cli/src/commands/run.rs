use anyhow::Result;
use colored::Colorize;
use lattice::agent::prompt;
use lattice::agent::{Agent, LoopEvent};
use lattice::core::types::Role;
use lattice::runtime::AgentSpec;
use std::collections::HashMap;
use std::io::Write;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    prompt: String,
    model: String,
    provider_override: Option<&str>,
    verbose: bool,
    json: bool,
    creds: &crate::credentials::CredentialStore,
    save: bool,
    previous_session: Option<crate::session::Session>,
    dry_run: bool,
    system_prompt: Option<&str>,
    max_turns: u32,
    stream_output: bool,
    security: crate::security::RuntimeSecurity,
) -> Result<()> {
    // Dry-run: compile prompt through engine and print, no LLM call, no credential needed
    if dry_run {
        return dry_run_prompt(&prompt, &model, system_prompt, previous_session.as_ref()).await;
    }

    if verbose {
        eprintln!("{}", format!("resolve: {} ...", model).dimmed());
    }

    let runtime = crate::runtime::model_runtime(creds.to_hashmap());
    let messages = previous_session
        .as_ref()
        .map(crate::session::messages_for_agent)
        .unwrap_or_default();
    let mut spec = AgentSpec::new(model.clone())
        .provider_override(provider_override.map(str::to_string))
        .messages(messages)
        .project_root(".")
        .security(Some(security.clone()));
    if let Some(system_prompt) = system_prompt {
        spec = spec.system_prompt(system_prompt);
    }
    let built = runtime.build_agent(spec)?;
    let mut agent = built.agent;
    let resolved = built.model;

    if verbose {
        eprintln!(
            "{}",
            format!("resolved: {}@{}", resolved.canonical_id, resolved.provider).cyan()
        );
    }

    let resolved_model = resolved.canonical_id.clone();
    let resolved_provider = resolved.provider.clone();

    if verbose {
        eprintln!("{}", "streaming...".dimmed());
    }

    let events = if json || !stream_output {
        let events = agent.run(&prompt, max_turns).await;
        display_events(&agent, events.clone(), verbose, json)?;
        events
    } else {
        let mut renderer = LoopEventRenderer::new(verbose, false);
        let mut render_error = None;
        let events = agent
            .run_streaming(&prompt, max_turns, |event| {
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
    crate::security::reap_audit(&security).await;
    let content = extract_content(&events);

    if save {
        let session = crate::session::finalize_session_turn(
            previous_session,
            resolved_model,
            resolved_provider,
            prompt.clone(),
            content,
        );
        let mgr = crate::session::SessionManager::new();
        mgr.save(&session)?;
        if verbose {
            eprintln!("{}", format!("session saved: {}", session.id).dimmed());
        }
    }

    Ok(())
}

/// Compile prompt through the engine and print the result without calling LLM.
async fn dry_run_prompt(
    prompt: &str,
    model_name: &str,
    system_prompt: Option<&str>,
    previous_session: Option<&crate::session::Session>,
) -> Result<()> {
    let history = previous_session
        .map(crate::session::messages_for_agent)
        .unwrap_or_default();
    let runtime = crate::runtime::model_runtime(HashMap::new());
    let report = runtime
        .dry_run_prompt(model_name, system_prompt, prompt, history)
        .await?;

    println!("{}", "═══ Compiled Prompt (dry-run) ═══".green());
    println!("{}", "─ Input Sections ─".dimmed());
    for (s, b) in report.sections.iter().zip(report.budgets.iter()) {
        let label = format!("{:?}", s.layer);
        let budget = match b {
            prompt::TokenBudget::Fixed(n) => format!("Fixed({})", n),
            prompt::TokenBudget::Ratio(r) => format!("Ratio({})", r),
            prompt::TokenBudget::Dynamic => "Dynamic".into(),
        };
        println!(
            "  {}  tokens={:<6} priority={:<3} budget={}",
            label.cyan(),
            s.tokens.to_string().yellow(),
            s.priority.to_string().dimmed(),
            budget.dimmed(),
        );
    }
    println!();
    println!("{}", "─ Rendered Messages ─".dimmed());
    for msg in &report.messages {
        let role_label = match msg.role {
            Role::System => "System".red(),
            Role::User => "User".cyan(),
            Role::Assistant => "Assistant".green(),
            Role::Tool => "Tool".yellow(),
        };
        println!("  [{}] {}", role_label, msg.content);
    }
    println!();
    println!("{}", "─ Stats ─".dimmed());
    println!(
        "  total_tokens={:<6} context_length={}",
        report.rendered.total_tokens.to_string().yellow(),
        report.resolved.context_length.to_string().dimmed(),
    );

    Ok(())
}

/// Run a pipeline: load agent registry, create registries, and execute.
#[allow(clippy::too_many_arguments)]
pub async fn run_pipeline(
    prompt: &str,
    start_agent: &str,
    agents_dir: Option<&str>,
    plugins_dir: Option<&str>,
    _tools_dir: Option<&str>,
    verbose: bool,
    json: bool,
    creds: HashMap<String, String>,
) -> Result<()> {
    let (runtime, dir) = crate::runtime::pipeline_runtime(agents_dir, plugins_dir, creds)?;

    if verbose {
        eprintln!(
            "{}",
            format!("loading agents from {} ...", dir.display()).dimmed()
        );
    }

    let registry = runtime.agent_registry();

    if registry.list().is_empty() {
        anyhow::bail!("No agent profiles found in '{}'", dir.display());
    }

    if verbose {
        eprintln!(
            "{}",
            format!("loaded {} agents", registry.list().len()).cyan()
        );
        for profile in registry.list() {
            eprintln!("  - {} ({})", profile.agent.name, profile.agent.model);
        }
    }

    if verbose {
        for meta in runtime.plugin_registry().list() {
            eprintln!("  plugin: {} — {}", meta.name, meta.description);
        }
    }

    // Validate pipeline chain before running
    let report = runtime.dry_run_pipeline(start_agent);
    if !report.valid {
        eprintln!("{}", "Pipeline validation failed:".red());
        for issue in &report.issues {
            eprintln!("  - {}", issue.red());
        }
        anyhow::bail!(
            "Pipeline '{}' is invalid — fix agent profiles before running",
            start_agent
        );
    }

    if verbose {
        eprintln!(
            "{}",
            format!(
                "pipeline chain: {} → end",
                report.agents_in_chain.join(" → ")
            )
            .cyan()
        );
    }

    let result = runtime.run_pipeline(start_agent, prompt).await;

    if json {
        let out = serde_json::json!({
            "completed": result.completed,
            "duration_ms": result.duration_ms,
            "agents": result.results.iter().map(|r| serde_json::json!({
                "agent": r.agent_name,
                "output": r.output,
                "next": r.next,
                "duration_ms": r.duration_ms,
            })).collect::<Vec<_>>(),
            "errors": result.errors.iter().map(|e| serde_json::json!({
                "agent": e.agent_name,
                "message": e.message,
                "skippable": e.skippable,
            })).collect::<Vec<_>>(),
            "skipped": result.skipped,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "{}",
            format!("Pipeline completed in {}ms", result.duration_ms).green()
        );
        if result.completed {
            println!("{}", "Status: completed".green());
        } else {
            println!("{}", "Status: incomplete".yellow());
        }

        for r in &result.results {
            println!();
            println!(
                "{}",
                format!("── {} ({}ms) ──", r.agent_name, r.duration_ms).cyan()
            );
            let preview: String = r.output.to_string().chars().take(500).collect();
            println!("{}", preview);
            if let Some(ref next) = r.next {
                eprintln!("{}", format!("  → next: {}", next).dimmed());
            }
        }

        for e in &result.errors {
            println!();
            println!("{}", format!("── {} (ERROR) ──", e.agent_name).red());
            println!("  {}", e.message.red());
            if e.skippable {
                println!("  {}", "(skippable)".dimmed());
            }
        }

        if !result.skipped.is_empty() {
            println!();
            println!(
                "{}",
                format!("Skipped: {}", result.skipped.join(", ")).yellow()
            );
        }
    }

    Ok(())
}

fn flush_reasoning(buf: &str, verbose: bool, json: bool) {
    if verbose && !json && !buf.is_empty() {
        eprintln!("{} {}", "reasoning:".dimmed(), buf.trim());
    }
}

struct LoopEventRenderer {
    verbose: bool,
    json: bool,
    content_buf: String,
    reasoning_buf: String,
}

impl LoopEventRenderer {
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
                    print!("{}", text);
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
                    eprintln!("\n{} {} tool call(s)...", "executing".dimmed(), calls.len());
                }
            }
            LoopEvent::ToolResult { call, result } => {
                if self.verbose && !self.json {
                    eprintln!(
                        "\n{} {} -> {}",
                        "tool".dimmed(),
                        call.function.name,
                        result.chars().take(200).collect::<String>()
                    );
                }
            }
            LoopEvent::Done { usage } => {
                self.flush_reasoning();
                if self.verbose && !self.json {
                    if let Some(u) = usage {
                        eprintln!(
                            "\n{}: {} tok (prompt: {}, completion: {})",
                            "usage".dimmed(),
                            u.total_tokens,
                            u.prompt_tokens,
                            u.completion_tokens,
                        );
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
        flush_reasoning(&self.reasoning_buf, self.verbose, self.json);
        self.reasoning_buf.clear();
    }
}

fn display_events(agent: &Agent, events: Vec<LoopEvent>, verbose: bool, json: bool) -> Result<()> {
    let mut renderer = LoopEventRenderer::new(verbose, json);
    for event in &events {
        renderer.render(event)?;
    }

    renderer.finish(Some(agent.token_usage()))
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
