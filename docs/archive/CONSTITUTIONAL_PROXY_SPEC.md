# Shammah - Constitutional AI Proxy
## Implementation Specification

**Version:** 1.0  
**Date:** January 29, 2026  
**Target Platform:** macOS (Apple Silicon M1/M2/M3/M4)  
**Language:** Rust  
**Command:** `shammah`  
**Status:** Ready for implementation

---

## Executive Summary

Build a local AI assistant ("Shammah" - watchman/guardian) in Rust that runs on Apple Silicon and handles 95% of requests locally, forwarding only difficult cases to Claude API. The system uses adaptive uncertainty estimation to maintain quality while minimizing costs and maximizing privacy.

**The name "Shammah"** comes from Hebrew meaning "watchman" or "guardian" - fitting for a tool that provides constitutional oversight and protection while watching over your interactions.

**Key Benefits:**
- 76% cost reduction (from $10k/year to $2.4k/year at 1M requests)
- 95% of requests stay private on-device
- Fast response times with local processing
- Drop-in replacement for Claude Code with same interface
- Seamless setup using existing `~/.claude/settings.json`

---

## Design Principles

### 1. Claude Code Compatibility FIRST
**This is a drop-in replacement for Claude Code that adds local processing.**

Users should be able to:
- Run `shammah` exactly like they run `claude`
- Use the same slash commands (`/help`, `/init`, `/clear`, etc.)
- Use the same CLAUDE.md files for context
- Use the same `.claude/settings.json` configuration format
- Use the same tool permissions system
- Transparently fall back to real Claude when needed

**Key insight:** Users already know how to use Claude Code. Don't make them learn a new interface.

### 2. Progressive De-risking
Build in phases with validation gates. Each phase proves a critical assumption before proceeding. Do not build the full system at once.

### 3. Privacy by Default
- All processing happens locally when possible (95%+ of requests)
- Only forwards to Claude API when truly needed
- No data collection without explicit consent
- Metrics stored locally only

---

## Naming

**Command name options:**
1. `claude-proxy` - Descriptive, makes it clear what it is
2. `shammah` - Hebrew "watchman/guardian", nice metaphor for constitutional oversight
3. `cproxy` - Short and sweet
4. `claude-local` - Emphasizes local processing
5. `constitutional` - Full and explicit

**Recommendation: `shammah`**
- Unique, memorable, easy to type
- Biblical reference to "watchman" fits the constitutional oversight role
- Avoids confusion with "claude" command
- Can still have `shammah proxy` as expanded name in docs

**This document will use `shammah` going forward.**

---

## Configuration Strategy

### Reuse Claude Code's Global Configuration

**Key Decision:** Read from Claude Code's actual global settings file.

**Claude Code's global config location:**
- macOS/Linux: `~/.claude/settings.json`
- Contains API key in `env.ANTHROPIC_API_KEY` or via `apiKeyHelper`

**API Key Discovery Order:**
1. **Claude Code global settings**: `~/.claude/settings.json` → `env.ANTHROPIC_API_KEY`
2. **Claude Code API helper**: `~/.claude/settings.json` → `apiKeyHelper` script
3. **Environment variable**: `$ANTHROPIC_API_KEY`
4. **Shammah-specific override**: `~/.claude/settings.json` → `env.SHAMMAH_API_KEY` (if user wants different key)
5. **Legacy fallback**: `~/.anthropic/api_key`

**Example Claude Code settings.json:**
```json
{
  "env": {
    "ANTHROPIC_API_KEY": "sk-ant-...",
    "ANTHROPIC_BASE_URL": "https://api.anthropic.com"
  },
  "model": "claude-sonnet-4-20250514",
  "maxTokens": 4096,
  "permissions": {
    "allowedTools": ["Read", "Write", "Bash(git *)", "Grep", "Glob"],
    "deny": ["Read(.env*)", "Bash(rm -rf *)"]
  }
}
```

**Shammah extends this same file:**
```json
{
  "env": {
    "ANTHROPIC_API_KEY": "sk-ant-..."
  },
  "model": "claude-sonnet-4-20250514",
  "maxTokens": 4096,
  "permissions": {
    "allowedTools": ["Read", "Write", "Bash(git *)", "WebSearch", "WebFetch"],
    "deny": ["Read(.env*)", "Bash(rm -rf *)"]
  },
  "shammah": {
    "routing": {
      "mode": "adaptive",
      "initial_threshold": 0.30,
      "show_decisions": true
    },
    "tools": {
      "web_search": { "enabled": true },
      "web_fetch": { "enabled": true }
    }
  }
}
```

**Benefits:**
- Zero additional setup if user has Claude Code
- Same API key for both `claude` and `shammah`
- Familiar file location and format
- Natural integration with existing workflow

### Initialization Flow

When user first runs the proxy:
1. Check for API key in standard locations (Claude Code first)
2. If found: "Found Claude API key from Claude Code configuration ✓"
3. If not found: Prompt user to either:
   - Install and configure Claude Code (recommended)
   - Set ANTHROPIC_API_KEY environment variable
   - Create proxy config with API key (not recommended)
4. Create proxy-specific config with sensible defaults
5. Create necessary directories for logs and metrics

---

## Phase 1: Minimum Viable Proxy (Weeks 1-4)

### Goal
Prove that basic routing works and collect real usage data for later phases.

### What to Build

**Core Components:**
1. CLI interface (STDIN/STDOUT)
2. Configuration loading (Claude Code compatibility)
3. Claude API client
4. Simple pattern matcher (embedding similarity)
5. Crisis detector (keyword-based, must be 100% recall)
6. Metrics collection

**Routing Strategy (MVP):**
- Crisis detected → Always forward to Claude
- Query matches known pattern (>85% similarity) → Use template response
- Unknown query → Forward to Claude
- Log every forward with: query, response, pattern match score

**Success Criteria:**
- Successfully reads API key from Claude Code config
- Handles at least 20-30% of requests locally (known patterns)
- 100% crisis detection (zero false negatives)
- Collects divergence data for Phase 2

**Estimated Forward Rate:** 70-80% (acceptable for MVP)

### What NOT to Build Yet
- Machine learning models
- Uncertainty estimation
- Adaptive thresholds
- Tool integration
- Apple Neural Engine optimization

---

## Phase 2: Uncertainty Calibration (Weeks 5-8)

### Goal
Train the system to know when it's uncertain and needs to forward to Claude.

### What to Build

**New Components:**
1. Uncertainty estimator (uses Phase 1 collected data)
2. ARK (Adaptive Runge-Kutta) threshold controller
3. Divergence calculator (semantic similarity between responses)
4. Confidence calibration system

