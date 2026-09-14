# Model cost, routing, and review loops

Design intent. Not current fact. Related: [router](../src/router/ROUTING.md),
[context assembly](../src/context/ASSEMBLY.md), [ticketing plugins](TICKETING_PLUGINS.md),
Pyramid PYR-777 (independent reviewer members).

The git-to-Jira "smart commit" bus is covered in ticketing plugins. This note is
the other half of that research dump: flagship models, subscription caps,
routers, and prompt caching — as they apply to *this* runtime, not to a generic
Codex write-up.

## What is already true here

Finch is supposed to be the harness so you stop hopping ChatGPT / Claude / Grok
UIs when a cap hits. One daemon, many providers, `/provider` to swap. Token
burn from `/plan` (IMPCPD, seven personas, live) and from implement-then-review
loops is real. Local Qwen/ONNX is already the cheap path for compression; cloud
is the coding/review path.

Do not take third-party summaries of Finch as inventory. `/plan` is not "six
keyword personas." Context assembly loads AGENTS.md and kin into the system
prompt (`src/context/ASSEMBLY.md`); `finch query` / `finch agent` / local
generators still do not get that (#77).

Finch is **not yet a daily harness**. ChatGPT device OAuth exists. Claude
subscription OAuth is [#199](https://github.com/darwin-finch/finch/issues/199)
and not done. Grok and other subscriptions are API keys only. Until those
logins feel like the vendor apps, people will keep switching UIs when a cap
hits, which is the problem this runtime exists to end.

## The provider/credential matrix is the UX tax

Claude Code, ChatGPT, and Grok each have **one** account and a model picker.
Finch has many named `[[providers]]`. `/provider` already switches those names.
The remaining tax is model and thinking level *inside* a name, and which name
fills `compress` / `review` when the user never set it.

**Lanes hang on a `[[providers]]` entry**, they are not a fourth object.
`ProviderEntry` already has `name`, `model`, and (on some types) `reasoning_effort`.
Add a `roles` list (`compress`, `cheap`, `review`, `default`). Heuristics fill
that from the stable `type` tag, not from the user's display name:

- `type = "local"` (Qwen/ONNX/Candle) → `compress` (and `cheap` if nothing else is)
- `type = "grok"` → `cheap` / implement default
- `type = "claude"` → `review` if no review role exists yet
- first enabled cloud entry → `default` work lane if unset

The user can override. `/provider` already switches by **name**; keep that.
Within one named entry, same backend and same credentials: `/model` and
`/effort` (thinking level) are cheap controls
([#217](https://github.com/darwin-finch/finch/issues/217),
[#338](https://github.com/darwin-finch/finch/issues/338)). Cap failover
([#450](https://github.com/darwin-finch/finch/issues/450)) offers the next
**named provider** (or the next entry that has the needed role), not a
credentials form.

**Cache:** if the model id does not change, the conversation prefix is already
stable and providers' automatic prefix cache works. Compression (#707) is for
**crossing providers** (or a model id that would miss the prefix), not for
toggling effort on the same model.

## Routing: packet role, not a second LLM

An LLM router that embeds the prompt to pick GPT vs Claude is optional later
and easy to get wrong. For this product the signal is already in the **claim**:

| Phase | Who | Model class |
|-------|-----|-------------|
| Implement | named implementer member | fast/cheap capable (Grok-class, mid Claude) |
| Lint / tests | same claim, tool results | local or cheapest cloud |
| Review | **different** member, cited review skill | stronger model, not the implementer |
| Accept | human, morning QA | not a model |

That is Pyramid PYR-777 plus Finch **lanes**, not RouteLLM. The router does not
need to understand the code. It needs to know whether this Brain is
implementing or reviewing. A classifier that reads "please find bugs" and
upgrades the model is a fallback for chat, not the factory.

Do not use one flagship for every persona in `/plan`. Worker vs critic vs
final arbiter can be three lanes. Models that obsessively re-open working
code when told to "check again" should not own the inner loop.

## Prompt caching: later, and only with a frozen prefix

Skepticism is correct. Cache hits require a **byte-identical leading prefix**.
A new adversarial reviewer with only the diff is a cache miss. Appending the
new patch in the *middle* of the repo dump is a miss. A maker that rewrites
three files and rebuilds "here is the tree" is a miss.

If we do it:

- Static prefix: system instructions + **base-revision** tree (or AGENTS.md +
  pinned files), cache breakpoint, then append-only task/diff/test log.
- Independent reviewers reuse that **same** prefix string, then a role override
  and the candidate at the tail (masked global context). They still must not be
  the implementer member.
- TTL is provider-specific (minutes to an hour). A swarm that starts cold after
  lunch pays full price again.
- This is a **specialized review harness** concern, not a Finch v1 blocker.
  Building Finch while also building a cache compiler is how the runtime stays
  unusable.

Subscriptions vs API: the daemon already speaks provider HTTP. Paying ChatGPT
Pro / Claude Max and pasting between UIs is the expensive path. One API wallet
(native keys or OpenRouter) on the daemon is the product. Prompt caching is an
optimization on that path, not a reason to delay it. Subscription **OAuth**
(Claude #199, Grok still missing) is what makes "one harness" true; API keys
alone keep you in the vendor apps for the quota you already paid.

## Mid-conversation provider switch: compress, then continue

Dumping a 1M-token Brain log at a new provider is how a cap-failover costs more
than the cap. The Brain's durable log stays complete. What the *new* model
gets is a **compression** plus a recent tail — the same idea as Pyramid's
cited summaries, not a second conversation.

Do that compression on a reserved **lane role**, not a model name. Users name
providers whatever they want; Finch must not grep for `qwen`.

| Role | Default bind | Job |
|------|----------------|-----|
| `compress` | bundled local Qwen (ONNX/Candle) if weights exist | summarize Brain log → compact handoff |
| `cheap` / `review` / … | user | actual work |

Setup assigns `compress` once (default: local Qwen; else cheapest configured
cloud; else refuse and send only the last N turns with a warning). Hot-swap
(#450) **to a different provider** then: run `compress` on the log → attach
summary + tail → new entry. Same provider, new model/effort: skip compress;
prefix cache should still hit. The summary is an event on the Brain, so you
can see what was dropped.

Do not use the outgoing flagship to summarize "random back and forth." That
defeats the point. Local Qwen shipped by default is how this stays free when
the user has no API.

## What not to do now

- Semantic embedding router for every turn.
- Putting Opus-class models on "review until no findings" in one session.
- Treating cache as a substitute for warm runtime / cold claim (Pyramid:
  durable Finch, clean task context).
- Git hooks that move Jira columns (ticketing plugins).
- Exposing the raw provider/credential/model/effort matrix as the Brain
  switcher.
- Selecting the compressor by provider **name** (`qwen`, `luna`, …). Heuristics
  use `type`.
- Replaying the full Brain log when the **provider** changes. Same-model
  effort/model tweaks should not compress.
