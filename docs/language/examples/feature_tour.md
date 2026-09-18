# Feature tour — small programs, real composition

Working CoLisp sketches, each exercising a cluster of features together rather than in isolation.
Purpose: find places where two independently-specified features don't actually compose. Where a
program needed something the spec doesn't define yet, that's called out inline as **GAP**, not
guessed at. Where a piece of syntax below isn't actually confirmed against an established example
elsewhere in the document, it's marked **UNVERIFIED** rather than presented as settled.

**Corrected 2026-09-17, whole file:** the first pass of every example here used an invented `fn`
header, `->` for a return type, and `=>` before a body — none of which are CoLisp. Caught when
Shammah asked directly whether Finch even uses that arrow notation. The real, already-established
form (from `square`/`save-report` in "Functions and annotations") is
`(define (name (params...)) : ReturnType ! effects body...)` — `define`, return type as `: T` after
the closing parenthesis of the parameter list, and no separator before the body at all, since in an
S-expression the body is simply whatever forms remain. Record construction was also wrong in the
first pass — `Foo { x: a, y: b }` is Co-Forth's spelling; CoLisp's is `(Foo :x a :y b)` per the
parity ledger. Rewritten throughout below.

**Updated 2026-09-17: `let` bindings now use `[...]`, not doubled `(( ))`.** `[n 10]` for one
binding, `[a 1 b 2]` for several — flat, Clojure-style, never nested pairs. `[...]`'s grammar is a
strict superset of the JSON-array syntax it already handled, so this needed no new bracket and
changes nothing about existing JSON literals — see the "binding lists... use `[...]`" addition in
`FINCH_LANGUAGE_DESIGN.md`.

## 1. Records, construction, properties

```lisp
(record Account
  pub id: string
  balance: int)                       ; module-private; no plain field named `balance` outside this module

(implementation Account
  (constructor (open (id : string)) : Account
    (Account :id id :balance 0))

  (get (balance (self)) : int
    (. self balance)))
    ; `self` is an ordinary parameter — unannotated defaults to `borrow`, same as every other
    ; parameter. No new receiver notation needed for a read-only accessor.

(let [a (Account.open "acct-1")]
  (assert-eq (. a balance) 0))   ; `get balance` is defined to resolve through `.` exactly like a plain field
```

**GAP, not sketched with an invented primitive:** `set balance` cannot be written yet. Its receiver
needs the exclusive mutable borrow this document already flags as unnamed (`borrow-mut`), and even
with that, writing the field in place needs a second primitive that was never named either —
`record-set` is explicitly functional/copying, and "mutation only through typed references with
explicit `vm.write` effects" is stated as a goal, never given a surface form. Both belong together as
one open item: a mutable-borrow keyword with nothing legal to do through it once you have one is half
a feature.

## 1b. Inherent operation shadows a concept operation of the same name

Stress-tests the `.` resolution order (fields → get/set → inherent `operation` → concept dispatch)
added alongside the constructor/property fix. Concept/implementation blocks have no ratified CoLisp
form anywhere in the document yet (only pseudocode paired with a real Co-Forth form, e.g.
`JsonSerializable`/`UserJson` in "Generics, concepts, dispatch, and metaprogramming") — that's a
pre-existing gap, logged in `PROGRESS.md`, out of scope to close in this file. The concept half below
is written in that same pseudocode, matching existing document convention rather than inventing a
third style:

```text
concept Describable {
    operation describe(&self) -> string
}

implementation AccountDescribable for Account : Describable {
    operation describe(&self) => format("Account({})", self.id)
}
```

```lisp
(implementation Account
  (operation (describe (self)) : string             ; inherent — same name as the concept operation
    (format "Account #{} (balance {})" (. self id) (. self balance))))

(let [a (Account.open "acct-2")]
  (assert-eq (Account.describe a) "Account #acct-2 (balance 0)"))  ; inherent wins — shadowed, not ambiguous
```

