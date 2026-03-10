// Slash command handling

use anyhow::Result;

use crate::metrics::MetricsLogger;
use crate::models::ThresholdValidator;
use crate::router::Router;

/// Output destination for commands
pub enum CommandOutput {
    Status(String),  // Short messages for status bar
    Message(String), // Long content for scrollback area
}

#[derive(Debug)]
pub enum Command {
    Help,
    Quit,
    Metrics,
    Memory,
    Debug,
    Training,
    Clear,
    Compact(Option<String>), // Clear with summary (optional instruction)
    PatternsList,
    PatternsRemove(String),
    PatternsClear,
    PatternsAdd,
    // Plan mode commands
    PlanModeToggle, // Toggle plan mode on/off (Shift+Tab or /plan without args)
    Plan(String),
    // Feedback commands for weighted LoRA training
    FeedbackCritical(Option<String>), // High-weight (10x) - critical strategy errors
    FeedbackMedium(Option<String>),   // Medium-weight (3x) - improvements
    FeedbackGood(Option<String>),     // Normal-weight (1x) - good examples
    // Local model testing
    Local { query: String }, // Query local model directly (bypass routing)
    // MCP plugin management
    McpList,                  // List connected MCP servers
    McpTools(Option<String>), // List tools from specific server (or all if None)
    McpRefresh,               // Refresh tools from all servers
    McpReload,                // Reconnect to all servers
    // Persona management (Phase 2)
    PersonaList,           // List available personas
    PersonaSelect(String), // Switch to a different persona
    PersonaShow,           // Show current persona and system prompt
    // Provider switching (/provider is canonical; /model and /teacher are silent aliases)
    ModelList,           // /provider list
    ModelSwitch(String), // /provider <name>  e.g. /provider grok
    ModelShow,           // /provider  (show current active provider)
    // Service discovery (Phase 3)
    Discover,  // Discover Finch daemons on local network
    Machines,  // List known peer machines (from LAN discovery)
    // License management
    LicenseStatus,           // /license or /license status
    LicenseActivate(String), // /license activate <key>
    LicenseRemove,           // /license remove
    // Daemon brain sessions
    Brain(String),       // /brain <task>  — spawn background research brain
    Brains,              // /brains        — list active brain sessions
    BrainCancel(String), // /brain cancel <name-or-id>
    // Execution graph
    Graph, // /graph — show causal trace of last query
    // Co-Forth VM stack ops
    Ask(String),                  // /ask <query>      — send directly to AI (bypass stack)
    StackPush(String),            // /push <text>      — push text onto the stack
    StackShow,                    // /stack            — show current stack contents
    StackPop,                     // /pop              — remove top item (undo last push)
    StackRun,                     // /run              — execute full stack as one query
    StackClear,                   // /stack clear      — drop all stack items
    StackProgram,                 // /program          — switch panel to Forth source view
    StackView,                    // /view             — switch panel to graph view (toggle)
    StackDemo,                    // /demo             — seed an example language to play with
    // Special Forth vocabulary ops
    StackChain(usize, usize),     // /chain W1 W2      — add edge W1 → W2
    StackForget(usize),           // /forget W1        — remove word and AI descendants
    StackDup(usize),              // /dup W1           — clone word as new entry
    StackSwap(usize, usize),      // /swap W1 W2       — swap labels of two words
    StackDescribe(String),        // /describe <word>  — show library entry for a word
    StackDefine(String, String),  // /define <word> <def> — add word to repo vocabulary
    StackOverride(String, String), // /override <word> <def> — machine-local override (~/.finch/library.toml)
    ForthEval(String),            // : word ... ; or /forth <expr> — eval in Forth interpreter
    ForthUndo,                    // /undefine — undo last Forth definition
    VmDump,                       // /vm — dump VM source to scrollback + clipboard
    LibraryUndefine(String),      // /undefine <word> — remove last user library entry for word
    LibraryRun(String),           // /run <word> — execute the Forth snippet for a library word
    Setup,                        // /setup — open the setup wizard (run 'finch setup' to reconfigure)
    Share,                        // /share — format session as a pasteable proof block
    BoxDiff,                      // /box-diff — compare all peers, offer to fix outliers
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();

        // Handle simple commands without arguments
        match trimmed {
            "/help" => return Some(Command::Help),
            "/quit" | "/exit" => return Some(Command::Quit),
            "/metrics" => return Some(Command::Metrics),
            "/memory" => return Some(Command::Memory),
            "/debug" => return Some(Command::Debug),
            "/training" => return Some(Command::Training),
            "/clear" | "/reset" => return Some(Command::Clear),
            "/compact" => return Some(Command::Compact(None)),
            // Feedback commands (simple form)
            "/critical" => return Some(Command::FeedbackCritical(None)),
            "/medium" => return Some(Command::FeedbackMedium(None)),
            "/good" => return Some(Command::FeedbackGood(None)),
            // Persona commands
            "/persona" | "/persona list" => return Some(Command::PersonaList),
            "/persona show" => return Some(Command::PersonaShow),
            // Provider commands (/provider canonical; /model and /teacher are aliases)
            "/provider" | "/provider show" | "/model" | "/model show" | "/teacher"
            | "/teacher show" => return Some(Command::ModelShow),
            "/provider list" | "/model list" | "/teacher list" => return Some(Command::ModelList),
            // Service discovery
            "/discover" => return Some(Command::Discover),
            "/machines" | "/peers" | "/nodes" => return Some(Command::Machines),
            // License management
            "/license" | "/license status" => return Some(Command::LicenseStatus),
            "/license remove" => return Some(Command::LicenseRemove),
            // Brain sessions
            "/brains" | "/brains list" => return Some(Command::Brains),
            "/graph" => return Some(Command::Graph),
            // Co-Forth VM
            "/vm" | "/vm dump" | "/vm copy" => return Some(Command::VmDump),
            "/stack" | "/stack list" | "/stack show" => return Some(Command::StackShow),
            "/stack clear" | "/stack reset" => return Some(Command::StackClear),
            "/pop" => return Some(Command::StackPop),
            "/run" | "/execute" | "/exec" => return Some(Command::StackRun),
            "/program" | "/words" | "/forth" => return Some(Command::StackProgram),
            "/view" | "/graph view" | "/poset" => return Some(Command::StackView),
            "/demo" | "/demo lang" => return Some(Command::StackDemo),
            "/setup" => return Some(Command::Setup),
            "/share" | "/prove" | "/proof" => return Some(Command::Share),
            "/box-diff" | "/cluster-diff" | "/cdiff" => return Some(Command::BoxDiff),
            _ => {}
        }