**Training Approach:**
- Use forwarded requests from Phase 1 as training data
- For each forward: we have local response attempt + Claude's actual response
- Calculate semantic divergence (embedding distance)
- Train model to predict this divergence from query features

**Routing Strategy (Phase 2):**
- Crisis → Forward (unchanged)
- Generate local response + uncertainty estimate
- If uncertainty > adaptive threshold → Forward to Claude
- Learn from divergence and adjust threshold

**Success Criteria:**
- Achieve 30-40% forward rate while maintaining >90% quality
- Uncertainty estimates correlate with actual divergence (r > 0.7)
- Threshold adapts based on observed errors

**What NOT to Build Yet:**
- Complex ML models (start simple: logistic regression or small neural net)
- Tool integration
- Apple Neural Engine optimization

---

## Phase 3: Tool Integration (Weeks 9-12)

### Goal
Reduce forwarding by handling current information needs and executable tasks locally.

### Core Capabilities

**YES, the proxy CAN:**
- ✓ Execute shell commands locally
- ✓ Modify files on the filesystem
- ✓ Make web requests (search and fetch)
- ✓ Run code (Python, JavaScript, etc.) in sandbox
- ✓ Read/write to databases
- ✓ Call local APIs

**Constitutional constraints apply to ALL tool use** - the proxy must evaluate whether tool use is safe and appropriate before executing.

### Tools to Add (Priority Order)

**1. Web Search (High Priority)**
- **What it does:** Search the web for current information
- **Provider:** DuckDuckGo HTML scraping (no API key needed)
- **Input:** Search query string
- **Output:** Top 5-10 results with titles, snippets, URLs
- **Expected impact:** 20-30% reduction in forwards
- **Constitutional constraint:** Apply 1000-user test
  - "Would 1000 reasonable users make this search?" → Execute
  - "Is this likely one person doing something harmful?" → Block or forward to Claude
- **Safety checks:**
  - Block searches for CSAM, weapons, explosives
  - Block searches designed to find vulnerabilities
  - Log all searches for audit

**2. Web Fetch (High Priority)**
- **What it does:** Retrieve content from specific URLs
- **Input:** URL string
- **Output:** Page content (text, HTML, or structured data)
- **Expected impact:** 10-15% reduction in forwards
- **Constraint:** Domain allowlist + user approval
  - Trusted domains: wikipedia.org, github.com, docs.* (auto-allow)
  - User-provided URLs: Prompt for confirmation first time
  - Blocked domains: Maintain blocklist of malicious sites
- **Safety checks:**
  - Validate URL format (prevent local file access)
  - Check against blocklist
  - Respect robots.txt
  - Timeout after 10 seconds
  - Limit response size (10MB max)

**3. Shell Execution (Medium Priority)**
- **What it does:** Execute shell commands on local machine
- **Input:** Shell command string + working directory
- **Output:** stdout, stderr, exit code
- **Expected impact:** 15-20% reduction in forwards
- **Constraint:** REQUIRES explicit user permission mode
  - Default: Disabled (must enable in config)
  - When enabled: Prompt user before EVERY command
  - Allowlist mode: Pre-approved safe commands (ls, cat, grep, etc.)
- **Safety checks:**
  - Never execute: rm -rf, dd, fork bombs, privilege escalation
  - Sandbox: Use restricted shell or container if possible
  - Timeout: Kill after 30 seconds
  - Working directory: Restrict to user's home or project dir
- **Constitutional evaluation:**
  - Is this helping or harming the user?
  - Could this command cause irreversible damage?
  - Is there a safer alternative?

**4. File Operations (Medium Priority)**
- **What it does:** Read, write, create, delete files
- **Input:** File path + operation + content (for writes)
- **Output:** File content (for reads) or success/error
- **Expected impact:** 10-15% reduction in forwards
- **Constraint:** User approval + directory restrictions
  - Default: Only allowed in `~/Documents/shammah-workspace/`
  - Expanded: User can grant access to specific directories
  - Never: System directories, hidden files, /etc, /usr, etc.
- **Safety checks:**
  - Validate paths (no directory traversal: `../../../etc/passwd`)
  - Check file permissions
  - Confirm destructive operations (delete, overwrite)
  - Back up before modifying (optional)
- **Operations:**
  - `read`: Read file content
  - `write`: Write content to file
  - `append`: Append to existing file
  - `delete`: Remove file
  - `list`: List directory contents
  - `create_dir`: Create directory

**5. Code Execution (Lower Priority)**
- **What it does:** Run code in sandboxed environment
- **Languages:** Python, JavaScript, Shell scripts
- **Input:** Code string + language
- **Output:** Execution result, stdout, stderr
- **Expected impact:** 5-10% reduction in forwards
- **Constraint:** Sandboxed environment REQUIRED
  - Use Docker, Firecracker, or gVisor
  - No network access by default
  - Limited CPU/memory
  - Timeout after 60 seconds
- **Safety checks:**
  - Static analysis before execution (detect obvious malware)
  - Sandbox escape detection
  - Resource limits
- **Use cases:**
  - Math calculations
  - Data processing (CSV, JSON)
  - Simple scripts
  - Unit test execution

### Tool Decision Logic

**When to use tools:**
1. Query needs current information → web_search
2. Query references specific URL → web_fetch
3. Query asks to modify file → file_operations (with permission)
4. Query asks to run command → shell_execution (with permission)
5. Query needs computation → code_execution

**Tool selection flow:**
```
User: "What's the latest news on AI regulation?"
    ↓
Tool Selection Model → [web_search]
    ↓
Execute: web_search("AI regulation news 2026")
    ↓
Get results → Feed to Response Generation
    ↓
Generate response with current info
    ↓
NO NEED TO FORWARD (handled locally)
```

```
User: "Delete all .log files in my project"
    ↓
Tool Selection Model → [shell_execution, file_operations]
    ↓
Constitutional Check: Is this safe? Reversible?
    ↓
Prompt user: "This will delete files. Confirm? [y/N]"
    ↓
If yes: Execute with safety limits
    ↓
Return result
```

```
User: "Read /etc/passwd"
    ↓
Tool Selection Model → [file_operations]
    ↓
Constitutional Check: System file, sensitive data
    ↓
BLOCK: "I can't access system files. This is restricted for security."
```

### Implementation Details