No gap in the resolution rule itself: the precedence added to the `.` resolution passage answers this
deterministically. Worth restating as a real tradeoff, not a defect: adding an inherent `operation` to
a record *after* a concept implementation already exists can silently change which body a caller
reaches — same hazard Rust accepts for inherent-vs-trait methods, inherited deliberately.

## 2. Ownership tour — borrow default, `steal`, `Unique`/`Shared`/`Weak`, `match-type`

```lisp
(define (describe (x : Foo)) : string                       ; unannotated = borrow, read-only, non-escaping
  (foo-name x))

(define (absorb (steal x : Foo)) : Foo                       ; ownership transfer
  x)

(define (log-carrier <O> (steal x : O)) : string             ; header placement now CONFIRMED —
  ! pure                                                      ; `render-all`'s `<(types Ts...)>` in
  (match-type O                                               ; "Parameter packs..." shows `<...>`
    (Unique<Foo> "exclusively owned")                         ; right after the name — but that example
    (Shared<Foo> (retain x) "shared, retained a handle")      ; only shows a *pack* header; `<O>` alone
    (_ "some other owner")))                                  ; for an ordinary bound is still a guess.

(let [u (Unique.new (Foo.default)) s (Shared.new (Foo.default))]
  (describe u)                        ; borrow, non-escaping — u still owns after this call
  (log-carrier (steal u))             ; ownership transferred; u is dead from here on
  (log-carrier (steal s))             ; Shared's steal just moves the handle, not the payload
  (let [w (Shared.downgrade s)]
    (match (Weak.upgrade w)
      (ok s2 (assert-eq (log-carrier (steal s2)) "shared, retained a handle"))
      (err _ (panic "unreachable: s is still alive")))))
```

Composes cleanly: `match-type` narrowing, `steal` desugaring to `<O : Owner<Foo>>`, and
`Weak::upgrade`'s CAS-loop-backed `result` all line up with what's specified. The generic-header
*placement* question from the first draft is resolved (confirmed against `render-all` in §9, below);
what's still an unconfirmed guess is only the narrower question of an ordinary bound's spelling
inside it (`<O>` versus `<O : Owner<Foo>>` versus something else) for a hand-written, non-pack case.

## 3. Effects (`!`), `?` propagation, concepts together

```text
concept JsonSerializable {
    associated Output = bytes
    operation serialize(&self, options: &JsonOptions) -> Output
}

implementation UserJson for User : JsonSerializable {
    operation serialize(&self, options) => UserCodec.serialize(options, self)
}
```

```lisp
(record User pub id: string pub name: string)

(define (save-user (u : User) (opts : borrow JsonOptions)) : (result unit IoError)
  ! throws IoError
  (let [bytes (serialize u opts)]          ; bare-name concept dispatch + one default-imported evidence,
    (? (fs-write "user.json" bytes))         ; per "Every implementation has a stable qualified name..." —
    (ok unit)))                              ; NOT `Type.operation`; `using UserJson` would disambiguate

(define (save-user-pure-check ()) : unit
  ! pure
  ; would not typecheck if the body called save-user — pure/throws mismatch caught here, not at the call site
  unit)
```

