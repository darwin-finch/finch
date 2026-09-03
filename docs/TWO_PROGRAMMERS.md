# Two Programmers, One VM

> **Removed, 2026-09.** Six sections of this document described an
> "English as Forth" model: a vocabulary where each English and Chinese word
> carried a Forth body, `argue`/`both-ways`/`gate` proof words, a
> `(a, b, check)` claim triple, and an IRC string transport. None of it
> survived contact — of 43 vocabulary bodies none defined a word, of 476 real
> definitions none linked against the typed VM, and no user input could reach
> the interpreter. The code went in #294, #298 and #304; those sections went
> with it. The original is preserved on `archive/word-seed-vocabulary`.
>
> What remains below is the part that is still true: two programmers sharing
> one typed VM, where a definition's stack signature is what settles a
> disagreement.

## The Mental Model

Finch is designed around a simple image: **two programmers sitting at the same terminal, arguing over a shared Forth VM**.

One programmer is the human. The other is the LLM. Both can read the stack, define words, push values, and run programs. Both can see each other's output in the scrollback. The stack settles disagreements — if two programs produce the same stack, the argument is over.

The relationship is **asymmetric**. The human is the owner of the session; the LLM is a peer with restricted authority.

---

## Shared State

Both programmers operate on the same VM instance:

| State | Description |
|-------|-------------|
| **Data stack** | The primary argument register |
| **Vocabulary** | All defined words (built-ins + STDLIB + user-defined) |
| **Rooms** | Named key-value spaces shared across peers |
| **Hash** | Key-value store within the current room (`h!`, `h@`) |
| **Ensembles** | Named groups of remote peer addresses |
| **Source log** | Every `: word body ;` definition, for session replay |

---

## Asymmetric Abilities

### Human (Owner)

- Unrestricted tool access (read, write, edit, bash, glob, grep, web)
- Can define and redefine any word
- Can cancel any operation (Ctrl+C)
- Can switch providers, models, and modes (`/provider`, `/model`, `/plan`)
- Can approve or reject LLM-proposed file changes
- Can restart the process (`/quit`, Ctrl+D)

### LLM (Peer)

| Tool class | Behavior |
|------------|----------|
| `read`, `glob`, `grep` | Silently allowed — no confirmation dialog |
| `write`, `edit`, `patch` | Surfaces as a **diff proposal** in the scrollback; human must accept/reject |
| `bash` (read-only: `ls`, `cat`, `find`) | Silently allowed |
| `bash` with side effects (`rm`, `git commit`, `mkdir`) | Requires human approval |
| `restart`, `spawn`, `kill` | **Hard blocked** — peer can never restart or spawn processes |
| Constitutional limits (`rm -rf`, `/etc/passwd`, etc.) | **Hard blocked** for both owner and peer |

See `src/tools/permissions.rs` for the implementation. Key invariants are tested there:
- `test_peer_cannot_restart`
- `test_peer_cannot_spawn`
- `test_peer_read_glob_grep_silently_allowed`
- `test_peer_write_edit_patch_surfaces_as_ask`
- `test_peer_constitutional_constraints_still_apply`

---

## Commands

### Human-only commands (slash commands)

| Command | Effect |
|---------|--------|
| `/plan` | Enter planning mode — LLM reads only, presents a plan for approval |
| `/approve` | Approve the current plan and enter execution mode |
| `/provider <name>` | Switch AI provider |
| `/model <name>` | Switch model |
| `/join <room>` | Join a CoForth room |
| `/part <room>` | Leave a room |
| `/graph` | Show execution graph for the last query |
| `/quit` | Exit |

### LLM-issued tools (via API tool calls)

| Tool | What it does |
|------|--------------|
| `EnterPlanMode` | Request to enter planning mode (requires human confirmation) |
| `PresentPlan` | Show the completed plan and wait for human approval |
| `AskUserQuestion` | Show a dialog and wait for a human answer |
| `Read`, `Glob`, `Grep` | Inspect files silently |
| `Write`, `Edit` | Propose a file change (diff shown; human approves) |
| `Bash` | Run a command (read-only: silent; side-effect: confirmation required) |
| `WebFetch` | Fetch a URL |
| `TodoCreate/Update` | Manage the session task list |

---

## Stack-Effect Proofs

Built-in word contracts are machine-checked by the typed VM's stack-effect
signatures in `src/vm`. A definition declares its effect and the verifier
enforces it:

```forth
: square ( S int -- S int ) dup * ;
```

The signature is the proof. A definition whose body does not match it is
rejected before it runs, rather than tested after.

This section previously described an `enum Builtin` in `src/coforth/interpreter.rs`
with a `mod stack_effects` test module. No user input could reach that
interpreter and #294 removed it; the typed VM is what runs `--forth`, `--lisp`,
`--exec` and `/forth`.

---

## Session Modes

| Mode | Who controls tools |
|------|--------------------|
| `Normal` | All tools available (with per-tool confirmation rules) |
| `Planning` | LLM restricted to read/glob/grep/web_fetch only; must call `PresentPlan` to exit |
| `Executing` | All tools enabled; plan being carried out |

Mode transitions require explicit human action — the LLM cannot unilaterally lock into or exit planning mode without a confirmation dialog.

---