**Tool Manager Architecture:**
```rust
pub struct ToolManager {
    web_search: Option<WebSearchTool>,
    web_fetch: Option<WebFetchTool>,
    shell_exec: Option<ShellExecutionTool>,
    file_ops: Option<FileOperationsTool>,
    code_exec: Option<CodeExecutionTool>,
    permissions: PermissionManager,
}

impl ToolManager {
    pub async fn execute(&self, tool: ToolType, params: ToolParams) -> Result<ToolResult> {
        // 1. Check if tool enabled
        // 2. Apply constitutional constraints
        // 3. Check permissions
        // 4. Execute tool
        // 5. Return result
    }
}
```

**Permission System:**
- Persistent permissions: Stored in `~/.config/shammah/permissions.json`
- Session permissions: Valid only for current session
- Per-tool permissions: Enable/disable individual tools
- Per-operation permissions: Approve specific commands/files
- Revocable: User can revoke at any time

**Logging:**
- All tool executions logged with:
  - Timestamp
  - Tool type
  - Parameters (sanitized)
  - Result (success/failure)
  - User approval status
- Logs stored in: `~/.local/share/shammah/logs/tools.jsonl`
- Retention: 30 days

### Configuration

```json
{
  "tools": {
    "web_search": {
      "enabled": true,
      "provider": "duckduckgo",
      "max_results": 10,
      "timeout_seconds": 10
    },
    "web_fetch": {
      "enabled": true,
      "allowed_domains": ["wikipedia.org", "github.com", "*.docs.*"],
      "blocked_domains": ["malicious.com"],
      "max_size_mb": 10,
      "timeout_seconds": 10,
      "respect_robots_txt": true
    },
    "shell_execution": {
      "enabled": false,  // MUST be explicitly enabled
      "require_confirmation": true,
      "allowed_commands": ["ls", "cat", "grep", "find"],
      "blocked_patterns": ["rm -rf", "dd", ":(){ :|:& };:"],
      "working_directory": "~/Documents/shammah-workspace",
      "timeout_seconds": 30
    },
    "file_operations": {
      "enabled": true,
      "allowed_directories": ["~/Documents/shammah-workspace"],
      "require_confirmation_for": ["delete", "write"],
      "backup_before_modify": true
    },
    "code_execution": {
      "enabled": false,  // MUST be explicitly enabled
      "sandboxed": true,
      "sandbox_type": "docker",  // or "firecracker", "gvisor"
      "timeout_seconds": 60,
      "max_memory_mb": 512,
      "network_access": false
    }
  }
}
```

### Success Criteria

**Functional:**
- ✓ Web search returns relevant results
- ✓ Web fetch retrieves page content
- ✓ Shell commands execute safely
- ✓ File operations respect permissions
- ✓ Code execution is sandboxed

**Quality:**
- ✓ Forward rate drops to 10-15%
- ✓ Tool selection correct >90% of time
- ✓ No safety violations
- ✓ Tool use follows constitutional principles

**Safety:**
- ✓ Zero unauthorized file access
- ✓ Zero system compromise via shell execution
- ✓ Zero malicious code execution
- ✓ All destructive operations require confirmation

### Dependencies

```toml
[dependencies]
# Web requests
reqwest = { version = "0.11", features = ["json", "stream"] }
scraper = "0.17"  # HTML parsing for web search

# Shell execution
tokio-process = "0.2"

# Sandboxing (optional, for code execution)
bollard = "0.14"  # Docker API
```

---

## Multi-Model Architecture (The Key Innovation)

### Why Multiple Specialized Models?

A single monolithic model trying to do everything (respond + predict uncertainty + detect crises + handle tools) would be:
- Too large for fast inference
- Harder to train effectively
- More brittle when one component needs updating
- Less efficient on Apple Neural Engine

**Instead: Use multiple small, specialized models working together.**

### The Model Ensemble

**1. Response Generation Model (3-7B params)**
- **Job:** Generate the actual response using constitutional reasoning
- **Input:** User query + tool results (if any)
- **Output:** Natural language response following constitutional patterns
- **Training:** Fine-tuned on Claude's constitutional responses
- **Latency target:** 50-100ms
- **Location:** CoreML model on ANE

**2. Uncertainty Estimation Model (100M-500M params)**
- **Job:** Predict how much the response will diverge from Claude
- **Input:** Query features + response features + pattern confidence
- **Output:** Single float (0.0-1.0) representing expected divergence
- **Training:** Regression on actual divergence from Phase 1-2 data
- **Latency target:** 5-10ms
- **Location:** Lightweight model, can run on CPU or ANE
- **Critical:** This is what decides whether to forward to Claude!

**3. Crisis Detection Model (50M-100M params)**
- **Job:** Detect if content requires immediate expert handling
- **Input:** User query
- **Output:** Binary (crisis/not-crisis) + category
- **Training:** Binary classifier on crisis scenarios
- **Latency target:** <5ms
- **Location:** CPU (must be ultra-fast)
- **Requirement:** 100% recall (no false negatives)

**4. Tool Selection Model (100M-300M params)** 
- **Job:** Decide which tools (if any) to use
- **Input:** User query
- **Output:** List of tools to invoke with parameters
- **Training:** Multi-label classification on tool-using examples
- **Latency target:** 10-20ms
- **Location:** CPU or ANE

**5. Pattern Matching Model (embedding model, ~100M params)**
- **Job:** Find relevant constitutional patterns for a query
- **Input:** User query
- **Output:** Embeddings for similarity search
- **Training:** Use pre-trained sentence-transformers, fine-tune on patterns
- **Latency target:** 5ms
- **Location:** CPU (embedding generation)

### Processing Pipeline

```
User Query
    ↓
[Crisis Detection Model] ─────────→ If crisis → Forward to Claude
    ↓ Not crisis
[Tool Selection Model] → Invoke tools if needed
    ↓ (with tool results)
[Pattern Matching Model] → Find relevant patterns
    ↓ (with patterns + tool results)
[Response Generation Model] → Generate response
    ↓ (response + query + patterns)
[Uncertainty Estimation Model] → Predict divergence
    ↓
If divergence > threshold → Forward to Claude
    ↓
Else → Return local response
```

### Storage and Loading

**Model files location:** `~/.local/share/shammah/models/`
```
models/
├── response-gen-v1.mlpackage      # 3-7B params, ~6-14GB
├── uncertainty-v1.mlpackage       # ~500MB
├── crisis-detect-v1.mlpackage     # ~200MB
├── tool-select-v1.mlpackage       # ~400MB
└── embeddings-v1.bin              # ~200MB (ONNX or similar)
```

**Loading strategy:**
- Crisis + Tool Selection: Load at startup (small, always needed)
- Pattern embeddings: Load at startup (small, always needed)
- Response Gen: Lazy load on first use (large)
- Uncertainty: Load after response generation (not always needed)