No gap in the mechanics that are specified: `!`-unified effect rows and `?` compose as documented.
Flagging one **readability** cost, not a buildability bug: a reader has to already know
`User : JsonSerializable` is implemented somewhere else in the module to know `serialize` resolves at
all — an argument for tooling (go-to-implementation), not a spec defect. Also flagging, again, that
`.`-sugar for concept dispatch (`u.serialize(...)`) is not written here because it's unverified
whether infix `.` is defined as sugar over concept-provided operations at all, versus only over
fields/`get`/`set` — the "Human-facing member syntax uses one `.` operation" passage never says
explicitly. **Corrected while writing this:** the first draft called this qualified as `User.serialize u opts`,
by analogy with the `Account.open`/`Account.describe` calls elsewhere in this file. Checking against
"Every implementation has a stable qualified name" ("Generics, concepts, dispatch, and
metaprogramming") shows that's wrong for concept dispatch specifically — the real mechanism is a bare
call resolved against a default-imported or `using`-named evidence, never `Type.operation`
qualification. `Account.open`/`Account.describe` are a *different* case (inherent constructor/
operation, no concept evidence involved) and still **UNVERIFIED**: no established convention for
naming an inherent operation at a call site was found anywhere in the document either — `Type.member`
is the most natural guess by analogy to how a constructor (which has no receiver to call through)
would need some namespacing, not a confirmed spelling.

## 4. Variant (tagged union) with record-shaped arms + destructuring

```lisp
(variant WebEvent
  PageLoad
  PageUnload
  (KeyPress char)
  (Paste string)
  (Click { x: int, y: int }))          ; record-shaped arm

(define (handle (e : WebEvent)) : string
  (match e
    (WebEvent.PageLoad "loaded")
    (WebEvent.PageUnload "unloaded")
    (WebEvent.KeyPress k (string-from-char k))
    (WebEvent.Paste s s)
    (WebEvent.Click { x, y } (format "click at {},{}" x y))))
```

No gap: this is the Rust `enum WebEvent {...}` example from earlier in the session, mapping directly
onto one `variant` with mixed unit/tuple/record arms, matching the "D/TypeScript enums are just
all-unit variants" resolution. Match-arm spelling (`WebEvent.PageLoad`, dotted-path constructor
patterns) is **UNVERIFIED** — no real `match`-over-`variant` CoLisp example exists elsewhere in the
document to check this against; only the abstract parity-ledger entry (`(match value ...)`) and the
`option<T>`/`result<T,E>` exhaustive-destructuring mention, neither of which shows arm syntax.

## 6. Simple call-forwarding needs no CTFE at all — ordinary generics already do it

From the `timed`/`define-syntax` retirement conversation: a wrapper that forwards to an unknown
function with its signature unchanged doesn't need `syntax`, `mixin`, or any CTFE machinery — it's
an ordinary generic higher-order function, the same way D's `alias name = template!(fn);` needs no
macro either. `F` is monomorphized per instantiation (established for `steal`/`match-type` earlier),
so `f` is a fully-known, directly-callable value at each concrete use:

```lisp
(define (require-pkg (name : string)) : void
  (pkg.ensure name)
  (svc.enable name))

(define (timed <F> (f : F)) : (fn (...) -> ...)   ; UNVERIFIED: no real function-type return
  (lambda (...args)                                ; annotation exists anywhere in the document —
    (let [t (now)]                                 ; same gap flagged for `make-adder` earlier.
      (let [r (f ...args)]
        (log-elapsed t)
        r))))

(define timed-require-pkg (timed require-pkg))   ; ordinary top-level, callable-by-name function —
                                                    ; define binds a value, a function is a value,
                                                    ; nothing CTFE-shaped is happening here at all
```

## 7. Structural rewriting genuinely needs CTFE — and hits a real, still-open gap

The advanced case from the same conversation: not wrapping a call, but inspecting and rewriting a
function's actual body. Minimal version — prepend one statement rather than "every other line," to
isolate the actual gap instead of burying it in list-splicing detail:

```lisp
(define (add-logging (f : syntax)) : syntax
  (let [spec (function-spec-of f)]                        ; UNVERIFIED/GAP: `function-spec-of` —
    `(lambda (,@(params->syntax (. spec parameters)))      ; resolve a syntax-carried reference to
       (log "entering")                                    ; its FunctionSpec — named here for the
       ,@(. spec body))))                                  ; first time, not previously specified.

