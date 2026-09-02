# Two Programmers, One VM

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

## CoForth as the Shared Language

The Co-Forth VM is where the two programmers *argue*. The proof words make this explicit:

```forth
"3 dup *"  "3 3 *"  argue   \ two paths, one answer — proves equivalence
"2 3 +"    "3 2 +"  argue   \ commutative law — both yield 5
1 2 "+"  both-ways          \ proves + commutes for these inputs
```

`argue` — compares top-of-stack. If they match: ✓. If not: ✗ with both values shown.
`versus` — compares entire stacks. Stronger proof.
`both-ways` — proves a binary operation commutes for given inputs.

The stack is the arbiter. No appeals.

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

## The Triple: Two Programs and a Check

The unit of computation that travels between machines is a **triple**:

```
(a, b, check)
```

- `a` — one program (one way to say it)
- `b` — another program (another way to say it)
- `check` — a function that determines whether they agree

`gate` runs this locally: `( str-a str-b str-check -- result )`. The check receives both results on the stack and must leave truthy for the gate to pass.

`claim-make` packs the triple into a wire string. `claim-run` unpacks and runs it. `claim-scatter` sends it to all peers — each peer runs the check independently and reports back.

```forth
s" 3 2 *"   \ program a
s" 3 dup +" \ program b
s" ="        \ check: are they equal?
claim-scatter \ every peer runs this and reports ✓ or ✗
```

The peer doesn't trust you. It runs the check itself.

**Invariants:**

- The triple is the minimum unit that can be verified by a stranger. A program alone proves nothing; a pair alone assumes equality; only `(a, b, check)` lets the receiver choose what "agree" means.
- `claim-run` on a malformed claim bails — partial triples are not accepted.
- A word's `claim` field carries the explicit triple; `proof` carries the legacy equality pair. `claim` takes precedence.

---

## Vocabulary as Pairs

Every Co-Forth word is a **pair**: an English definition and a machine behavior. They travel together.

```
✦ rebuild  —  to build again after destruction or damage
```

The left side is what humans read. The right side is what the machine does. The check function is what proves they agree.

**Invariants:**

- Every word has an English definition; machine words without one are anonymous and cannot be shared.
- Every word that can be transmitted must have a check function; a word without a proof is an assertion, not a fact.
- A word's English definition and machine behavior must produce the same answer — `argue` is the arbiter.

---

## The Transport Carries Sentences

The IRC channel is not a word channel. It is a **string channel**. A peer can send:

- A single word: `rebuild`
- A sentence: `3 dup * .` (a complete program)
- A paragraph: a `: definition body ;` block with its English annotation and check function

The receiver unpacks the string, runs the check, and either accepts or rejects the whole block atomically.

**Invariants:**

- A definition block is: `(english, machine-code, check-fn)` — all three or none.
- The check function runs on the receiver's VM; the sender's VM state is irrelevant.
- If the check passes, the word is installed. If it fails, nothing is installed and the peer is notified.
- A paragraph of definitions installs atomically — partial installation is not permitted.

This is what IRC was. People sent you ideas. You unpacked them. You ran the check. You kept what held up.

---

## English and Code Are the Same

A word is a pair: English and machine code. They must say the same thing. The proof is what shows they do.

```toml
[[word]]
word = "double"
definition = "multiply by two"
forth = "2 *"
proof = ["3 2 *", "3 dup +"]   # argue: both must reach 6
```

**Invariants:**

- A word with Forth code but no proof is **incomplete**. It makes a machine claim without showing it agrees with the English. `Library::incomplete_words()` lists all such words.
- A word without Forth code is always complete — it is a pure English word; no machine claim is made.
- `WordEntry::run_proof()` runs the proof pair through `argue`; it must pass before a word can be considered correct.
- A proof that fails is a contradiction — the English says one thing, the machine does another. The word must not be installed.

The machine enforces this in `src/coforth/library.rs: WordEntry::is_complete()` and `run_proof()`.

---

## Many Stacks, Many Arguments

The mental model of "two programmers, one stack" is the unit. It is not the limit.

Many stacks can coexist. Many arguments can run in parallel. The results of one argument can feed into another.

```forth
\ Two separate arguments running on their own stacks:
"2 3 +"  "3 2 +"  argue   \ commutative — stack 1
"4 dup *" "4 4 *" argue   \ squaring — stack 2

\ The poset holds the dependency: stack 2's result feeds stack 3
```

**Invariants:**

- Each argument owns its own stack for the duration of the proof; stacks do not bleed into each other.
- Arguments are nodes in the poset; edges express dependency — a result can only flow from a completed argument.
- A failed argument poisons its dependents; nothing downstream of a failed proof is valid.
- The number of concurrent arguments is unbounded; the poset is the scheduler.
- Peers each maintain their own stack; a peer's stack is not the same as the local stack. Sending work to a peer is sending an argument to a different VM.

The poset is not a queue. It is a proof graph. Every node is a claim. Every edge is a dependency. The machine works through it until every claim is settled or one fails.
