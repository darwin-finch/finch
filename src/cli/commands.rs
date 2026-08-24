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
    Discover, // Discover Finch daemons on local network
    Machines, // List known peer machines (from LAN discovery)
    // License management
    LicenseStatus,           // /license or /license status
    LicenseActivate(String), // /license activate <key>
    LicenseRemove,           // /license remove
    // Durable Brain sessions
    Brains,                        // /brains — list named, detachable sessions
    BrainArchive(String),          // /brain archive <name>
    BrainAttach(String),           // /brain attach <name@machine[:port]>
    BrainDetach,                   // /brain detach
    BrainPassword(Option<String>), // /brain password [new-password]
    // Execution graph
    Graph, // /graph — show causal trace of last query
    // Co-Forth VM stack ops
    Ask(String),       // /ask <query>      — send directly to AI (bypass stack)
    StackPush(String), // /push <text>      — push text onto the stack
    StackShow,         // /stack            — show current stack contents
    StackPop,          // /pop              — remove top item (undo last push)
    StackRun,          // /run              — execute full stack as one query
    StackEval,         // /eval-each        — evaluate each stack item independently, show results
    StackClear,        // /stack clear      — drop all stack items
    StackProgram,      // /program          — switch panel to Forth source view
    StackView,         // /view             — switch panel to graph view (toggle)
    StackDemo,         // /demo             — seed an example language to play with
    // Special Forth vocabulary ops
    StackChain(usize, usize),      // /chain W1 W2      — add edge W1 → W2
    StackForget(usize),            // /forget W1        — remove word and AI descendants
    StackDup(usize),               // /dup W1           — clone word as new entry
    StackSwap(usize, usize),       // /swap W1 W2       — swap labels of two words
    StackDescribe(String),         // /describe <word>  — show library entry for a word
    StackDefine(String, String),   // /define <word> <def> — add word to repo vocabulary
    StackOverride(String, String), // /override <word> <def> — machine-local override (~/.finch/library.toml)
    ForthEval(String),             // : word ... ; or /forth <expr> — eval in Forth interpreter
    ForthUndo,                     // /undefine — undo last Forth definition
    VmDump,                        // /vm — dump VM source to scrollback + clipboard
    LibraryUndefine(String),       // /undefine <word> — remove last user library entry for word
    LibraryRun(String),            // /run <word> — execute the Forth snippet for a library word
    Setup,   // /setup — open the setup wizard (run 'finch setup' to reconfigure)
    Share,   // /share — format session as a pasteable proof block
    BoxDiff, // /box-diff — compare all peers, offer to fix outliers
    // Channel commands (IRC-style)
    JoinChannel(String),        // /join #channel        — join a named channel
    PartChannel(String),        // /part #channel        — leave a named channel
    SayChannel(String, String), // /say #channel message — send a message to a channel
    // Peer connect / disconnect
    Connect(String), // /connect <host:port>   — add peer to current room + peer list
    Disconnect(String), // /disconnect <name-or-addr> — remove peer from room + list
    // Room management
    Room(Option<String>), // /room [uuid]  — join/create room (no uuid = show current)
    RoomNew,              // /room new     — create a fresh room with a random UUID
    RoomAdd(String),      // /room add <addr>
    RoomRemove(String),   // /room remove <name-or-addr>
    RoomList,             // /room list    — list all rooms + member counts
    SelfFix,              // /self-fix     — diagnose, fix, verify, restart
    // Peer registry / gas ledger
    SelfPeer,             // /self-peer            — register local daemon with itself
    GasSend(String, u64), // /gas-send <addr> <ms> — send gas to a peer
    Balance,              // /balance              — show my ledger balance
    Settle(String),       // /settle <addr>        — settle debt with peer
    JoinRegistry(String), // /join-registry <addr> — register with a remote registry
    RegistrySet(String),  // /registry <addr>      — set registry address
    // Diff proposal flow
    Accept(Option<String>), // /accept [diff-id-prefix] — accept most-recent (or matched) pending diff
    Reject(Option<String>), // /reject [reason]         — reject most-recent pending diff
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        // Model profile names are user-defined and may end in punctuation
        // (for example "GPT-4o (work)"). Parse these before the historical
        // punctuation cleanup used for conversational slash commands.
        let raw = input.trim();
        match raw {
            "/provider" | "/provider show" | "/model" | "/model show" | "/teacher"
            | "/teacher show" => return Some(Command::ModelShow),
            "/provider list" | "/model list" | "/teacher list" => return Some(Command::ModelList),
            _ => {}
        }
        if let Some(rest) = raw
            .strip_prefix("/provider ")
            .or_else(|| raw.strip_prefix("/model "))
            .or_else(|| raw.strip_prefix("/teacher "))
        {
            let profile_name = rest.trim();
            if profile_name != "list" && profile_name != "show" && !profile_name.is_empty() {
                return Some(Command::ModelSwitch(profile_name.to_string()));
            }
        }

        let trimmed = input
            .trim()
            .trim_end_matches(|c: char| c.is_ascii_punctuation() && c != '/');

        // Handle simple commands without arguments
        match trimmed {
            "/" => return Some(Command::Help),
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
            // Room management (no-arg forms)
            "/room" | "/room show" => return Some(Command::Room(None)),
            "/room new" => return Some(Command::RoomNew),
            "/room list" | "/rooms" => return Some(Command::RoomList),
            // License management
            "/license" | "/license status" => return Some(Command::LicenseStatus),
            "/license remove" => return Some(Command::LicenseRemove),
            // Brain sessions
            "/brains" | "/brains list" | "/brain list" | "/brain ls" => {
                return Some(Command::Brains);
            }
            "/brain detach" => return Some(Command::BrainDetach),
            "/brain password" => return Some(Command::BrainPassword(None)),
            "/graph" => return Some(Command::Graph),
            // Co-Forth VM
            "/vm" | "/vm dump" | "/vm copy" => return Some(Command::VmDump),
            "/stack" | "/stack list" | "/stack show" => return Some(Command::StackShow),
            "/stack clear" | "/stack reset" => return Some(Command::StackClear),
            "/pop" => return Some(Command::StackPop),
            "/run" | "/execute" => return Some(Command::StackRun),
            "/eval-each" | "/eval" => return Some(Command::StackEval),
            "/program" | "/words" | "/forth" => return Some(Command::StackProgram),
            "/view" | "/graph view" | "/poset" => return Some(Command::StackView),
            "/demo" | "/demo lang" => return Some(Command::StackDemo),
            "/setup" => return Some(Command::Setup),
            "/share" | "/prove" | "/proof" => return Some(Command::Share),
            "/box-diff" | "/cluster-diff" | "/cdiff" => return Some(Command::BoxDiff),
            "/self-fix" | "/fix" | "/repair" => return Some(Command::SelfFix),
            // Diff proposal flow (no-arg forms)
            "/accept" => return Some(Command::Accept(None)),
            "/reject" => return Some(Command::Reject(None)),
            _ => {}
        }

        // Handle /license activate <key>
        // Peer connect / disconnect
        // Diff proposal flow with arguments
        if let Some(rest) = trimmed.strip_prefix("/accept ") {
            let prefix = rest.trim();
            return Some(Command::Accept(if prefix.is_empty() {
                None
            } else {
                Some(prefix.to_string())
            }));
        }
        if let Some(rest) = trimmed.strip_prefix("/reject ") {
            let reason = rest.trim();
            return Some(Command::Reject(if reason.is_empty() {
                None
            } else {
                Some(reason.to_string())
            }));
        }

        // Channel commands
        fn ensure_hash(s: &str) -> String {
            if s.starts_with('#') {
                s.to_string()
            } else {
                format!("#{s}")
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/join ") {
            let chan = rest.trim();
            if !chan.is_empty() {
                return Some(Command::JoinChannel(ensure_hash(chan)));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/part ") {
            let chan = rest.trim();
            if !chan.is_empty() {
                return Some(Command::PartChannel(ensure_hash(chan)));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/say ") {
            let rest = rest.trim();
            if let Some(space) = rest.find(|c: char| c.is_whitespace()) {
                let chan = rest[..space].trim();
                let msg = rest[space..].trim();
                if !chan.is_empty() && !msg.is_empty() {
                    return Some(Command::SayChannel(ensure_hash(chan), msg.to_string()));
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("/connect ") {
            let addr = rest.trim();
            if !addr.is_empty() {
                return Some(Command::Connect(addr.to_string()));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/disconnect ") {
            let name = rest.trim();
            if !name.is_empty() {
                return Some(Command::Disconnect(name.to_string()));
            }
        }
        // Room management with arguments
        if let Some(rest) = trimmed.strip_prefix("/room add ") {
            let addr = rest.trim();
            if !addr.is_empty() {
                return Some(Command::RoomAdd(addr.to_string()));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/room remove ") {
            let addr = rest.trim();
            if !addr.is_empty() {
                return Some(Command::RoomRemove(addr.to_string()));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/room ") {
            let uuid = rest.trim();
            if !uuid.is_empty() {
                return Some(Command::Room(Some(uuid.to_string())));
            }
        }

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
                    (
                        rest[..space].trim().to_string(),
                        rest[space..].trim().to_string(),
                    )
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
                        (
                            rest[1..=close].to_string(),
                            rest[close + 2..].trim().to_string(),
                        )
                    } else {
                        (rest.trim_matches('"').to_string(), String::new())
                    }
                } else if let Some(space) = rest.find(|c: char| c.is_whitespace()) {
                    (
                        rest[..space].trim().to_string(),
                        rest[space..].trim().to_string(),
                    )
                } else {
                    (rest.to_string(), String::new())
                };
                if !word.is_empty() {
                    return Some(Command::StackOverride(word, definition));
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("/brain attach ") {
            let target = rest.trim();
            if !target.is_empty() {
                return Some(Command::BrainAttach(target.to_string()));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/brain archive ") {
            let name = rest.trim();
            if !name.is_empty() {
                return Some(Command::BrainArchive(name.to_string()));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/brain password ") {
            let password = rest.trim();
            if !password.is_empty() {
                return Some(Command::BrainPassword(Some(password.to_string())));
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

        // Peer registry / gas ledger
        if trimmed == "/self-peer" {
            return Some(Command::SelfPeer);
        }
        if trimmed == "/balance" {
            return Some(Command::Balance);
        }
        if let Some(rest) = trimmed.strip_prefix("/settle ") {
            let addr = rest.trim();
            if !addr.is_empty() {
                return Some(Command::Settle(addr.to_string()));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/join-registry ") {
            let addr = rest.trim();
            if !addr.is_empty() {
                return Some(Command::JoinRegistry(addr.to_string()));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/registry ") {
            let addr = rest.trim();
            if !addr.is_empty() {
                return Some(Command::RegistrySet(addr.to_string()));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("/gas-send ") {
            // /gas-send <addr> <amount_ms>
            let rest = rest.trim();
            let parts: Vec<&str> = rest.rsplitn(2, ' ').collect();
            if parts.len() == 2 {
                if let Ok(ms) = parts[0].parse::<u64>() {
                    let addr = parts[1].trim();
                    if !addr.is_empty() {
                        return Some(Command::GasSend(addr.to_string(), ms));
                    }
                }
            }
        }

        // Any unrecognized /command → show help instead of falling through to Forth/NL.
        if trimmed.starts_with('/') {
            return Some(Command::Help);
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
        Command::Brains
        | Command::BrainArchive(_)
        | Command::BrainAttach(_)
        | Command::BrainDetach
        | Command::BrainPassword(_) => Ok(CommandOutput::Status(
            "Brain commands should be handled in REPL.".to_string(),
        )),
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
        | Command::StackEval
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
        // Peer / room commands — all handled in the REPL event loop
        Command::Connect(_)
        | Command::Disconnect(_)
        | Command::Room(_)
        | Command::RoomNew
        | Command::RoomAdd(_)
        | Command::RoomRemove(_)
        | Command::RoomList => Ok(CommandOutput::Status(
            "Peer/room command should be handled in REPL.".to_string(),
        )),
        Command::SelfFix => Ok(CommandOutput::Status(
            "SelfFix command should be handled in REPL.".to_string(),
        )),
        // Peer registry / gas ledger — handled in the REPL event loop
        Command::SelfPeer
        | Command::GasSend(_, _)
        | Command::Balance
        | Command::Settle(_)
        | Command::JoinRegistry(_)
        | Command::RegistrySet(_) => Ok(CommandOutput::Status(
            "Registry/gas commands should be handled in REPL.".to_string(),
        )),
        // Diff proposal flow — handled in the REPL event loop
        Command::Accept(_) | Command::Reject(_) => Ok(CommandOutput::Status(
            "Diff command should be handled in REPL.".to_string(),
        )),
        // Channel commands are handled directly in REPL
        Command::JoinChannel(_) | Command::PartChannel(_) | Command::SayChannel(_, _) => Ok(
            CommandOutput::Status("Channel commands should be handled in REPL.".to_string()),
        ),
    }
}

/// Parse "W3" or "3" into a node id (usize).
fn parse_word_id(s: &str) -> Option<usize> {
    let s = s.trim();
    let digits = s
        .strip_prefix('W')
        .or_else(|| s.strip_prefix('w'))
        .unwrap_or(s);
    digits.parse::<usize>().ok()
}

pub fn format_help() -> String {
    use crossterm::style::{Attribute, Color, SetAttribute, SetForegroundColor};
    let reset = SetAttribute(Attribute::Reset);
    let cyan = SetForegroundColor(Color::DarkCyan);
    let gray = SetForegroundColor(Color::DarkGrey);
    let red = SetForegroundColor(Color::DarkRed);
    let green = SetForegroundColor(Color::DarkGreen);
    let yellow = SetForegroundColor(Color::DarkYellow);
    let cyan_bold = format!("{}{}", SetAttribute(Attribute::Bold), cyan);
    let green_bold = format!("{}{}", SetAttribute(Attribute::Bold), green);
    let yellow_bold = format!("{}{}", SetAttribute(Attribute::Bold), yellow);
    format!("{cyan_bold}╔═══════════════════════════════════════════════════════════════════════╗{reset}\n\
         {cyan_bold}║{reset}                   {green_bold}Finch Help - Commands & Shortcuts{reset}                   {cyan_bold}║{reset}\n\
         {cyan_bold}╚═══════════════════════════════════════════════════════════════════════╝{reset}\n\n\
         {yellow_bold}📋 Basic Commands:{reset}\n\
         {cyan}  /help{reset}              Show this help message\n\
         {cyan}  /quit{reset}              Exit the REPL (also: Ctrl+D)\n\
         {cyan}  /clear{reset}             Clear conversation history and free up context\n\
         {cyan}  /compact [note]{reset}    Clear history but keep a summary in context\n\
         {cyan}  /debug{reset}             Toggle debug output\n\
         {cyan}  /metrics{reset}           Display usage statistics\n\
         {cyan}  /memory{reset}            Show memory usage (system and process)\n\
         {cyan}  /training{reset}          Show detailed training statistics\n\n\
         {yellow_bold}🤖 Provider Commands:{reset}\n\
         {cyan}  /model{reset}             Show current named model profile\n\
         {cyan}  /model list{reset}        List configured cloud and local profiles\n\
         {cyan}  /model <name>{reset}      Switch profiles without clearing context\n\
         {reset}                     Example: /provider grok\n\
         {cyan}  /local <query>{reset}     Query local ONNX model directly (bypass routing)\n\
         {reset}                     Example: /local What is 2+2?\n\
         {reset}\n\
         {gray}  Aliases: /model and /teacher also work (kept for compatibility){reset}\n\
         {gray}  Switch between Claude, Grok, GPT-4, local ONNX, etc.{reset}\n\
         {gray}  Conversation history is preserved across switches.{reset}\n\n\
         {yellow_bold}🔌 MCP Plugin Commands:{reset}\n\
         {cyan}  /mcp list{reset}          List connected MCP servers\n\
         {cyan}  /mcp tools{reset}         List all MCP tools from all servers\n\
         {cyan}  /mcp tools <srv>{reset}   List tools from specific server\n\
         {cyan}  /mcp refresh{reset}       Refresh tool list from all servers\n\
         {cyan}  /mcp reload{reset}        Reconnect to all MCP servers\n\
         {reset}\n\
         {gray}  What is MCP?{reset} Model Context Protocol - extend Finch with external\n\
         {gray}  tools (GitHub, filesystem, databases, etc.) via MCP servers.\n\n\
         {yellow_bold}🎭 Persona Commands:{reset}\n\
         {cyan}  /persona{reset}           List available personas\n\
         {cyan}  /persona select <name>{reset} Switch to a different persona\n\
         {cyan}  /persona show{reset}      Show current persona and system prompt\n\
         {reset}\n\
         {gray}  What are personas?{reset} Customize AI behavior and personality.\n\
         {gray}  Built-in:{reset} default, expert-coder, teacher, analyst, creative, researcher\n\n\
         {yellow_bold}🔍 Service Discovery:{reset}\n\
         {cyan}  /machines{reset}          List known peer machines on the LAN\n\
         {cyan}  /discover{reset}          Scan LAN for new Finch daemons (mDNS)\n\
         {reset}\n\
         {gray}  Uses mDNS/Bonjour to find remote Finch instances for distributed GPU access.{reset}\n\n\
         {yellow_bold}🔒 Tool Confirmation Patterns:{reset}\n\
         {cyan}  /patterns{reset}          List all saved confirmation patterns\n\
         {cyan}  /patterns add{reset}      Add a new pattern (interactive wizard)\n\
         {cyan}  /patterns rm <id>{reset}  Remove a specific pattern by ID\n\
         {cyan}  /patterns clear{reset}    Remove all patterns (requires confirmation)\n\
         {reset}\n\
         {gray}  What are patterns?{reset} Saved rules for auto-approving tool executions.\n\
         {gray}  Example:{reset} \"Always allow reading *.rs files\" or \"Allow git status\"\n\n\
         {yellow_bold}📝 Plan Mode:{reset}\n\
         {gray}  Claude can enter plan mode to explore your codebase in read-only mode,{reset}\n\
         {gray}  then present a plan for your approval via an interactive dialog.{reset}\n\
         {reset}\n\
         {gray}  Workflow:{reset} 1. Ask Claude to plan → 2. Claude explores (read-only) →\n\
         {gray}            3. Claude presents plan → 4. Dialog appears automatically →\n\
         {gray}            5. You approve/request changes/reject → 6. Execution\n\n\
         {yellow_bold}🎓 Weighted Feedback (LoRA Fine-Tuning):{reset}\n\
         {cyan}  /critical [note]{reset}   Mark response as {red}critical error{reset} (10x training weight)\n\
         {cyan}  /medium [note]{reset}     Mark response {yellow}needs improvement{reset} (3x weight)\n\
         {cyan}  /good [note]{reset}       Mark response as {green}good example{reset} (1x weight)\n\
         {reset}\n\
         {gray}  Aliases:{reset} /feedback critical|high|medium|good [note]\n\
         {reset}\n\
         {gray}  Examples:{reset}\n\
         {gray}    /critical{reset} Never use .unwrap() in production code\n\
         {gray}    /medium{reset} Prefer iterator chains over manual loops\n\
         {gray}    /good{reset} This is exactly the right approach\n\n\
         {yellow_bold}⌨️  Keyboard Shortcuts:{reset}\n\
         {cyan}  Ctrl+C{reset}             Cancel current query (interrupts generation)\n\
         {cyan}  Ctrl+D{reset}             Exit REPL (same as /quit)\n\
         {cyan}  Ctrl+G{reset}             Mark last response as {green}good{reset} (1x training weight)\n\
         {cyan}  Ctrl+B{reset}             Mark last response as {red}bad{reset} (10x training weight)\n\
         {cyan}  Ctrl+Z{reset}             Undo last Forth definition (/undefine)\n\
         {cyan}  Ctrl+P{reset}             Pop top word off vocabulary stack (/pop)\n\
         {cyan}  Tab{reset}                Complete /command (accepts ghost text)\n\
         {cyan}  Shift+Tab{reset}          Toggle plan mode on/off\n\
         {cyan}  Shift+Enter{reset}        Multi-line input (insert newline)\n\
         {cyan}  Shift+PgUp{reset}         Scroll up in history\n\
         {cyan}  Shift+PgDown{reset}       Scroll down in history\n\
         {gray}  ↑ / ↓ arrows{reset}       Navigate command history\n\n\
         {yellow_bold}🛠️  Tool Execution:{reset}\n\
         When Claude needs to use tools (read files, run commands, etc.), you'll\n\
         be asked to approve each action. You can:\n\
         {green}  • Approve once{reset}              Execute this time only\n\
         {green}  • Approve for session{reset}      Allow during this session\n\
         {green}  • Remember pattern{reset}         Always allow (saves to /patterns)\n\
         {red}  • Deny{reset}                     Reject the action\n\n\
         Available tools: Read, Glob, Grep, WebFetch, Bash, Restart\n\n\
         {yellow_bold}📚 Co-Forth VM:{reset}\n\
         {cyan}  /push <text>{reset}       Push a word onto the stack (silent)\n\
         {cyan}  /pop{reset}               Remove top item (undo last push)\n\
         {cyan}  /run{reset}               Execute the program (shows approval dialog)\n\
         {cyan}  /program{reset}           Show current program as Forth source\n\
         {cyan}  /stack{reset}             Show stack contents\n\
         {cyan}  /stack clear{reset}       Drop all stack items\n\
         {cyan}  /describe <word>{reset}   Show library definition + related words\n\
         {cyan}  /define <w> <def>{reset}  Add/override a word in your personal library\n\
         {cyan}  /define \"phrase\" <def>{reset} Override a multi-word phrase or Chinese term\n\
         {cyan}  /define <w>:<sense>{reset} Add a specific sense (e.g. /define bank:river the sloping land)\n\
         {gray}                     (1030 English words preloaded — override at your peril){reset}\n\
         {reset}\n\
         {gray}  Type text to push words. The AI pushes back via Push tool.\n\
         The stack builds a Forth dialect. /run executes it.{reset}\n\
         {gray}  /run collapses the stack and executes it.{reset}\n\n\
         {yellow_bold}💬 Channel Commands:{reset}\n\
         {cyan}  /join #channel{reset}     Join a named channel; announce to all peers\n\
         {cyan}  /part #channel{reset}     Leave a named channel\n\
         {cyan}  /say #channel msg{reset}  Send a message to a channel\n\
         {reset}\n\
         {yellow_bold}🔀 Diff Proposal Flow:{reset}\n\
         {gray}  Peers (AI or remote) propose diffs in the room. You argue back in chat.{reset}\n\
         {gray}  When you're satisfied, accept or reject:{reset}\n\
         {cyan}  /accept{reset}            Apply the most recent pending diff\n\
         {cyan}  /accept <prefix>{reset}   Apply the diff whose id starts with prefix\n\
         {cyan}  /reject [reason]{reset}   Reject the most recent pending diff\n\
         {reset}\n\
         {yellow_bold}🧠 Detachable Brains:{reset}\n\
         {cyan}  /brain list{reset}        List named Brain sessions ({gray}/brains{reset} also works)\n\
         {cyan}  /brain attach <name>[@machine]{reset} Attach locally or to a remote Brain\n\
         {cyan}  /brain detach{reset}      Return to this console's home Brain\n\
         {cyan}  /brain archive <name>{reset} Remove an inactive Brain but preserve its log\n\
         {cyan}  /brain password [new]{reset} Show or rotate the local brain credential\n\
         {reset}\n\
         {gray}  A Brain is one durable session; agents and scheduled work are runs within it.{reset}\n\n\
         {yellow_bold}📚 Learn More:{reset}\n\
         {cyan}  GitHub:{reset}   https://github.com/darwin-finch/finch\n\
         {cyan}  Issues:{reset}   https://github.com/darwin-finch/finch/issues\n\
         {cyan}  Docs:{reset}     See README.md and docs/ folder\n\n\
         {yellow_bold}💡 Quick Start:{reset}\n\
         Just type your question! Examples:\n\
         {gray}  • How do I implement a binary search in Rust?{reset}\n\
         {gray}  • Can you read my Cargo.toml and explain the dependencies?{reset}\n\
         {gray}  • Find all TODO comments in my code{reset}\n\n\
         {cyan_bold}─────────────────────────────────────────────────────────────────────────{reset}\n\
         {gray}Tip: Use Ctrl+C to cancel long-running queries{reset}")
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
    fn parses_named_brain_attachment_commands() {
        assert!(matches!(
            Command::parse("/brain attach finch@workstation.local"),
            Some(Command::BrainAttach(target)) if target == "finch@workstation.local"
        ));
        assert!(matches!(
            Command::parse("/brain detach"),
            Some(Command::BrainDetach)
        ));
        assert!(matches!(
            Command::parse("/brain password new-secret-value"),
            Some(Command::BrainPassword(Some(password))) if password == "new-secret-value"
        ));
        assert!(matches!(Command::parse("/brain list"), Some(Command::Brains)));
        assert!(matches!(Command::parse("/brain ls"), Some(Command::Brains)));
        assert!(matches!(
            Command::parse("/brain archive old-project"),
            Some(Command::BrainArchive(name)) if name == "old-project"
        ));
        assert!(matches!(
            Command::parse("/brain investigate flaky tests"),
            Some(Command::Help)
        ));
        assert!(matches!(
            Command::parse("/brain cancel old-worker"),
            Some(Command::Help)
        ));
        assert!(format_help().contains("/brain list"));
        assert!(!format_help().contains("Spawn a background research brain"));
    }

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

        // Test empty ID — catch-all returns Help (unknown /command with no ID)
        assert!(matches!(
            Command::parse("/patterns remove "),
            Some(Command::Help)
        ));
        assert!(matches!(
            Command::parse("/patterns rm "),
            Some(Command::Help)
        ));
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
        match Command::parse("/model GPT-4o (work)") {
            Some(Command::ModelSwitch(name)) => assert_eq!(name, "GPT-4o (work)"),
            _ => panic!("Expected punctuation in the profile name to be preserved"),
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
        // Invalid/incomplete /commands → catch-all returns Help
        assert!(matches!(
            Command::parse("/patterns invalid"),
            Some(Command::Help)
        ));
        assert!(matches!(
            Command::parse("/patterns remove"),
            Some(Command::Help)
        )); // Missing ID
        assert!(matches!(
            Command::parse("/patterns rm"),
            Some(Command::Help)
        )); // Missing ID
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
        // Invalid subcommands → catch-all returns Help
        assert!(matches!(
            Command::parse("/mcp invalid"),
            Some(Command::Help)
        ));
        // Note: "/mcp " (with trailing space) is trimmed to "/mcp" which matches McpList
    }

    #[test]
    fn test_parse_mcp_case_sensitive() {
        // Uppercase /commands don't match known commands → catch-all returns Help
        assert!(matches!(Command::parse("/MCP list"), Some(Command::Help)));
        assert!(matches!(Command::parse("/mcp LIST"), Some(Command::Help)));
        assert!(matches!(Command::parse("/Mcp list"), Some(Command::Help)));
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