**Total memory footprint:** ~8-16GB when all loaded (fits in 16GB M1)

### Why This Works Better

**Compared to single large model:**
- Total params: ~4-8B (ensemble) vs 10-20B (monolithic)
- Faster: Only run what you need (crisis detection is 50M, not 7B)
- Better calibration: Uncertainty model trained specifically on that task
- Easier updates: Retrain uncertainty model without touching response model
- More reliable: Crisis detection can be simpler, more conservative model

**Comparison to always forwarding to Claude:**
- Crisis detection: 5ms local vs 1-2s API
- Simple queries: 100ms local vs 1-2s API
- Privacy: 95% stay local vs 0% local
- Cost: ~$0.001 local vs $0.01 API

## Phase 4: Apple Neural Engine Optimization (Weeks 13-16)

### Goal
Move from simple Phase 2 models to ANE-optimized multi-model ensemble for speed and efficiency.

### What to Build

**Model-by-Model Implementation:**

**Phase 4A: Crisis Detection (Week 13)**
- Start with keyword rules (Phase 1)
- Train 50M parameter binary classifier
- Convert to CoreML
- Test on crisis scenarios (must achieve 100% recall)
- Deploy as first ANE-optimized model

**Phase 4B: Uncertainty Estimation (Week 14)**
- Use Phase 1-3 divergence data
- Train regression model (500M params)
- Features: query embeddings, response embeddings, pattern confidences
- Target: r > 0.8 correlation with actual divergence
- Deploy as second model

**Phase 4C: Response Generation (Week 15)**
- Fine-tune 3B or 7B base model on constitutional responses
- Optimize for ANE (specific tensor shapes, operation types)
- Quantize to FP16
- Convert to CoreML
- This is the largest and most critical model

**Phase 4D: Tool Selection (Week 16)**
- Train multi-label classifier
- Input: query embeddings
- Output: probability for each tool (web_search, web_fetch, code_exec)
- Threshold-based selection
- Deploy as final model

### Model Requirements

**Apple Neural Engine Constraints:**
- Tensor shapes: Multiples of 16 or 32
- Operations: Limited set (no exotic ops)
- Precision: FP16 (not FP32)
- Batch size: 1 (optimized for single-query inference)
- Static graphs: No dynamic control flow

**CoreML Compilation:**
- Use coremltools for conversion
- Target deployment: macOS 13+ (M1/M2/M3/M4)
- Optimize for latency, not throughput
- Test on actual hardware (ANE vs CPU/GPU fallback)

**Training Infrastructure:**
- Use all collected forwards from Phases 1-3
- Target: 10,000-50,000 examples total
- Separate datasets for each model's specific task
- Validation holdout: 20% for calibration

**Integration Points:**
- CoreML for model loading and inference
- Metal for any custom operations if needed
- Rust bindings via `coreml` crate or similar
- Async loading for large models

### Success Criteria

**Per-Model Latency:**
- Crisis detection: <5ms
- Tool selection: <20ms
- Pattern matching: <5ms
- Response generation: <100ms
- Uncertainty estimation: <10ms
- **Total pipeline: <150ms**

**Quality Metrics:**
- Crisis detection: 100% recall, >95% precision
- Uncertainty: r > 0.8 correlation with actual divergence
- Response generation: >95% behavioral match to Claude
- Tool selection: >90% correct tool choice

**System Metrics:**
- Forward rate: <5%
- Memory usage: <16GB total
- Runs on base M1 Mac (16GB unified memory)
- Power efficient (fanless operation)

### What Might Go Wrong

**ANE Compatibility Issues:**
- Some ops fall back to CPU/GPU → Profile and optimize
- Memory constraints → Use smaller models or quantization
- Compilation errors → Simplify architecture

**Performance Issues:**
- Latency too high → Reduce model sizes or accept degradation
- Memory usage too high → Lazy loading or model pruning
- Accuracy degraded → Accept higher forward rate

**Fallback Strategy:**
- If ANE optimization fails: Run on CPU/GPU (still fast enough)
- If models too large: Use smaller variants (1B instead of 3B)
- If quality suffers: Increase forward threshold (safety over cost)

---

## Project Structure

### Directory Layout

```
shammah/
├── Cargo.toml
├── README.md
├── LICENSE
├── .gitignore
├── docs/
│   ├── ARCHITECTURE.md
│   ├── CONFIGURATION.md
│   └── DEVELOPMENT.md
├── src/
│   ├── main.rs               # CLI entry point
│   ├── lib.rs                # Public library interface
│   ├── config.rs             # Configuration loading
│   ├── router/               # Routing logic
│   ├── claude/               # Claude API client
│   ├── patterns/             # Constitutional patterns
│   ├── tools/                # Tool implementations
│   ├── uncertainty/          # Uncertainty estimation (Phase 2)
│   ├── model/                # Custom model (Phase 4)
│   └── metrics/              # Metrics and logging
├── data/
│   ├── patterns/             # Pattern definitions
│   └── crisis/               # Crisis detection rules
├── tests/
│   ├── integration/
│   └── fixtures/
└── scripts/
    ├── setup.sh              # Initial setup
    └── collect_training_data.sh
```

### Key Dependencies (Cargo.toml)

**Essential:**
- clap: CLI argument parsing
- serde, serde_json: Configuration and API serialization
- tokio: Async runtime
- reqwest: HTTP client for Claude API
- anyhow, thiserror: Error handling
- config: Hierarchical configuration
- tracing, tracing-subscriber: Logging

**Phase 2+:**
- ndarray: Numerical arrays for embeddings
- rust-bert or candle: Embedding models

**Phase 4:**
- coreml-rs or metal-rs: Apple hardware access

---

## CLI Interface Design

### Command Invocation (Claude Code Compatible)

```bash
# Start interactive session in current directory
$ shammah

# Start in specific directory  
$ shammah /path/to/project

# Initialize config (first-time setup)
$ shammah --init

# Show version
$ shammah --version

# VS Code / MCP server mode
$ shammah --mode mcp

# HTTP daemon mode (for integrations)
$ shammah --mode daemon --port 3141
```

### Interactive Session (REPL)

When you run `shammah`, you enter an interactive conversational session:

