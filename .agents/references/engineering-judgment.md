# Engineering judgment

Agents take the shortest path that satisfies the ticket. That path is usually a local edit and a green test. It is the right default for a typo. It is the wrong default for a data model, a module boundary, or a bug sitting next to the feature.

This page is a set of questions, not commandments. Load it when writing a [solution contract](solution-contract.md), when [reviewing](../skills/review/SKILL.md), and when [auditing the tree](../skills/codebase-audit/SKILL.md). A principle that is not on those paths does not run.

## Cost of reversal

Ask, of every durable decision: **if this is wrong in six months, is the fix a local edit or a migration?**

Local edit: rename a function, split a file, add a branch, inject a dependency at a call site. Cheap. Do it when tension appears, not before.

Migration: change what a row means, what a key is, who owns a record, how many of a thing exist, what a public payload contains, what an event log will replay. Expensive, error-prone, and they accumulate compatibility layers that nobody deletes. Spend the extra thinking now.

That is the whole distinction between a hole and an abstraction.

## Holes, not frameworks

A **hole** is a shape in the durable record (schema, identity, ownership, cardinality, public format) that lets a *known* next feature land as an addition. You are not implementing the feature. You are refusing to paint over the place it will attach.

A **framework** is code for a caller you do not have: an adapter interface around the one database you use, a plugin registry with one plugin, a `Manager` whose only job is to look like architecture. [The rule of three](https://holdenrehg.com/blog/2021-09-20_rule-of-three) is the warning sign — after you have built the similar thing three times, you *may* be looking at a pattern. Until then, duplication is cheaper than the wrong interface.

| Leave the hole | Wait for tension |
| --- | --- |
| Identity is a typed id, not a display string you will later need to rename | Database adapter interface for the one database |
| A relationship you already talk about as many-to-many is not flattened into a flag | Event bus for two functions in the same module |
| A domain with a third known state is not a boolean | Strategy object for the only algorithm |
| User-owned data lives where permissions can later attach | Generic `Handler` / `Service` / `Manager` wrapping one call |
| A public payload has an extension point you will not have to version-break | Extracting a shared helper from a single call site "for reuse" |

The known next feature has to be actually known: written in a ticket, a design, or the contract's non-goals. "We might want plugins someday" is not a hole. "Invites are in the milestone after this one, so membership is not a boolean on the user row" is a hole.

## Compression must not flatten a seam

The implementation loop compresses before review: remove unused adapters, mirroring tests, compatibility layers with no owner and no removal trigger. That pass is for *code you do not need*.

It is not permission to collapse a boundary that exists so the next feature does not rewrite the model. If the contract named a seam, the compression pass leaves it. If the seam has no owner, no successor, and no removal trigger, it is not a seam — it is leftover scaffolding and the pass should delete it.

## Bugs before features

A known defect in the same area outranks a feature that touches it, unless the feature *is* the fix. This is a queue rule, recorded on the item, not a reason to swell the current diff with unrelated repairs.

Incidental dead code found along the way becomes its own deletion ticket, naming the exact code and the evidence that nothing reaches it. It does not ride along.

## Required stack vs boy scout

If the **feature requires a refactor or a migration**, that is not one implementation. It is a stack of landings, still one outcome:

1. Tests that pin current behavior (so the refactor cannot cheat).
2. The refactor and/or migration.
3. The feature.

Each step is its own candidate, its own review, its own squash. The parent ticket stays open until the stack is on `main`. "Make the change easy, then make the easy change." `$implement` that discovers it needs a migration in the same diff **stops and splits**; it does not mix steps 2 and 3.

**Boy scout** is different: leave the campground better when the feature does *not* need that cleanup to ship. File it for later. Do not "while I'm here" into the feature landing. Review that finds campground work files a ticket; review that finds a *required* refactor splits a stacked predecessor.

## Chunkability (Miller's seven)

A reader — human or model — holds about seven chunks in working memory. Structure code so each layer is loadable as a handful of independent pieces. This is not numerology; it is why [unwieldy functions accrete](https://schancel.github.io/2019-07-14-software-and-magic-number-seven.html) from progressive addition.

Ask:

- Can this function's arguments and locals be held at once, or is it a bag of flags that wants a small options type?
- Does this function have more stanzas than a reader can collapse, and do those stanzas have a linear data flow that would make them real helpers?
- Does this module require opening its dependencies' *implementations*, or do their interfaces suffice?
- Are two pieces coupled through a global or a shared mutable bag so they cannot be thought about separately?

Apply this where it improves local reasoning. Do not invent hierarchy for a twenty-line script, and do not funnel unrelated APIs into a generic `contracts` module so the org chart looks clean.

The module-boundary version of the same idea: an agent changing one module should need that module, its own instructions, and the *interfaces* of what it depends on — never their implementations.

## Single home, single direction

- A fact has one owner. A second representation needs a stated derived-from relationship, or it will drift.
- Dependencies point one way. If A and B both know each other's internals, they are one module wearing two directories.
- Encapsulation is the ability to change a representation without asking callers to change. If callers already reach through, there is no boundary to preserve — say so, or put one in.

Dependency injection, separation of concerns, and "keep it simple" are names for these questions. They are not a checklist. A constructor that takes the one database the process has is fine. A constructor that takes an interface "so we can test" is fine when the test would otherwise need the world; it is not fine when the interface exists only to look SOLID.

Do not treat SOLID or DRY as a per-diff checklist. [The wrong abstraction is worse than duplication](https://sandimetz.com/blog/2016/1/20/the-wrong-abstraction) — three calls to a bad helper are not the rule of three.

## States, names, and the edge

Ask, when the diff invents a type, a name, or a check:

- **Can this value be nonsense?** A boolean pair that can be `true, true`, a string that is "really" an id, a status plus a flag that repeats the status. Prefer a type that cannot say that. This is the hole in type form: make illegal states unrepresentable.
- **Is the name a job title with no job?** `Manager`, `Utils`, `Helper`, `data`, `Handler` wrapping one call. The missing module is usually nearby; the name is covering it.
- **Is this checked at the edge once, or all the way down?** Parse (or reject) at the boundary; the inside of the module should see a typed value, not re-validate a string. Scattered `if !valid` is not a hole, it is fear.
- **Are we faster in theory?** Caches, pools, thread locals, and "hot path" rewrites without a measured problem are frameworks. Wait.

A public payload, a stored row, and a log line are promises. Compatibility is cost of reversal, already above.

**Postel's law applies only to external inputs** (the wire, a file we did not write, a peer we do not control): be conservative in what you *send*; be robust in what you *accept* so a slightly-wrong peer does not knock you over. After that parse, stop being liberal. Tolerant readers *inside* the module hide broken writers. Do not Postel your own database, your own logs, or another module in-process.

## What a contract should say that a ticket usually does not

When the change touches a durable record, the [solution contract](solution-contract.md) names:

- the current shape and the target shape;
- the known next feature that the target shape must not paint over;
- what is *not* being built (the framework you are refusing);
- the proof that the hole still exists (a test, a type, a schema comment, a rejected alternative).

When the change does not touch a durable record, do not invent this section.

## What review should catch that tests will not

The [review protocol](review-protocol.md) includes a **shape** perspective, selected when the diff touches models, persistence, public formats, or module boundaries. That perspective answers the questions on this page against the actual diff. Architectural taste without a reversal cost is `PLAUSIBLE` at most, and usually a follow-up, not a blocker.

Confirmed shape findings look like: "this boolean will have to become an enum when ticket #N lands, and #N is already accepted"; "this string is the only copy of an identity that two tables now store"; "callers import the implementation module, so the facade is fiction."

## What an audit should catch that a ticket will not

Feature work never asks whether a module is still reached. A bugfix never sees the third copy of a helper. The [audit skill](../skills/codebase-audit/SKILL.md) walks the tree looking for exactly those: dead code with evidence, patterns that have now happened three times, modules that cannot be loaded in bounded context, seams that were promised and then filled in.

Audit output is tickets, not a refactor in place.
