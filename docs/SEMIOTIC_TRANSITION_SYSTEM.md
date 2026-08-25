# Semiotic Transition System

Status: parked research note with no active implementation TODO. Revisit only after Finch's current
Lisp/Co-Forth product is production-ready and benchmarked. It is not part of Finch's trusted program
wire, Brain runtime, boot sequence, or current language roadmap.

## Motivation

Human utterances are programs in the broad sense that reading them changes an interpreter's state:
expectations, references, commitments, available actions, and the interpretation of later symbols.
A dictionary entry describes a word with more words, but does not formally specify those state
changes. This experiment asks whether a useful core of natural language can instead be distilled
into executable, inspectable transition rules.

The intended payoff is not to replace a language model with a brittle dictionary. Coherent regions
of language should execute cheaply through already-converged rules; inference should remain at the
boundary where a use is novel, ambiguous, contradictory, or under-specified. Repeated successful
inferences can then be proposed as new versioned rules. In that sense the system is a semantic
partial evaluator: execute the known core and infer only the residual holes.

## Proposed machine model

The experimental frontend is a token-stream pushdown transducer, not postfix Co-Forth and not a
batch `ProgramSubmission` parser. A transition is conceptually:

```text
(token, parser frames, discourse state, assumptions)
    -> possible transitions, observations, and effects
```

Every token is handled by the current parser-frame stack. A handler may:

- update discourse or parser state;
- push, replace, suspend, or complete a parser frame;
- emit a typed observation or proposed action;
- retain a continuation whose resolution depends on following tokens;
- constrain the possible meanings of earlier or later symbols.

This makes prefix stream behavior such as `SAY #channel "Hello"` materially different from postfix
Co-Forth's `"Hello" say`. In the former, `SAY` installs the receiving state before the destination
and payload arrive. The two systems may share typed values, vocabulary metadata, host-effect
descriptors, and selected IR operations, but they do not share parsing semantics.

## Meaning as constrained behavior

A word's meaning is not one stack effect in isolation. It is a versioned family of context-sensitive
transitions constrained by observed uses. Examples, paraphrases, contrasts, entailments, and
successful interactions provide constraints. Claims such as:

```text
"The word was God."
"God was the word."
```

may support an equivalence in a particular formal context, but do not prove unrestricted synonymy.
Natural language is underdetermined: deixis, pragmatics, metaphor, social context, and world
knowledge prevent a finite corpus from establishing one globally correct definition. Accordingly,
the system may prove only relative statements such as:

```text
Given corpus C, assumptions A, context class K, and transition theory T,
rules R1 and R2 are observationally equivalent for observations O.
```

Each proposed rule therefore needs provenance, scope, confidence, counterexamples, and a stable
version. Inference can propose a meaning; it must not silently become the trusted parser for a
previously defined transition.

## Programmable definitions

A definition should eventually be able to carry more than executable behavior:

- token/frame transition behavior;
- accepted context and input-state predicates;
- resulting state and observations;
- documentation and examples;
- equivalence or refinement claims;
- proofs or executable checks;
- provenance, confidence, and known counterexamples.

Unknown-symbol handling must be explicit policy. It may quote the symbol, ask for clarification,
delegate an inference, or reject it, but an unknown token must not accidentally acquire ambient
authority or execute an invented host operation.

## Experiments

Keep the initial work deliberately separate and falsifiable:

1. Define a tiny typed parser-frame protocol with no host effects.
2. Encode a small controlled-language corpus by hand.
3. Test whether independently authored utterances converge on reusable transition rules.
4. Measure how often distilled rules replace model inference without changing observed behavior.
5. Record ambiguities and counterexamples rather than forcing premature convergence.
6. Compare inferred rules with held-out human judgments and task outcomes.
7. Only then test collaborative LLM proposals, reviewed before entering a versioned corpus.

Useful measurements include inference calls avoided, rule coverage, conflicting-rule frequency,
repair rate, held-out agreement, transition cost, and the size of the residual prompt needed for
unresolved cases.

## Safety and product boundary

- Finch Lisp and typed Co-Forth remain the current model-facing execution languages.
- Brain convergence must not depend on this research.
- The corpus is optional reviewed data, never ambient boot poetry or an automatic vocabulary
  mutation mechanism.
- Natural-language observations do not grant filesystem, process, network, credential, approval,
  or Brain-control authority.
- Any bridge into Finch host effects must pass through the ordinary typed capability verifier and
  approval policy.
- The legacy semiotic Co-Forth interpreter is historical implementation, not the foundation of this
  experiment; reuse requires a separate technical evaluation.

## Open questions

- What is the smallest useful discourse state and parser-frame interface?
- Which observations define contextual equivalence without smuggling an LLM judgment into every
  proof?
- How should ambiguity remain first-class rather than being collapsed to one transition?
- Can rules be composed and minimized while retaining provenance and counterexamples?
- Which fragments of English are regular enough to distill profitably?
- Can a model emit reviewed "source code" for those fragments that generalizes beyond its training
  examples?
