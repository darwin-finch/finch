# finch

A terminal AI coding assistant with persistent memory and tool use.

## What it does

- Answers coding questions, reads your files, runs commands, and searches your codebase — with your permission before every action
- Remembers context across sessions using a hierarchical memory tree (MemTree), automatically injected into each conversation
- Works with any of the major AI providers: Grok, Claude, GPT-4, Gemini, Mistral, Groq
- Optionally runs a local model (Qwen 2.5) on your machine for offline use

## Quick Start

The fastest way to get started is with `--cloud-only` and a Grok API key. X Premium+ subscribers get free Grok API access at [console.x.ai](https://console.x.ai) — no credit card required.

### 1. Get a Grok API key

Sign in at [console.x.ai](https://console.x.ai) and create an API key.

### 2. Install the binary

**Apple Silicon (M1/M2/M3/M4):**
```bash
curl -L https://github.com/darwin-finch/finch/releases/latest/download/finch-macos-arm64.tar.gz | tar xz
sudo mv finch /usr/local/bin/finch
```

**Intel Mac:**
```bash
curl -L https://github.com/darwin-finch/finch/releases/latest/download/finch-macos-x86_64.tar.gz | tar xz
sudo mv finch /usr/local/bin/finch
```

**Linux (x86_64):**
```bash
curl -L https://github.com/darwin-finch/finch/releases/latest/download/finch-linux-x86_64.tar.gz | tar xz
sudo mv finch /usr/local/bin/finch
```

**macOS quarantine note:** macOS may block the binary because it is not code-signed yet. If you see a security warning, run:
```bash
xattr -dr com.apple.quarantine /usr/local/bin/finch
```

### 3. Run setup

```bash
finch setup
```

The interactive wizard will ask for your API key and configure `~/.finch/config.toml`.

### 4. Start finch

```bash
finch --cloud-only
```

`--cloud-only` skips the local model entirely and routes all queries to your configured provider. No model download needed.

---

## What you can ask it

Ask questions in plain English. finch has access to tools and will ask your permission before using them.

**Read and explain code:**
```
> Read src/main.rs and explain what the startup sequence does
```

**Find things in your codebase:**
```
> Find all uses of unwrap() in Rust files and list the file names
```

**Run your tests:**
```
> Run cargo test and tell me which tests failed
```

**Get documentation:**
```
> Fetch the tokio docs for spawn_blocking and show me an example
```

---

## Local model (offline use)

If you want finch to run without any cloud provider, it can download and run a Qwen 2.5 model locally via ONNX Runtime. The model is selected automatically based on your available RAM:

| RAM    | Model  | Download size |
|--------|--------|---------------|
| 8 GB   | 1.5B   | ~1.5 GB       |
| 16 GB  | 3B     | ~3 GB         |
| 32 GB  | 7B     | ~7 GB         |
| 64 GB+ | 14B    | ~14 GB        |

The download happens in the background on first run. On Apple Silicon, inference uses Metal acceleration via CoreML.

To use the local model, run `finch` without `--cloud-only`. The REPL starts immediately; queries fall back to your cloud provider while the model loads.

---

## Commands reference

| Command / Key       | What it does                                           |
|---------------------|--------------------------------------------------------|
| `finch`             | Start the interactive REPL (with local model if ready) |
| `finch setup`       | Run the interactive setup wizard                       |
| `finch --cloud-only`| Start REPL using only cloud providers, no local model  |
| `/teacher grok`     | Switch to Grok for the current session                 |
| `/teacher claude`   | Switch to Claude for the current session               |
| `Ctrl+C`            | Cancel the current query                               |
| `Ctrl+G`            | Mark the last response as good (training signal)       |
| `Ctrl+B`            | Mark the last response as bad (training signal)        |

---

## Privacy

- All configuration is stored locally at `~/.finch/config.toml`
- Conversation memory is stored locally at `~/.finch/memory.db` (SQLite)
- No account required, no telemetry, no cloud sync
- When using a cloud provider, your queries are sent to that provider's API under your own API key
- When using the local model, nothing leaves your machine

---

## Build from source

Requires Rust 1.70 or later.

```bash
git clone https://github.com/darwin-finch/finch
cd finch
cargo build --release
./target/release/finch --version
```

---

## License

MIT OR Apache-2.0