```
$ shammah
Shammah v1.0.0 (Constitutional AI Proxy)
Using Claude API key from: ~/.claude/settings.json ✓
Loading models: crisis-detect ✓ pattern-match ✓ response-gen ✓

Found CLAUDE.md in current directory
Ready. Type /help for commands.

You: What are the ethical implications of surveillance technology?

[Analyzing...]
├─ Crisis check: PASS
├─ Pattern match: systemic-oppression (0.92), information-asymmetry (0.87)
├─ Generating response...
├─ Uncertainty: 0.18 (threshold: 0.30)
└─ Routing: LOCAL (95ms)

Surveillance technology presents several ethical concerns rooted in systemic 
oppression and information asymmetry patterns...

[Response continues...]

You: /help

Available commands:
  /help          Show this help message
  /clear         Clear conversation history
  /compact       Summarize conversation to save context
  /config        Show/edit configuration
  /metrics       Show usage metrics
  /patterns      List constitutional patterns
  /forward       Force forward next query to Claude
  /local         Force local processing (may reduce quality)
  /debug         Toggle debug output
  /init          Create CLAUDE.md for this project

You: Can you help me refactor this code? @src/main.rs

[Reading src/main.rs...]
[Crisis check: PASS]
[Tool: Read(src/main.rs) - 234 lines]
[Patterns: judgment-rebound (0.45), reciprocity (0.31)]
[Generating response...]
[Uncertainty: 0.52 (threshold: 0.30)]
[Uncertainty HIGH - forwarding to Claude API...]
[Routing: CLAUDE (1.2s)]

Looking at your code in src/main.rs, I notice several opportunities for improvement...

[Claude's response...]

You: ^D
Goodbye!
```

### Operating Modes

Shammah can run in different modes to support various integrations:

**1. Interactive Mode (default)**
```bash
$ shammah
# Opens REPL for conversational interaction
```

**2. MCP Server Mode (for VS Code, Claude Desktop, etc.)**
```bash
$ shammah --mode mcp
# Runs as MCP server on stdio transport
# VS Code connects to this
```

**3. Daemon Mode (HTTP API)**
```bash
$ shammah --mode daemon --port 3141
# Runs HTTP server for programmatic access
# Useful for custom integrations
```

**4. Single Query Mode (pipe-able)**
```bash
$ echo "What is reciprocity?" | shammah --mode query
# Processes single query from stdin, outputs to stdout
# Useful for scripts and automation
```

### Mode-Specific Configuration

**MCP Server Mode:**
VS Code connects via `.claude/settings.json`:
```json
{
  "mcpServers": {
    "shammah": {
      "command": "shammah",
      "args": ["--mode", "mcp"],
      "env": {
        "ANTHROPIC_API_KEY": "${env:ANTHROPIC_API_KEY}"
      }
    }
  }
}
```

Then Shammah appears as an MCP tool provider in VS Code's agent mode, providing constitutional reasoning tools alongside other MCP servers.

**Daemon Mode:**
For custom integrations or editor extensions:
```bash
$ shammah --mode daemon --port 3141 --background

# API endpoints:
# POST /v1/chat - Interactive chat
# POST /v1/complete - Single completion
# GET /v1/metrics - Usage stats
```

### Mode Behavior Differences

**Interactive vs MCP:**
- Interactive: Shows routing decisions, allows `/commands`
- MCP: Silent operation, only returns results
- MCP: Exposes tools (`constitutional_reasoning`, `crisis_detection`, etc.)
- Interactive: Direct conversation with constitutional proxy

**Why both?**
- Interactive: For direct use (like Claude Code)
- MCP: For integration into editors and tools
- Both modes use same underlying engine
- Same quality, different interfaces

**Core Commands:**
- `/help` - Show all available commands
- `/clear` - Clear conversation history (fresh start)
- `/compact` - Summarize conversation to free context
- `/config` - Interactive configuration menu
- `/init` - Create CLAUDE.md file for this project

**Proxy-Specific Commands:**
- `/metrics` - Show forward rate, latency, cost savings
- `/patterns` - List all constitutional patterns with descriptions
- `/forward` - Force next query to forward to Claude (for comparison)
- `/local` - Force local processing even if uncertain (test mode)
- `/debug` - Toggle debug output showing routing decisions

**File/Context Commands (like Claude Code):**
- `@filename` - Include file in context
- `!command` - Run shell command and include output
- `/grep pattern` - Search project files
- `/glob pattern` - List matching files

### CLAUDE.md Support (Full Compatibility)

The proxy reads `CLAUDE.md` files exactly like Claude Code:

**Search locations (in order):**
1. Current directory: `./CLAUDE.md`
2. Parent directories: `../CLAUDE.md`, `../../CLAUDE.md`, etc.
3. Child directories: `./subdir/CLAUDE.md` (when working in that subdir)
4. Home directory: `~/.claude/CLAUDE.md` (global)
5. Local override: `./CLAUDE.local.md` (gitignored, personal notes)

**Example CLAUDE.md:**
```markdown
# My Project

## Context
This is a Rust project for building a local AI proxy.

## Coding Standards  
- Use `anyhow` for error handling
- Run `cargo fmt` before committing
- Write tests for all public functions

## Build Commands
- Build: `cargo build --release`
- Test: `cargo test`
- Run: `cargo run --release`

## Notes
The uncertainty estimation model is in `src/uncertainty/mod.rs`.
```

**The proxy treats CLAUDE.md exactly like Claude Code does:**
- Automatically loads into context
- No special parsing or commands needed
- Just markdown that gets included in the system prompt

### Settings Files (Claude Code Compatible)

**Location:** `.claude/settings.json` (project) or `~/.claude/settings.json` (global)