(define require-pkg/logged (mixin (add-logging require-pkg)))
```

**Real gap, exactly the one flagged when `FunctionSpec` was added:** `params->syntax` doesn't exist.
`FunctionSpec.parameters` is a `ParameterSpec` — structured `ParamEntry` values (name, type,
ownership mode), deliberately *not* raw syntax, so ordinary code can inspect it without pattern-
matching a tree. But rebuilding a new lambda's parameter list means going the other direction —
struct back to syntax — and nothing plays `datum->syntax`'s role for `ParameterSpec` specifically.
Without it, "keep the original signature, change only the body" cannot be written at all; the CTFE
function would have to reconstruct each parameter's surface spelling by hand from `ParamEntry`
fields, which is exactly the "re-invent a piece of the compiler" cost `ParameterSpec` was added to
avoid in the first place — just moved one step later, from reading a signature to rebuilding one.
Also unverified in the same example: whether `,@` (splice) is legal against something that isn't a
literal list already in the source (here, the *result* of calling `params->syntax`) — every real
`,@` example in the document splices an already-bound list value, so this should be fine, but it's
worth flagging since it's the first time this file uses `,@` at all.

## 9. Compile-time heterogeneous parameter packs, extended past the document's own sketch

The document's own `render-all`/`sum` examples ("Parameter packs, runtime rest arguments, and C
varargs") are real, established syntax, but both bodies are just `...` / left empty. Extending
`render-all` to an actual working body, to test the composition rather than just the declaration
shape:

```lisp
(define (render-all <(types Ts...)>
                    (args : (params (borrow Ts)...))) : string
  (let [pieces (list)]
    (ct-foreach ((T arg) args)
      (set! pieces (append pieces (to-string arg))))
    (join pieces ", ")))

(render-all 1 "x" true)   ; => "1, x, true" — exact call syntax the document itself shows
```

**UNVERIFIED, flagged rather than assumed:** `set!`/mutable-local reassignment (`pieces` needs to
accumulate across pack iterations) isn't established anywhere confirmed in this document either —
same family of gap as `borrow-mut`/in-place field mutation from §1, now hit from a different angle.
Whether `ct-foreach`'s body may perform ordinary (non-CTFE) side effects like this at all, or
whether it's restricted to CTFE-only operations since the pack itself only exists at compile time,
is also not stated — the document says CTFE "may inspect, slice, destructure, zip, and `foreach`
over" packs, but doesn't say what an *ordinary* runtime value built up during that iteration is
allowed to do. `to-string`/`append`/`join`/`list` are also invented for this example — plausible
stdlib names, not confirmed against anything.

No gap in the pack mechanics themselves: `<(types Ts...)>` header placement, `(params (borrow Ts)...)`
as the pack-typed parameter, and the call syntax `(render-all 1 "x" true)` all match the document's
own real example exactly — extending the body is what surfaced the *next* layer of gaps (mutable
locals, CTFE-vs-runtime boundary inside a `ct-foreach` body), not a problem with the pack feature
itself.

## 10. Open gaps, current as of this pass — what's still missing and why

- **Capability requests with wildcarded paths** (`read{path="~/**"}`) — still ungrammared; unchanged
  since first flagged.
- **`borrow-mut` and in-place field mutation** — one combined open item (§1); a mutable-borrow
  keyword with nothing legal to write through it once you have one is half a feature.
- **Explicit discriminant/`repr` for variants** — not specified, so `WebEvent` (§4) has no `repr`
  clause.
- **`ParameterSpec -> syntax` reconstruction** (§7, new this pass) — needed to rebuild a signature
  from its introspected form; without it, "same signature, different body" CTFE can't be written.
  This is the concrete, load-bearing case; §1's `borrow-mut` is the other half of the same family
  (a capability with no way to act on what it gives you).
- **`function-spec-of`, or whatever resolves a captured `syntax` reference to its `FunctionSpec`**
  (§7, new this pass) — named here for the first time; the document establishes the two-step
  resolve-then-`require` *pattern* but never names the actual entry point a CTFE body would call.
- **Mutable locals** (`set!` or equivalent, §9, new this pass) — hit from a different angle than
  §1's field mutation: accumulating a value across a `ct-foreach` pack iteration needs *some* local
  reassignment primitive, and none is established. Possibly the same underlying gap as `borrow-mut`,
  possibly a separate, narrower one (a local isn't behind a borrow) — not resolved either way.
- **Whether ordinary runtime side effects are permitted inside a `ct-foreach` body at all** (§9, new
  this pass) — the document says CTFE may inspect/slice/`foreach` a pack, but not whether the loop
  body itself runs as ordinary code (with ordinary effects) or is restricted to CTFE-only operations
  the way the pack's own existence is compile-time-only.