        // Handle /license activate <key>
        if let Some(rest) = trimmed.strip_prefix("/license activate ") {
            let key = rest.trim();
            if !key.is_empty() {
                return Some(Command::LicenseActivate(key.to_string()));
            }
        }

        // Handle /ask <query> — bypass stack, send directly to AI
        if let Some(rest) = trimmed.strip_prefix("/ask ") {
            let query = rest.trim();
            if !query.is_empty() {
                return Some(Command::Ask(query.to_string()));
            }
        }

        // Handle /push <text> — push onto stack
        if let Some(rest) = trimmed.strip_prefix("/push ") {
            let text = rest.trim();
            if !text.is_empty() {
                return Some(Command::StackPush(text.to_string()));
            }
        }

        // Co-Forth special ops: /chain W1 W2, /forget W1, /dup W1, /swap W1 W2
        if let Some(rest) = trimmed.strip_prefix("/chain ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() == 2 {
                let a = parse_word_id(parts[0]);
                let b = parse_word_id(parts[1]);
                if let (Some(a), Some(b)) = (a, b) {
                    return Some(Command::StackChain(a, b));
                }
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/forget ") {
            if let Some(id) = parse_word_id(rest.trim()) {
                return Some(Command::StackForget(id));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/dup ") {
            if let Some(id) = parse_word_id(rest.trim()) {
                return Some(Command::StackDup(id));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/swap ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() == 2 {
                let a = parse_word_id(parts[0]);
                let b = parse_word_id(parts[1]);
                if let (Some(a), Some(b)) = (a, b) {
                    return Some(Command::StackSwap(a, b));
                }
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/describe ") {
            let word = rest.trim();
            if !word.is_empty() {
                return Some(Command::StackDescribe(word.to_string()));
            }
        }
        // Forth definition typed directly: `: word ... ;`
        if trimmed.starts_with(": ") {
            return Some(Command::ForthEval(trimmed.to_string()));
        }
        // Forth / library undo
        if trimmed == "/undefine" {
            return Some(Command::ForthUndo);
        }
        if let Some(rest) = trimmed.strip_prefix("/undefine ") {
            let word = rest.trim().to_string();
            if !word.is_empty() {
                return Some(Command::LibraryUndefine(word));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/run ") {
            let word = rest.trim().to_string();
            if !word.is_empty() && !word.contains(' ') {
                return Some(Command::LibraryRun(word));
            }
        }
        // Forth eval via /forth
        if let Some(rest) = trimmed.strip_prefix("/forth ") {
            let expr = rest.trim();
            if !expr.is_empty() {
                return Some(Command::ForthEval(expr.to_string()));
            }
        }

        if let Some(rest) = trimmed.strip_prefix("/define ") {
            // /define <word> <definition…>   — definition may be empty (AI auto-define)
            // Word may be:
            //   • a single token (no spaces)  →  /define hello   A greeting
            //   • a quoted phrase              →  /define "machine learning"   AI technique
            //   • a Chinese word/phrase        →  /define 你好   A Chinese greeting
            let rest = rest.trim();
            if !rest.is_empty() {
                let (word, definition) = if rest.starts_with('"') {
                    // Quoted phrase: find closing '"'
                    if let Some(close) = rest[1..].find('"') {
                        let phrase = rest[1..=close].to_string();
                        let def = rest[close + 2..].trim().to_string();
                        (phrase, def)
                    } else {
                        // Unclosed quote — treat whole thing as the word
                        (rest.trim_matches('"').to_string(), String::new())
                    }
                } else if let Some(space) = rest.find(|c: char| c.is_whitespace()) {
                    (rest[..space].trim().to_string(), rest[space..].trim().to_string())
                } else {
                    (rest.to_string(), String::new())
                };
                if !word.is_empty() {
                    return Some(Command::StackDefine(word, definition));
                }
            }
        }

        // Handle /override — machine-local word override (writes to ~/.finch/library.toml)
        if let Some(rest) = trimmed.strip_prefix("/override ") {
            let rest = rest.trim();
            if !rest.is_empty() {
                let (word, definition) = if rest.starts_with('"') {
                    if let Some(close) = rest[1..].find('"') {
                        (rest[1..=close].to_string(), rest[close + 2..].trim().to_string())
                    } else {
                        (rest.trim_matches('"').to_string(), String::new())
                    }
                } else if let Some(space) = rest.find(|c: char| c.is_whitespace()) {
                    (rest[..space].trim().to_string(), rest[space..].trim().to_string())
                } else {
                    (rest.to_string(), String::new())
                };
                if !word.is_empty() {
                    return Some(Command::StackOverride(word, definition));
                }
            }
        }

        // Handle /brain cancel <name-or-id>
        if let Some(rest) = trimmed.strip_prefix("/brain cancel ") {
            let name = rest.trim();
            if !name.is_empty() {
                return Some(Command::BrainCancel(name.to_string()));
            }
        }

        // Handle /brain <task>  (must come after /brain cancel)
        if let Some(rest) = trimmed.strip_prefix("/brain ") {
            let task = rest.trim();
            if !task.is_empty() {
                return Some(Command::Brain(task.to_string()));
            }
        }

        // Handle /persona select <name>
        if let Some(rest) = trimmed.strip_prefix("/persona select ") {
            let persona_name = rest.trim();
            if !persona_name.is_empty() {
                return Some(Command::PersonaSelect(persona_name.to_string()));
            }
        }

        // Handle /provider <name> (canonical), /model <name>, /teacher <name> (aliases)
        if let Some(rest) = trimmed
            .strip_prefix("/provider ")
            .or_else(|| trimmed.strip_prefix("/model "))
            .or_else(|| trimmed.strip_prefix("/teacher "))
        {
            let teacher_name = rest.trim();
            // Filter out subcommands
            if teacher_name != "list" && teacher_name != "show" && !teacher_name.is_empty() {
                return Some(Command::ModelSwitch(teacher_name.to_string()));
            }
        }

        // Handle /plan command
        if trimmed == "/plan" {
            // Without arguments: toggle plan mode
            return Some(Command::PlanModeToggle);
        }

        if let Some(rest) = trimmed.strip_prefix("/plan ") {
            let task = rest.trim();
            if !task.is_empty() {
                return Some(Command::Plan(task.to_string()));
            } else {
                // "/plan " with only whitespace: toggle plan mode
                return Some(Command::PlanModeToggle);
            }
        }

        // Handle /feedback commands with optional explanation
        if let Some(rest) = trimmed
            .strip_prefix("/feedback critical ")
            .or_else(|| trimmed.strip_prefix("/feedback high "))
        {
            let explanation = rest.trim();
            return Some(Command::FeedbackCritical(if explanation.is_empty() {
                None
            } else {
                Some(explanation.to_string())
            }));
        }

        if trimmed == "/feedback critical" || trimmed == "/feedback high" {
            return Some(Command::FeedbackCritical(None));
        }

        if let Some(rest) = trimmed.strip_prefix("/feedback medium ") {
            let explanation = rest.trim();
            return Some(Command::FeedbackMedium(if explanation.is_empty() {
                None
            } else {
                Some(explanation.to_string())
            }));
        }

        if trimmed == "/feedback medium" {
            return Some(Command::FeedbackMedium(None));
        }

        if let Some(rest) = trimmed
            .strip_prefix("/feedback good ")
            .or_else(|| trimmed.strip_prefix("/feedback normal "))
        {
            let explanation = rest.trim();
            return Some(Command::FeedbackGood(if explanation.is_empty() {
                None
            } else {
                Some(explanation.to_string())
            }));
        }

        if trimmed == "/feedback good" || trimmed == "/feedback normal" {
            return Some(Command::FeedbackGood(None));
        }

        // Handle /compact command with optional instruction
        if let Some(rest) = trimmed.strip_prefix("/compact ") {
            let instruction = rest.trim();
            return Some(Command::Compact(if instruction.is_empty() {
                None
            } else {
                Some(instruction.to_string())
            }));
        }

        // Handle /local command with query
        if let Some(rest) = trimmed.strip_prefix("/local ") {
            let query = rest.trim();
            if !query.is_empty() {
                return Some(Command::Local {
                    query: query.to_string(),
                });
            }
        }

        // Handle /mcp commands with subcommands
        if trimmed == "/mcp" || trimmed == "/mcp list" {
            return Some(Command::McpList);
        }

        if trimmed == "/mcp refresh" {
            return Some(Command::McpRefresh);
        }

        if trimmed == "/mcp reload" {
            return Some(Command::McpReload);
        }

        if trimmed == "/mcp tools" {
            return Some(Command::McpTools(None));
        }

        if let Some(rest) = trimmed.strip_prefix("/mcp tools ") {
            let server = rest.trim();
            if !server.is_empty() {
                return Some(Command::McpTools(Some(server.to_string())));
            }
        }

        // Handle /patterns commands with subcommands
        if trimmed == "/patterns" || trimmed == "/patterns list" {
            return Some(Command::PatternsList);
        }

        if trimmed == "/patterns clear" {
            return Some(Command::PatternsClear);
        }

        if trimmed == "/patterns add" {
            return Some(Command::PatternsAdd);
        }

        // Handle /patterns remove <id> and /patterns rm <id>
        if let Some(rest) = trimmed.strip_prefix("/patterns remove ") {
            let id = rest.trim();
            if !id.is_empty() {
                return Some(Command::PatternsRemove(id.to_string()));
            }
        }

        if let Some(rest) = trimmed.strip_prefix("/patterns rm ") {
            let id = rest.trim();
            if !id.is_empty() {
                return Some(Command::PatternsRemove(id.to_string()));
            }
        }

        None
    }
}

pub fn handle_command(
    command: Command,
    metrics_logger: &MetricsLogger,
    router: Option<&Router>, // CHANGED: Router instead of ThresholdRouter
    validator: Option<&ThresholdValidator>,
    debug_enabled: &mut bool,
) -> Result<CommandOutput> {
    match command {
        // Long-form outputs go to scrollback
        Command::Help => Ok(CommandOutput::Message(format_help())),
        Command::Metrics => Ok(CommandOutput::Message(format_metrics(metrics_logger)?)),
        Command::Training => Ok(CommandOutput::Message(format_training(router, validator)?)),

        // Short outputs go to status bar
        Command::Debug => {
            *debug_enabled = !*debug_enabled;
            Ok(CommandOutput::Status(format!(
                "Debug mode: {}",
                if *debug_enabled { "ON" } else { "OFF" }
            )))
        }
        Command::Quit => Ok(CommandOutput::Status("Goodbye!".to_string())),
        Command::Clear => Ok(CommandOutput::Status("".to_string())), // Handled in REPL directly
        Command::Compact(_) => Ok(CommandOutput::Status("".to_string())), // Handled in REPL directly
        // Pattern commands are now handled directly in REPL
        Command::PatternsList
        | Command::PatternsRemove(_)
        | Command::PatternsClear
        | Command::PatternsAdd => Ok(CommandOutput::Status(
            "Pattern management commands should be handled in REPL.".to_string(),
        )),
        // Plan mode commands are handled directly in REPL
        Command::PlanModeToggle | Command::Plan(_) => Ok(CommandOutput::Status(
            "Plan mode commands should be handled in REPL.".to_string(),
        )),
        // Feedback commands are handled directly in REPL
        Command::FeedbackCritical(_) | Command::FeedbackMedium(_) | Command::FeedbackGood(_) => Ok(
            CommandOutput::Status("Feedback commands should be handled in REPL.".to_string()),
        ),
        // Local command is handled directly in REPL
        Command::Local { .. } => Ok(CommandOutput::Status(
            "Local command should be handled in REPL.".to_string(),
        )),
        // Memory command is handled directly in REPL
        Command::Memory => Ok(CommandOutput::Status(
            "Memory command should be handled in REPL.".to_string(),
        )),
        // MCP commands are handled directly in REPL
        Command::McpList | Command::McpTools(_) | Command::McpRefresh | Command::McpReload => Ok(
            CommandOutput::Status("MCP commands should be handled in REPL.".to_string()),
        ),
        // Persona commands are handled directly in REPL (Phase 2)
        Command::PersonaList | Command::PersonaSelect(_) | Command::PersonaShow => Ok(
            CommandOutput::Status("Persona commands should be handled in REPL.".to_string()),
        ),
        // Model/Teacher switching commands are handled directly in REPL
        Command::ModelList | Command::ModelSwitch(_) | Command::ModelShow => Ok(
            CommandOutput::Status("Model commands should be handled in REPL.".to_string()),
        ),
        // Service discovery is handled directly in REPL (Phase 3)
        Command::Discover | Command::Machines => Ok(CommandOutput::Status(
            "Service discovery commands should be handled in REPL.".to_string(),
        )),
        // License commands are handled directly in REPL
        Command::LicenseStatus | Command::LicenseActivate(_) | Command::LicenseRemove => Ok(
            CommandOutput::Status("License commands should be handled in REPL.".to_string()),
        ),
        // Brain commands are handled directly in REPL
        Command::Brain(_) | Command::Brains | Command::BrainCancel(_) => Ok(
            CommandOutput::Status("Brain commands should be handled in REPL.".to_string()),
        ),
        // Graph command is handled directly in REPL
        Command::Graph => Ok(CommandOutput::Status(
            "Graph command should be handled in REPL.".to_string(),
        )),
        // Ask / stack commands are handled directly in REPL
        Command::Ask(_)
        | Command::StackPush(_)
        | Command::StackShow
        | Command::StackPop
        | Command::StackRun
        | Command::StackClear
        | Command::StackProgram
        | Command::StackView
        | Command::StackDemo
        | Command::StackChain(_, _)
        | Command::StackForget(_)
        | Command::StackDup(_)
        | Command::StackSwap(_, _)
        | Command::StackDescribe(_)
        | Command::StackDefine(_, _)
        | Command::StackOverride(_, _)
        | Command::ForthEval(_)
        | Command::ForthUndo
        | Command::VmDump
        | Command::LibraryUndefine(_)
        | Command::LibraryRun(_) => Ok(CommandOutput::Status(
            "Stack commands should be handled in REPL.".to_string(),
        )),
        // Setup command is handled directly in REPL
        Command::Setup => Ok(CommandOutput::Status(
            "Setup command should be handled in REPL.".to_string(),
        )),
        Command::Share => Ok(CommandOutput::Status(
            "Share command should be handled in REPL.".to_string(),
        )),
        Command::BoxDiff => Ok(CommandOutput::Status(
            "BoxDiff command should be handled in REPL.".to_string(),
        )),
    }
}

/// Parse "W3" or "3" into a node id (usize).
fn parse_word_id(s: &str) -> Option<usize> {
    let s = s.trim();
    let digits = s.strip_prefix('W').or_else(|| s.strip_prefix('w')).unwrap_or(s);
    digits.parse::<usize>().ok()
}

pub fn format_help() -> String {
    "\x1b[1;36m╔═══════════════════════════════════════════════════════════════════════╗\x1b[0m\n\
         \x1b[1;36m║\x1b[0m                   \x1b[1;32mFinch Help - Commands & Shortcuts\x1b[0m                   \x1b[1;36m║\x1b[0m\n\
         \x1b[1;36m╚═══════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n\
         \x1b[1;33m📋 Basic Commands:\x1b[0m\n\
         \x1b[36m  /help\x1b[0m              Show this help message\n\
         \x1b[36m  /quit\x1b[0m              Exit the REPL (also: Ctrl+D)\n\
         \x1b[36m  /clear\x1b[0m             Clear conversation history and free up context\n\
         \x1b[36m  /compact [note]\x1b[0m    Clear history but keep a summary in context\n\
         \x1b[36m  /debug\x1b[0m             Toggle debug output\n\
         \x1b[36m  /metrics\x1b[0m           Display usage statistics\n\
         \x1b[36m  /memory\x1b[0m            Show memory usage (system and process)\n\
         \x1b[36m  /training\x1b[0m          Show detailed training statistics\n\n\
         \x1b[1;33m🤖 Provider Commands:\x1b[0m\n\
         \x1b[36m  /provider\x1b[0m          Show current active provider\n\
         \x1b[36m  /provider list\x1b[0m     List all configured providers (Claude, Grok, etc.)\n\
         \x1b[36m  /provider <name>\x1b[0m   Switch to a specific provider mid-session\n\
         \x1b[0m                     Example: /provider grok\n\
         \x1b[36m  /local <query>\x1b[0m     Query local ONNX model directly (bypass routing)\n\
         \x1b[0m                     Example: /local What is 2+2?\n\
         \x1b[0m\n\
         \x1b[90m  Aliases: /model and /teacher also work (kept for compatibility)\x1b[0m\n\
         \x1b[90m  Switch between Claude, Grok, GPT-4, local ONNX, etc.\x1b[0m\n\
         \x1b[90m  Conversation history is preserved across switches.\x1b[0m\n\n\
         \x1b[1;33m🔌 MCP Plugin Commands:\x1b[0m\n\
         \x1b[36m  /mcp list\x1b[0m          List connected MCP servers\n\
         \x1b[36m  /mcp tools\x1b[0m         List all MCP tools from all servers\n\
         \x1b[36m  /mcp tools <srv>\x1b[0m   List tools from specific server\n\
         \x1b[36m  /mcp refresh\x1b[0m       Refresh tool list from all servers\n\
         \x1b[36m  /mcp reload\x1b[0m        Reconnect to all MCP servers\n\
         \x1b[0m\n\
         \x1b[90m  What is MCP?\x1b[0m Model Context Protocol - extend Finch with external\n\
         \x1b[90m  tools (GitHub, filesystem, databases, etc.) via MCP servers.\n\n\
         \x1b[1;33m🎭 Persona Commands:\x1b[0m\n\
         \x1b[36m  /persona\x1b[0m           List available personas\n\
         \x1b[36m  /persona select <name>\x1b[0m Switch to a different persona\n\
         \x1b[36m  /persona show\x1b[0m      Show current persona and system prompt\n\
         \x1b[0m\n\
         \x1b[90m  What are personas?\x1b[0m Customize AI behavior and personality.\n\
         \x1b[90m  Built-in:\x1b[0m default, expert-coder, teacher, analyst, creative, researcher\n\n\
         \x1b[1;33m🔍 Service Discovery:\x1b[0m\n\
         \x1b[36m  /machines\x1b[0m          List known peer machines on the LAN\n\
         \x1b[36m  /discover\x1b[0m          Scan LAN for new Finch daemons (mDNS)\n\
         \x1b[0m\n\
         \x1b[90m  Uses mDNS/Bonjour to find remote Finch instances for distributed GPU access.\x1b[0m\n\n\
         \x1b[1;33m🔒 Tool Confirmation Patterns:\x1b[0m\n\
         \x1b[36m  /patterns\x1b[0m          List all saved confirmation patterns\n\
         \x1b[36m  /patterns add\x1b[0m      Add a new pattern (interactive wizard)\n\
         \x1b[36m  /patterns rm <id>\x1b[0m  Remove a specific pattern by ID\n\
         \x1b[36m  /patterns clear\x1b[0m    Remove all patterns (requires confirmation)\n\
         \x1b[0m\n\
         \x1b[90m  What are patterns?\x1b[0m Saved rules for auto-approving tool executions.\n\
         \x1b[90m  Example:\x1b[0m \"Always allow reading *.rs files\" or \"Allow git status\"\n\n\
         \x1b[1;33m📝 Plan Mode:\x1b[0m\n\
         \x1b[90m  Claude can enter plan mode to explore your codebase in read-only mode,\x1b[0m\n\
         \x1b[90m  then present a plan for your approval via an interactive dialog.\x1b[0m\n\
         \x1b[0m\n\
         \x1b[90m  Workflow:\x1b[0m 1. Ask Claude to plan → 2. Claude explores (read-only) →\n\
         \x1b[90m            3. Claude presents plan → 4. Dialog appears automatically →\n\
         \x1b[90m            5. You approve/request changes/reject → 6. Execution\n\n\
         \x1b[1;33m🎓 Weighted Feedback (LoRA Fine-Tuning):\x1b[0m\n\
         \x1b[36m  /critical [note]\x1b[0m   Mark response as \x1b[31mcritical error\x1b[0m (10x training weight)\n\
         \x1b[36m  /medium [note]\x1b[0m     Mark response \x1b[33mneeds improvement\x1b[0m (3x weight)\n\
         \x1b[36m  /good [note]\x1b[0m       Mark response as \x1b[32mgood example\x1b[0m (1x weight)\n\
         \x1b[0m\n\
         \x1b[90m  Aliases:\x1b[0m /feedback critical|high|medium|good [note]\n\
         \x1b[0m\n\
         \x1b[90m  Examples:\x1b[0m\n\
         \x1b[90m    /critical\x1b[0m Never use .unwrap() in production code\n\
         \x1b[90m    /medium\x1b[0m Prefer iterator chains over manual loops\n\
         \x1b[90m    /good\x1b[0m This is exactly the right approach\n\n\
         \x1b[1;33m⌨️  Keyboard Shortcuts:\x1b[0m\n\
         \x1b[36m  Ctrl+C\x1b[0m             Cancel current query (interrupts generation)\n\
         \x1b[36m  Ctrl+D\x1b[0m             Exit REPL (same as /quit)\n\
         \x1b[36m  Ctrl+G\x1b[0m             Mark last response as \x1b[32mgood\x1b[0m (1x training weight)\n\
         \x1b[36m  Ctrl+B\x1b[0m             Mark last response as \x1b[31mbad\x1b[0m (10x training weight)\n\
         \x1b[36m  Ctrl+Z\x1b[0m             Undo last Forth definition (/undefine)\n\
         \x1b[36m  Ctrl+P\x1b[0m             Pop top word off vocabulary stack (/pop)\n\
         \x1b[36m  Tab\x1b[0m                Complete /command (accepts ghost text)\n\
         \x1b[36m  Shift+Tab\x1b[0m          Toggle plan mode on/off\n\
         \x1b[36m  Shift+Enter\x1b[0m        Multi-line input (insert newline)\n\
         \x1b[36m  Shift+PgUp\x1b[0m         Scroll up in history\n\
         \x1b[36m  Shift+PgDown\x1b[0m       Scroll down in history\n\
         \x1b[90m  ↑ / ↓ arrows\x1b[0m       Navigate command history\n\n\
         \x1b[1;33m🛠️  Tool Execution:\x1b[0m\n\
         When Claude needs to use tools (read files, run commands, etc.), you'll\n\
         be asked to approve each action. You can:\n\
         \x1b[32m  • Approve once\x1b[0m              Execute this time only\n\
         \x1b[32m  • Approve for session\x1b[0m      Allow during this session\n\
         \x1b[32m  • Remember pattern\x1b[0m         Always allow (saves to /patterns)\n\
         \x1b[31m  • Deny\x1b[0m                     Reject the action\n\n\
         Available tools: Read, Glob, Grep, WebFetch, Bash, Restart\n\n\
         \x1b[1;33m📚 Co-Forth VM:\x1b[0m\n\
         \x1b[36m  /push <text>\x1b[0m       Push a word onto the stack (silent)\n\
         \x1b[36m  /pop\x1b[0m               Remove top item (undo last push)\n\
         \x1b[36m  /run\x1b[0m               Execute the program (shows approval dialog)\n\
         \x1b[36m  /program\x1b[0m           Show current program as Forth source\n\
         \x1b[36m  /stack\x1b[0m             Show stack contents\n\
         \x1b[36m  /stack clear\x1b[0m       Drop all stack items\n\
         \x1b[36m  /describe <word>\x1b[0m   Show library definition + related words\n\
         \x1b[36m  /define <w> <def>\x1b[0m  Add/override a word in your personal library\n\
         \x1b[36m  /define \"phrase\" <def>\x1b[0m Override a multi-word phrase or Chinese term\n\
         \x1b[36m  /define <w>:<sense>\x1b[0m Add a specific sense (e.g. /define bank:river the sloping land)\n\
         \x1b[90m                     (1030 English words preloaded — override at your peril)\x1b[0m\n\
         \x1b[0m\n\
         \x1b[90m  Type text to push words. The AI pushes back via Push tool.\n\
         The stack builds a Forth dialect. /run executes it.\x1b[0m\n\
         \x1b[90m  /run collapses the stack and executes it.\x1b[0m\n\n\
         \x1b[1;33m🧠 Daemon Brain Sessions:\x1b[0m\n\
         \x1b[36m  /brain <task>\x1b[0m      Spawn a background research brain\n\
         \x1b[90m                     Example: /brain investigate why auth tests are flaky\x1b[0m\n\
         \x1b[36m  /brains\x1b[0m            List active brain sessions\n\
         \x1b[36m  /brain cancel <n>\x1b[0m  Cancel a brain by name or id\n\
         \x1b[0m\n\
         \x1b[90m  Brains run in the daemon and survive REPL disconnects.\x1b[0m\n\
         \x1b[90m  When a brain has a question or plan, a dialog appears in the REPL.\x1b[0m\n\n\
         \x1b[1;33m📚 Learn More:\x1b[0m\n\
         \x1b[36m  GitHub:\x1b[0m   https://github.com/schancel/finch\n\
         \x1b[36m  Issues:\x1b[0m   https://github.com/schancel/finch/issues\n\
         \x1b[36m  Docs:\x1b[0m     See README.md and docs/ folder\n\n\
         \x1b[1;33m💡 Quick Start:\x1b[0m\n\
         Just type your question! Examples:\n\
         \x1b[90m  • How do I implement a binary search in Rust?\x1b[0m\n\
         \x1b[90m  • Can you read my Cargo.toml and explain the dependencies?\x1b[0m\n\
         \x1b[90m  • Find all TODO comments in my code\x1b[0m\n\n\
         \x1b[1;36m─────────────────────────────────────────────────────────────────────────\x1b[0m\n\
         \x1b[90mTip: Use Ctrl+C to cancel long-running queries\x1b[0m".to_string()
}

pub fn format_metrics(metrics_logger: &MetricsLogger) -> Result<String> {
    let summary = metrics_logger.get_today_summary()?;

    let local_pct = if summary.total > 0 {
        (summary.local_count as f64 / summary.total as f64) * 100.0
    } else {
        0.0
    };

    let forward_pct = if summary.total > 0 {
        (summary.forward_count as f64 / summary.total as f64) * 100.0
    } else {
        0.0
    };

    let crisis_pct = if summary.total > 0 {
        (summary.crisis_count as f64 / summary.total as f64) * 100.0
    } else {
        0.0
    };

    let no_match_pct = if summary.total > 0 {
        (summary.no_match_count as f64 / summary.total as f64) * 100.0
    } else {
        0.0
    };

    Ok(format!(
        "Metrics (last 24 hours):\n\
        Total requests: {}\n\
        Local: {} ({:.1}%)\n\
        Forwarded: {} ({:.1}%)\n\
          - Crisis: {} ({:.1}%)\n\
          - No match: {} ({:.1}%)\n\
        Avg response time (local): {}ms\n\
        Avg response time (forwarded): {}ms\n",
        summary.total,
        summary.local_count,
        local_pct,
        summary.forward_count,
        forward_pct,
        summary.crisis_count,
        crisis_pct,
        summary.no_match_count,
        no_match_pct,
        summary.avg_local_time,
        summary.avg_forward_time
    ))
}

pub fn format_training(
    router: Option<&Router>, // CHANGED: Router instead of ThresholdRouter
    validator: Option<&ThresholdValidator>,
) -> Result<String> {
    let mut output = String::new();
    output.push_str("Training Statistics\n");
    output.push_str("===================\n\n");

    if let Some(router) = router {
        let router_stats = router.stats();

        // Overall stats
        output.push_str(&format!("Total Queries: {}\n", router_stats.total_queries));
        output.push_str(&format!(
            "Local Attempts: {}\n",
            router_stats.total_local_attempts
        ));
        output.push_str(&format!(
            "Success Rate: {:.1}%\n",
            router_stats.success_rate * 100.0
        ));
        output.push_str(&format!(
            "Forward Rate: {:.1}%\n",
            router_stats.forward_rate * 100.0
        ));
        output.push_str(&format!(
            "Confidence Threshold: {:.2}\n\n",
            router_stats.confidence_threshold
        ));

        // Per-category breakdown
        output.push_str("Performance by Category:\n");
        let mut categories: Vec<_> = router_stats.categories.iter().collect();
        categories.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.local_attempts));

        for (category, stats) in categories {
            if stats.local_attempts > 0 {
                let success_rate = stats.successes as f64 / stats.local_attempts as f64 * 100.0;
                output.push_str(&format!(
                    "  {:?}: {} attempts, {:.1}% success\n",
                    category, stats.local_attempts, success_rate
                ));
            }
        }
    } else {
        output.push_str("No router statistics available\n");
    }

    if let Some(validator) = validator {
        let validator_stats = validator.stats();

        output.push_str("\nQuality Validation:\n");
        output.push_str(&format!(
            "Total Validations: {}\n",
            validator_stats.total_validations
        ));
        output.push_str(&format!("Approved: {}\n", validator_stats.approved));
        output.push_str(&format!("Rejected: {}\n", validator_stats.rejected));
        output.push_str(&format!(
            "Approval Rate: {:.1}%\n\n",
            validator_stats.approval_rate * 100.0
        ));

        output.push_str("Quality Signals:\n");
        let mut signals: Vec<_> = validator_stats.signal_stats.iter().collect();
        signals.sort_by_key(|(_, stats)| {
            std::cmp::Reverse(stats.present_and_good + stats.present_and_bad)
        });

        for (signal, stats) in signals {
            let total = stats.present_and_good + stats.present_and_bad;
            if total >= 5 {
                // Only show signals with enough data
                let precision = if total > 0 {
                    stats.present_and_good as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                output.push_str(&format!(
                    "  {:?}: {:.1}% precision ({} samples)\n",
                    signal, precision, total
                ));
            }
        }
    } else {
        output.push_str("\nNo validator statistics available\n");
    }

    Ok(output)
}

// Pattern management command handlers are now in Repl (Phase 3 implementation)
// The command handlers above return a placeholder message since the actual
// handling is done directly in the REPL loop to avoid borrowing issues

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_patterns_list() {
        assert!(matches!(
            Command::parse("/patterns"),
            Some(Command::PatternsList)
        ));
        assert!(matches!(
            Command::parse("/patterns list"),
            Some(Command::PatternsList)
        ));
    }

    #[test]
    fn test_parse_patterns_clear() {
        assert!(matches!(
            Command::parse("/patterns clear"),
            Some(Command::PatternsClear)
        ));
    }

    #[test]
    fn test_parse_patterns_add() {
        assert!(matches!(
            Command::parse("/patterns add"),
            Some(Command::PatternsAdd)
        ));
    }

    #[test]
    fn test_parse_patterns_remove() {
        // Test "remove" alias
        match Command::parse("/patterns remove abc123") {
            Some(Command::PatternsRemove(id)) => assert_eq!(id, "abc123"),
            _ => panic!("Expected PatternsRemove command"),
        }

        // Test "rm" alias
        match Command::parse("/patterns rm xyz789") {
            Some(Command::PatternsRemove(id)) => assert_eq!(id, "xyz789"),
            _ => panic!("Expected PatternsRemove command"),
        }

        // Test with extra whitespace
        match Command::parse("/patterns remove   abc123  ") {
            Some(Command::PatternsRemove(id)) => assert_eq!(id, "abc123"),
            _ => panic!("Expected PatternsRemove command"),
        }

        // Test empty ID returns None
        assert!(matches!(Command::parse("/patterns remove "), None));
        assert!(matches!(Command::parse("/patterns rm "), None));
    }

    #[test]
    fn test_parse_provider_commands() {
        // /provider is canonical
        assert!(matches!(
            Command::parse("/provider"),
            Some(Command::ModelShow)
        ));
        assert!(matches!(
            Command::parse("/provider show"),
            Some(Command::ModelShow)
        ));
        assert!(matches!(
            Command::parse("/provider list"),
            Some(Command::ModelList)
        ));
        // switch
        match Command::parse("/provider grok") {
            Some(Command::ModelSwitch(name)) => assert_eq!(name, "grok"),
            _ => panic!("Expected ModelSwitch(grok)"),
        }
        match Command::parse("/provider claude") {
            Some(Command::ModelSwitch(name)) => assert_eq!(name, "claude"),
            _ => panic!("Expected ModelSwitch(claude)"),
        }
        // Legacy aliases still work
        assert!(matches!(Command::parse("/model"), Some(Command::ModelShow)));
        assert!(matches!(
            Command::parse("/teacher"),
            Some(Command::ModelShow)
        ));
        assert!(matches!(
            Command::parse("/teacher list"),
            Some(Command::ModelList)
        ));
        match Command::parse("/teacher grok") {
            Some(Command::ModelSwitch(name)) => assert_eq!(name, "grok"),
            _ => panic!("Expected ModelSwitch(grok) via /teacher alias"),
        }
    }

    #[test]
    fn test_parse_existing_commands() {
        // Ensure existing commands still work
        assert!(matches!(Command::parse("/help"), Some(Command::Help)));
        assert!(matches!(Command::parse("/quit"), Some(Command::Quit)));
        assert!(matches!(Command::parse("/metrics"), Some(Command::Metrics)));
        assert!(matches!(Command::parse("/debug"), Some(Command::Debug)));
        assert!(matches!(
            Command::parse("/training"),
            Some(Command::Training)
        ));
        assert!(matches!(Command::parse("/clear"), Some(Command::Clear)));
    }

    #[test]
    fn test_parse_compact() {
        // Test /compact without argument
        match Command::parse("/compact") {
            Some(Command::Compact(None)) => (),
            _ => panic!("Expected Compact(None)"),
        }

        // Test /compact with instruction
        match Command::parse("/compact focus on key decisions") {
            Some(Command::Compact(Some(instruction))) => {
                assert_eq!(instruction, "focus on key decisions");
            }
            _ => panic!("Expected Compact(Some(...))"),
        }

        // Test with extra whitespace
        match Command::parse("/compact   key points  ") {
            Some(Command::Compact(Some(instruction))) => {
                assert_eq!(instruction, "key points");
            }
            _ => panic!("Expected Compact(Some(...))"),
        }

        // Test empty instruction (should be None)
        match Command::parse("/compact ") {
            Some(Command::Compact(None)) => (),
            other => panic!("Expected Compact(None), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_invalid_patterns_command() {
        // Invalid subcommands should return None
        assert!(matches!(Command::parse("/patterns invalid"), None));
        assert!(matches!(Command::parse("/patterns remove"), None)); // Missing ID
        assert!(matches!(Command::parse("/patterns rm"), None)); // Missing ID
    }

    // MCP Command Tests

    #[test]
    fn test_parse_mcp_list() {
        // Both /mcp and /mcp list should work
        assert!(matches!(Command::parse("/mcp"), Some(Command::McpList)));
        assert!(matches!(
            Command::parse("/mcp list"),
            Some(Command::McpList)
        ));
    }

    #[test]
    fn test_parse_mcp_tools() {
        // /mcp tools with no argument
        match Command::parse("/mcp tools") {
            Some(Command::McpTools(None)) => (),
            _ => panic!("Expected McpTools(None)"),
        }

        // /mcp tools with server name
        match Command::parse("/mcp tools filesystem") {
            Some(Command::McpTools(Some(server))) => {
                assert_eq!(server, "filesystem");
            }
            _ => panic!("Expected McpTools(Some(...))"),
        }

        // With extra whitespace
        match Command::parse("/mcp tools   github  ") {
            Some(Command::McpTools(Some(server))) => {
                assert_eq!(server, "github");
            }
            _ => panic!("Expected McpTools(Some(...))"),
        }
    }

    #[test]
    fn test_parse_mcp_refresh() {
        assert!(matches!(
            Command::parse("/mcp refresh"),
            Some(Command::McpRefresh)
        ));
    }

    #[test]
    fn test_parse_mcp_reload() {
        assert!(matches!(
            Command::parse("/mcp reload"),
            Some(Command::McpReload)
        ));
    }

    #[test]
    fn test_parse_mcp_invalid() {
        // Invalid subcommands should return None
        assert!(matches!(Command::parse("/mcp invalid"), None));
        // Note: "/mcp " (with trailing space) is trimmed to "/mcp" which matches McpList
    }

    #[test]
    fn test_parse_mcp_case_sensitive() {
        // Commands should be case-sensitive (lowercase only)
        assert!(matches!(Command::parse("/MCP list"), None));
        assert!(matches!(Command::parse("/mcp LIST"), None));
        assert!(matches!(Command::parse("/Mcp list"), None));
    }

    #[test]
    fn test_parse_mcp_with_leading_trailing_whitespace() {
        // Should handle whitespace correctly
        assert!(matches!(
            Command::parse("  /mcp list  "),
            Some(Command::McpList)
        ));
        assert!(matches!(
            Command::parse("\t/mcp refresh\t"),
            Some(Command::McpRefresh)
        ));
    }

    #[test]
    fn test_mcp_tools_empty_server_name() {
        // /mcp tools with only whitespace after should be treated as no argument
        match Command::parse("/mcp tools   ") {
            Some(Command::McpTools(None)) => (),
            other => panic!("Expected McpTools(None), got {:?}", other),
        }
    }

    #[test]
    fn test_mcp_tools_multiple_words() {
        // Server names can contain spaces (though unlikely in practice)
        // The entire string after "/mcp tools " is captured as the server name
        match Command::parse("/mcp tools my server") {
            Some(Command::McpTools(Some(server))) => {
                // Full string is captured including spaces
                assert_eq!(server, "my server");
            }
            _ => panic!("Expected McpTools with server name"),
        }
    }
}