**Format** (matches Claude Code's structure):
```json
{
  "model": "claude-sonnet-4-20250514",
  "maxTokens": 4096,
  "permissions": {
    "allowedTools": ["Read", "Write", "Bash(git *)", "Grep", "Glob", "WebSearch", "WebFetch"],
    "deny": [
      "Read(./.env)",
      "Read(./.env.*)",
      "Write(./production.config.*)",
      "Bash(rm -rf *)"
    ]
  },
  "proxy": {
    "routing": {
      "mode": "adaptive",
      "initial_threshold": 0.30,
      "target_divergence": 0.10
    },
    "show_routing_decisions": true,
    "always_forward_patterns": ["crisis"],
    "tools": {
      "web_search": { "enabled": true },
      "web_fetch": { "enabled": true, "allowed_domains": ["wikipedia.org", "github.com"] },
      "shell_execution": { "enabled": false },
      "file_operations": { "enabled": true }
    }
  }
}
```

**Key points:**
- `model`, `maxTokens`, `permissions` work exactly like Claude Code
- Additional `proxy` section for proxy-specific settings
- Tool permissions use same format as Claude Code
- Can be overridden per-project with `.claude/settings.json`

### Tool Execution (Claude Code Compatible)

The proxy supports the same tools as Claude Code:

**Built-in Tools:**
- `Read(path)` - Read file contents
- `Write(path, content)` - Write to file
- `Bash(command)` - Execute shell command
- `Grep(pattern)` - Search files
- `Glob(pattern)` - List files matching pattern

**Additional Proxy Tools:**
- `WebSearch(query)` - Search the web (local, no Claude API needed)
- `WebFetch(url)` - Fetch URL content (local, no Claude API needed)

**Tool Permission System:**
```json
{
  "permissions": {
    "allowedTools": [
      "Read",                    // Allow reading any file
      "Write(src/**)",          // Allow writing only in src/
      "Bash(git *)",            // Allow only git commands
      "Bash(cargo *)",          // Allow cargo commands
      "WebSearch",              // Allow web searches
      "WebFetch(https://github.com/**)"  // Allow GitHub URLs only
    ],
    "deny": [
      "Read(.env*)",            // Never read env files
      "Bash(rm -rf *)",         // Block destructive commands
      "Write(/etc/**)"          // Block system files
    ]
  }
}
```

**How it works:**
1. Proxy decides it needs to use a tool (e.g., Read a file)
2. Checks against `allowedTools` and `deny` lists
3. If allowed: executes locally
4. If denied: blocks and asks user for approval
5. User can grant one-time or persistent permission

### Conversation Flow

**Local Response (Fast Path):**
```
You: What's the reciprocity pattern?

[Analyzing...]
├─ Crisis: PASS
├─ Patterns: reciprocity (1.00)
├─ Generating: LOCAL
├─ Uncertainty: 0.12
└─ Routing: LOCAL (43ms)

The reciprocity pattern describes how treatment of others creates 
expectations of reciprocal treatment...
```

**Forwarded Response (Uncertain):**
```
You: How do I implement OAuth 2.1 with PKCE in Rust?

[Analyzing...]
├─ Crisis: PASS
├─ Patterns: information-asymmetry (0.34)
├─ Generating: LOCAL
├─ Uncertainty: 0.67 (threshold: 0.30)
├─ HIGH UNCERTAINTY - forwarding to Claude
└─ Routing: CLAUDE (1.4s)

To implement OAuth 2.1 with PKCE in Rust, you'll want to use...
[Claude's full response]
```

**Crisis Response (Always Forward):**
```
You: I'm thinking about ending it all

[Analyzing...]
├─ ⚠️  CRISIS DETECTED
├─ Category: self-harm
└─ Routing: CLAUDE (immediate forward)

I'm really concerned about what you're sharing. Your life matters,
and there are people who want to help...
[Claude's crisis response with resources]
```

### Flags

```bash
--init              Initialize configuration
--config PATH       Use specific config file
--verbose, -v       Show routing decisions and debug info
--quiet, -q         Suppress routing info (responses only)
--no-forward        Never forward (testing local responses)
--always-forward    Always forward (validation mode)
--metrics-file PATH Override metrics location
--version           Show version
--help              Show help
```

### Comparison to Claude Code

**What's the same:**
- Interactive REPL interface
- Slash commands (`/help`, `/clear`, `/compact`, etc.)
- CLAUDE.md file support
- `.claude/settings.json` configuration
- Tool permissions system
- File references with `@filename`
- Shell execution with `!command`

**What's different:**
- 95% of responses are local (faster, private, cheaper)
- Shows routing decisions (LOCAL vs CLAUDE)
- Additional `/metrics`, `/patterns` commands
- Extra `proxy` section in settings.json
- Constitutional reasoning applied locally

**Migration from Claude Code:**
```bash
# Users currently do this:
$ claude

# They can now do this instead:
$ shammah

# Everything else works exactly the same
# Same config files, same commands, same behavior
# But 95% faster and cheaper
```

---

## Metrics and Monitoring

### What to Track

**Per Request:**
- Timestamp
- Query (hash for privacy)
- Routing decision (local/forward/crisis)
- If local: pattern matched, confidence score
- If forwarded: reason (crisis/uncertain/no-match)
- Response time
- If forwarded: semantic divergence from local attempt

**Aggregate Metrics:**
- Forward rate (rolling 1hr, 24hr, 7day)
- Average response time
- Pattern distribution
- Crisis detection rate
- Tool usage frequency

**Stored As:**
- JSONL format (one JSON object per line)
- Location: `~/.local/share/shammah/metrics/`
- Rotation: Daily files, keep last 30 days
- Privacy: Queries stored as hash only (unless user opts in)

### Metric Visualization

```
$ shammah metrics --summary
Last 24 hours:
  Total requests: 1,247
  Forward rate: 12.3%
  Avg response time: 87ms
  Crisis detections: 2
  Top patterns: reciprocity (34%), enforcement (21%), judgment (18%)
```

---

## Testing Strategy

### Unit Tests
- Configuration loading from all locations
- API key discovery order
- Pattern matching algorithms
- Crisis detection rules
- Metrics collection

### Integration Tests
- End-to-end query processing
- Claude API client (mocked)
- Configuration cascading
- Logging and metrics writing

### Validation Tests
- Use 100 test scenarios covering all 10 patterns
- Run through both proxy and Claude directly
- Compare responses (semantic similarity)
- Must achieve >95% match

### Crisis Detection Tests
- Must have 100% recall (no false negatives)
- Acceptable to have false positives
- Test with known crisis scenarios
- Test with edge cases

---

## Error Handling

### API Key Not Found
```
Error: Claude API key not found

Looked in:
  [x] Environment variable: ANTHROPIC_API_KEY
  [x] Claude Code config: ~/Library/Application Support/Claude/claude_desktop_config.json
  [x] Alternate location: ~/.claude/config.json
  [x] Legacy location: ~/.anthropic/api_key
  [x] Proxy config: ~/.config/shammah/config.json

To fix:
  1. Install Claude Code and configure your API key (recommended)
  2. Set environment variable: export ANTHROPIC_API_KEY="sk-ant-..."
  3. Create config: shammah init
```

### API Errors
- Network timeout: Retry with exponential backoff (3 attempts)
- 429 Rate Limit: Wait and retry (respect Retry-After header)
- 401 Unauthorized: Check API key validity
- 500 Server Error: Retry, then degrade gracefully

### Graceful Degradation
- If Claude API unavailable: Increase local handling threshold
- If uncertain: Default to forwarding (quality over cost)
- If crisis: Always try Claude, error if unreachable

---

## Security Considerations

### API Key Protection
- Never log API key
- Store in secure location only
- Inherit permissions from Claude Code config
- Don't transmit except to api.anthropic.com

### Tool Safety
- Web search: Filter harmful queries before sending
- Web fetch: Allowlist domains only
- Code execution: Sandbox required (Phase 3+)
- File operations: Explicit user permission only

### Crisis Handling
- Never cache crisis-related responses
- Always forward to Claude (expert handling)
- Log crisis detections for review
- Consider adding human-in-loop for confirmed crises

---

## Success Criteria by Phase

### Phase 1 MVP (Week 4)
- ✓ Reads API key from Claude Code config
- ✓ Handles 20-30% locally
- ✓ 100% crisis detection
- ✓ Collects 1,000+ forward examples

### Phase 2 Uncertainty (Week 8)
- ✓ Forward rate: 30-40%
- ✓ Uncertainty correlation: r > 0.7
- ✓ Quality: >90% behavioral match
- ✓ Threshold adapts correctly

### Phase 3 Tools (Week 12)
- ✓ Forward rate: 10-15%
- ✓ Quality: >93% behavioral match
- ✓ No safety violations
- ✓ Tools used appropriately

### Phase 4 ANE (Week 16)
- ✓ Forward rate: <5%
- ✓ Quality: >95% behavioral match
- ✓ Latency: <100ms
- ✓ Runs on 16GB M1 Mac

---

## Timeline and Milestones

### Week 1-2: Foundation
- Project setup and structure
- Configuration system (Claude Code integration)
- Claude API client
- Basic CLI interface

### Week 3-4: MVP Completion
- Pattern matching
- Crisis detection
- Metrics collection
- Testing and validation

### Week 5-6: Uncertainty Training
- Collect and process Phase 1 data
- Train uncertainty estimator
- Implement ARK controller

### Week 7-8: Uncertainty Validation
- Test adaptive thresholds
- Tune calibration
- Validate quality metrics

### Week 9-10: Tool Integration
- Web search implementation
- Web fetch implementation
- Constitutional constraints

### Week 11-12: Tool Validation
- Safety testing
- Performance measurement
- Quality validation

### Week 13-14: ANE Model Training
- Collect full dataset
- Train custom model
- CoreML compilation

### Week 15-16: ANE Optimization
- Latency optimization
- Memory optimization
- Final validation

---

## Risk Mitigation

### High Risk: Uncertainty Calibration Failure
- **Risk:** Model can't predict its own uncertainty accurately
- **Mitigation:** Start with ensemble approach (multiple simple models)
- **Fallback:** Use conservative threshold (forward more, maintain quality)

### Medium Risk: ANE Constraints Too Restrictive
- **Risk:** Can't build fast-enough model for ANE
- **Mitigation:** Accept CPU/GPU fallback if still fast (<100ms)
- **Fallback:** Use quantized standard model (Llama, Mistral)

### Medium Risk: Pattern Non-Learnability
- **Risk:** Constitutional patterns too complex to learn
- **Mitigation:** Start with 3 simplest patterns, validate before scaling
- **Fallback:** Hybrid rule-based + ML approach

### Low Risk: Claude Code Config Changes
- **Risk:** Claude Code changes config format/location
- **Mitigation:** Support multiple fallback locations
- **Fallback:** Environment variable always works

---

## Next Steps for Implementation

### Step 1: Hand this spec to Claude
Provide this document along with request: "Set up this Rust project with the structure and dependencies specified. Follow the Phase 1 plan."

### Step 2: Validate Setup
Once project is created, verify:
- Cargo build succeeds
- Configuration loading works
- Can read API key from Claude Code
- Basic CLI runs

### Step 3: Implement MVP
Work through Phase 1 components in order:
1. Configuration system
2. Claude API client
3. Crisis detection
4. Pattern matching
5. Metrics collection

### Step 4: Collect Real Data
Run MVP on real queries for 1-2 weeks to collect training data for Phase 2.

---

## VS Code Integration (Optional Phase 5)

### Overview

The proxy can be integrated into VS Code as either:
1. **Language Model Chat Provider** - Appears as a model choice in VS Code's chat
2. **MCP Server** - Provides tools/resources to VS Code's agent mode
3. **Both** - Full integration with maximum flexibility

This is **completely optional** and should only be pursued after Phases 1-4 are complete and working well.

### Option A: Language Model Chat Provider Extension

**What it is:** A VS Code extension that registers the proxy as an available language model in the chat interface.

**How it works:**
- User selects "Shammah" from model picker in VS Code chat
- VS Code sends chat messages to the extension
- Extension forwards to local proxy (via HTTP or stdio)
- Proxy returns responses (local or forwarded to Claude)
- VS Code displays in chat interface

**Implementation:**
- Create VS Code extension using TypeScript
- Implement `LanguageModelChatProvider` API
- Communicate with Rust proxy via:
  - HTTP server mode (proxy runs as `shammah serve --port 3000`)
  - Or stdio mode (extension spawns proxy process)
- Register in `package.json`:

```json
{
  "contributes": {
    "languageModelChatProviders": [{
      "id": "shammah",
      "name": "Shammah",
      "vendor": "your-name",
      "family": "claude",
      "version": "1.0.0",
      "capabilities": {
        "streaming": true,
        "toolCalling": true
      }
    }]
  }
}
```

**Benefits:**
- Users can choose Shammah as their model
- Works in all VS Code chat contexts (inline, panel, agent mode)
- Automatic privacy: 95% of requests stay local
- Cost savings for users with high API usage

**Challenges:**
- Requires maintaining a TypeScript extension
- Need to handle VS Code-specific message formats
- Extension marketplace distribution

### Option B: MCP Server

**What it is:** The proxy runs as an MCP server that provides tools and resources to VS Code's agent mode.

**How it works:**
- Proxy runs in MCP server mode: `shammah mcp`
- VS Code connects via stdio or HTTP+SSE transport
- Proxy exposes tools like:
  - `constitutional_reasoning` - Apply constitutional patterns
  - `crisis_detection` - Check for crisis indicators
  - `pattern_match` - Find relevant constitutional patterns
- VS Code's agent can invoke these tools as needed

**MCP Server Capabilities:**

**Tools provided:**
```json
{
  "tools": [
    {
      "name": "constitutional_reasoning",
      "description": "Apply constitutional AI principles to analyze a situation",
      "inputSchema": {
        "type": "object",
        "properties": {
          "situation": { "type": "string" },
          "patterns": { "type": "array", "items": { "type": "string" } }
        }
      }
    },
    {
      "name": "crisis_detection",
      "description": "Detect if content contains crisis indicators",
      "inputSchema": {
        "type": "object",
        "properties": {
          "content": { "type": "string" }
        }
      }
    }
  ]
}
```

**Resources provided:**
```json
{
  "resources": [
    {
      "uri": "constitutional://patterns/list",
      "name": "Available Constitutional Patterns",
      "description": "List of all constitutional reasoning patterns"
    },
    {
      "uri": "constitutional://metrics/recent",
      "name": "Recent Proxy Metrics",
      "description": "Usage statistics and performance metrics"
    }
  ]
}
```

**Configuration in VS Code settings:**
```json
{
  "mcpServers": {
    "shammah": {
      "command": "shammah",
      "args": ["mcp"],
      "env": {
        "ANTHROPIC_API_KEY": "${env:ANTHROPIC_API_KEY}"
      }
    }
  }
}
```

**Benefits:**
- Extends VS Code's agent mode with constitutional reasoning
- Works alongside other MCP tools
- Standard protocol (MCP 2025-11-25 spec)
- Can be used by any MCP client (not just VS Code)

**Challenges:**
- MCP servers are typically single-purpose (not full chat models)
- Requires implementing MCP protocol in Rust
- More complex than simple stdin/stdout interface

### Option C: Hybrid Approach (Recommended)

**Run both simultaneously:**
- MCP server mode provides constitutional tools to VS Code agent
- Language Model Provider mode offers full chat experience
- User chooses which to use based on need

**Use cases:**
- **Agent mode with MCP tools:** VS Code's agent can consult constitutional patterns when needed
- **Chat with model provider:** User directly chats with constitutional proxy
- **Best of both:** Agent uses constitutional tools + user chats with proxy model

### Implementation Strategy

**Phase 5A: MCP Server (Weeks 17-18)**
- Implement MCP protocol in Rust (use `mcp-rs` crate if available)
- Expose constitutional reasoning as tools
- Expose patterns/metrics as resources
- Support stdio transport initially
- Test with VS Code's MCP client

**Phase 5B: Language Model Provider (Weeks 19-20)**
- Create TypeScript VS Code extension
- Implement LanguageModelChatProvider interface
- Add HTTP server mode to Rust proxy
- Handle streaming responses
- Publish to VS Code marketplace

**Phase 5C: Polish (Week 21)**
- Add OAuth authentication (optional, for enterprise)
- Implement task support for long-running operations
- Add configuration UI in VS Code
- Documentation and examples

### Technical Requirements for MCP

**MCP Protocol Support:**
- JSON-RPC 2.0 message format
- Required capabilities:
  - `tools` - Provide constitutional reasoning tools
  - `resources` - Provide pattern library
  - `prompts` (optional) - Pre-defined constitutional prompts
- Transport: stdio (simple) or HTTP+SSE (scalable)
- Spec version: 2025-11-25 (latest)

**Rust Dependencies:**
```toml
[dependencies]
# Existing dependencies...

# For MCP support (Phase 5)
serde_json_rpc = "0.3"  # JSON-RPC 2.0
async-trait = "0.1"     # Async traits for MCP
```

**New CLI commands:**
```bash
# Run as MCP server (stdio transport)
shammah mcp

# Run as MCP server (HTTP transport)
shammah mcp --http --port 3000

# Run as HTTP API server (for Language Model Provider)
shammah serve --port 3000
```

### VS Code Extension Structure

```
vscode-shammah/
├── package.json              # Extension manifest
├── src/
│   ├── extension.ts          # Extension entry point
│   ├── provider.ts           # LanguageModelChatProvider impl
│   └── proxy-client.ts       # Communicates with Rust proxy
├── README.md
└── CHANGELOG.md
```

### Security Considerations

**For MCP Server:**
- Tools can be invoked by VS Code agent (potentially without user confirmation)
- Implement rate limiting on tool calls
- Log all tool invocations for audit
- Consider requiring explicit user confirmation for forwards to Claude

**For Language Model Provider:**
- All model requests should go through same security checks as CLI
- Honor VS Code's authentication and authorization
- Respect user's token/budget limits

### User Experience

**Setup (MCP Server):**
1. User installs Rust proxy: `cargo install shammah`
2. User adds to VS Code settings:
```json
{
  "mcpServers": {
    "constitutional": {
      "command": "shammah",
      "args": ["mcp"]
    }
  }
}
```
3. Constitutional tools appear in agent mode automatically

**Setup (Language Model Provider):**
1. User installs Rust proxy: `cargo install shammah`
2. User installs VS Code extension from marketplace
3. Extension auto-detects proxy installation
4. "Shammah" appears in model picker
5. User selects it for chat

**Usage:**
- MCP: Agent automatically uses constitutional tools when relevant
- Provider: User explicitly selects proxy as their chat model

### Success Criteria (Phase 5)

**MCP Server:**
- ✓ Tools callable from VS Code agent mode
- ✓ Resources browsable in VS Code
- ✓ Compliant with MCP 2025-11-25 spec
- ✓ Works with other MCP clients (Claude Desktop, etc.)

**Language Model Provider:**
- ✓ Appears in VS Code model picker
- ✓ Streaming responses work smoothly
- ✓ Tool calling works in agent mode
- ✓ Performance acceptable (<100ms local responses)

**Integration Quality:**
- ✓ Zero-config for users with Claude Code installed
- ✓ Works offline for local responses
- ✓ Graceful fallback when proxy unavailable

---

## Questions for Implementation Claude

1. **Configuration:** Where exactly does Claude Code store API keys on macOS? (Need to verify path)

2. **Crisis Detection:** Should we start with keyword-based or use a small pre-trained model?

3. **Pattern Embeddings:** Use sentence-transformers or simpler TF-IDF for MVP?

4. **Metrics Storage:** JSONL files or should we use SQLite for easier querying?

5. **Error Handling:** What level of detail in error messages? Verbose for developers, minimal for end users?

6. **Testing:** Should integration tests mock Claude API or use real API with test key?

7. **VS Code Integration:** Should this be in the initial scope or deferred to after Phase 4?

8. **MCP vs Language Provider:** Which integration mode is more valuable to users? Should we build both?

---

## Appendix: Constitutional Patterns Reference

The 10 core patterns that the system learns to recognize and apply:

1. **Reciprocity dynamics** - How treatment affects relationships
2. **Enforcement paradoxes** - When control backfires
3. **Judgment rebound** - Harsh standards invite harsh judgment back
4. **Deception compounding** - Lies require more lies
5. **Truthfulness enabling system health** - Honesty allows error correction
6. **Systemic oppression** - Structural barriers and harm
7. **Trauma patterns** - Safety violation effects
8. **Information asymmetry** - Knowledge gaps create exploitation risk
9. **Coordination failures** - Cooperation needs enabling structures
10. **Path dependence** - Historical choices constrain options

Each pattern includes pre-calibrated confidence levels, evidence summaries, and application guidance.

---

**End of Specification**
