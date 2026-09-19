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
`JsonSerializable`/`User` in "Generics, concepts, dispatch, and metaprogramming") — that's a
pre-existing gap, logged in `PROGRESS.md`, out of scope to close in this file. The concept half below
is written in that same pseudocode, matching existing document convention rather than inventing a
third style:

```text
concept Describable {
    operation describe(&self) -> string
}

implementation Account : Describable {
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

(let [u (new unique Foo :field 0) s (new shared Foo :field 0)]
  (describe u)                        ; borrow, non-escaping — u still owns after this call
  (log-carrier (steal u))             ; ownership transferred; u is dead from here on
  (log-carrier (steal s))             ; Shared's steal just moves the handle, not the payload
  (let [w (weaken s)]                 ; GAP below — this call is a guess
    (match (upgrade w)                ; `upgrade` returns `option<Shared<T>>`, NOT `result` —
      (some s2 (assert-eq (log-carrier (steal s2)) "shared, retained a handle"))  ; `some`/`none`
      (none (panic "unreachable: s is still alive")))))                          ; arms, not `ok`/`err`
```

**Corrected while checking this section against real syntax rather than assuming the earlier draft
was right:** the first pass invented `Unique.new (...)`/`Shared.new (...)`/`Shared.downgrade` — none
of which exist. The real, established construction syntax is `(new unique Foo ...)`/
`(new shared Foo ...)` ("Stack, heap, and deterministic destruction," `:2914-2916`). Worse than an
invented spelling: the first pass also matched `upgrade`'s result with `(ok s2 ...)`/`(err _ ...)` —
but the document states plainly that "`Weak<T>` ... upgrade returns `option<Shared<T>>`," not a
`result` — `option` destructures as `some`/`none`, not `ok`/`err`. Fixed both.

**Remaining, real GAP, not fixed because there's nothing to fix it to:** `weaken`'s and `upgrade`'s
*call* syntax is still a guess. `weaken` is confirmed as a real word — but only shown as a
*closure-capture* mode ("entries may explicitly borrow, mutably borrow, steal, retain, weaken,
clone..."), never as an ordinary function called on an arbitrary `Shared<T>` value outside a
capture clause. Whether the same word does both jobs, or whether converting a `Shared<T>` to
`Weak<T>` in ordinary code has a different name entirely, isn't stated. `(weaken s)`/`(upgrade w)`
above are the most natural guesses, not confirmed spellings.

`match-type` narrowing and `steal` desugaring to `<O : Owner<Foo>>` do line up with what's specified,
now that the construction/upgrade mistakes are fixed. The generic-header *placement* question from
the first draft is resolved (confirmed against `render-all` in §9, below); what's still an
unconfirmed guess is only the narrower question of an ordinary bound's spelling inside it (`<O>`
versus `<O : Owner<Foo>>` versus something else) for a hand-written, non-pack case.

## 3. Effects (`!`), `?` propagation, concepts together

```text
concept JsonSerializable {
    associated Output = bytes
    operation serialize(&self, options: &JsonOptions) -> Output
}

implementation User : JsonSerializable {
    operation serialize(&self, options) => UserCodec.serialize(options, self)
}
```

```lisp
(record User pub id: string pub name: string)

(define (save-user (u : User) (opts : borrow JsonOptions)) : (result unit IoError)
  ! throws IoError
  (let [bytes (serialize u opts)]          ; bare-name concept dispatch — coherence guarantees exactly
    (? (fs-write "user.json" bytes))         ; one JsonSerializable implementation for User, so this is
    (ok unit)))                              ; unambiguous without naming anything, unlike an earlier pass

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

## 11. Closure captures with mixed ownership modes, `weaken` where it's actually confirmed

§2 flagged `weaken` as confirmed only for closure captures, not as an ordinary standalone function.
Testing exactly that composition — `weaken` in a capture list, `upgrade` used on it inside the body —
using the document's own real capture-list syntax directly:

```lisp
(define (make-observer (cache : Shared<Cache>)) : (fn () -> string)   ; UNVERIFIED return-type
  (lambda (:captures (weaken cache))                                  ; annotation, same as §6 —
          ()                                                          ; not re-flagging separately.
    (match (upgrade cache)
      (some c (describe c))
      (none "cache is gone"))))
```

No gap in the capture mechanics themselves: `(:captures (weaken cache))` matches the document's own
`(:captures (borrow config) (steal socket) (retain cache))` shape exactly, just with the fifth listed
capture mode (`weaken`) instead of the first three shown. `upgrade`'s `some`/`none` destructuring
(fixed in §2) is consistent here too. **Same UNVERIFIED as §2, not a new one:** whether `upgrade` is
called bare like this, or some other way, is still a guess — this example just confirms `weaken`
*itself* is solid in the one place it's actually documented, narrowing rather than closing that gap.

## 12. Derive-style msgpack serialization — an end-to-end CTFE stress test

The real target this whole `syntax`/`FunctionSpec`/`members-of`/`fields-of`/`! comptime` mechanism
was built to reach: a library-authored derive, no compiler support beyond the hooks already
specified, generating real serialize/deserialize code from a record's own field list.

```lisp
(concept MsgPackSerializable
  (operation serialize (&self) -> bytes))

(define (write-msgpack-field <T : MsgPackSerializable> (buf : MsgpackBuffer) (name : string) (value : T)) : unit
  (buf.write-tagged name (value.serialize)))
```

**The safety property this whole exercise was actually testing:** `write-msgpack-field` is an
ordinary generic function bounded on the concept, not "write anything blindly." A type that holds a
raw OS resource — a file descriptor, a socket — simply doesn't implement `MsgPackSerializable`,
the same way `std::fs::File` in Rust doesn't implement `serde::Serialize` at all. That's a library
design choice, not a language restriction, and it's already fully sufficient: the moment
`derive-msgpack-serialize` (below) generates a call to `write-msgpack-field` for a field whose type
doesn't satisfy `T : MsgPackSerializable`, that's an ordinary, already-existing concept-bound
violation — a compile error at the derive site, not silent corruption discovered later when a
deserialized `FileHandle` turns out to reference a completely unrelated OS resource in whatever
process reads it back. No new "record vs. class" split needed for this — the concept-bound check
already *is* the distinction between safely-serializable and not.

```lisp
; extends §1's Account with one more field for this section specifically — not a literal
; continuation of the same declaration, which would be a duplicate-definition error
(record Account
  pub id: string
  balance: int
  handle: FileHandle)   ; FileHandle deliberately does NOT implement MsgPackSerializable
```

**Serialize direction — widens `fields-of` to include get-only properties, since serialize has every
reason to read one; `handle`'s field type not satisfying `T : MsgPackSerializable` is exactly the
compile error `write-msgpack-field`'s bound already produces, not something `derive-msgpack-serialize`
itself needs to check for separately:**

```lisp
(define (derive-msgpack-serialize (rec : syntax)) : syntax
  (let [flds (fields-of rec :include-properties-readonly #t)]
    (let [writes (map (lambda (f)
                         (let [name-stx (datum->syntax (. f name) rec)]
                           `(write-msgpack-field buf ,(. f name) (. self ,name-stx))))
                       flds)]
      `(implementation ,(fresh-name rec "MsgPack") for ,rec : MsgPackSerializable
         (operation (serialize (self)) : bytes
           (let [buf (new-msgpack-buffer)]
             ,@writes
             (msgpack-buffer-to-bytes buf)))))))

(mixin (derive-msgpack-serialize Account))
; GAP, not glossed: this line should fail to compile, once `write-msgpack-field`'s bound and
; `handle`'s type are both real — `handle : FileHandle` has no `MsgPackSerializable` implementation,
; so the generated `(write-msgpack-field buf "handle" (. self handle))` should be rejected the same
; way any other unsatisfied concept bound is. Included specifically to show the failure is real and
; located at the derive site, not to claim this repository has verified it actually rejects today.
```

**Deserialize direction — default `fields-of` call (no widening) already excludes get-only properties
builds a fresh constructor rather than reusing an existing one, sidestepping the
`ParameterSpec -> syntax` gap entirely:**

```lisp
(define (derive-msgpack-deserialize (rec : syntax)) : syntax
  (let [flds (fields-of rec :include-private #t)]   ; NOT the narrow default — checked against
                                                       ; §1's `Account.balance`, module-private:
                                                       ; the default would silently drop it from
                                                       ; the reconstructed record, a real round-trip
                                                       ; bug, not just an access-control nicety.
                                                       ; Consistent with mixin's own established
                                                       ; rule (full access "as if written at that
                                                       ; site") — a generated constructor already
                                                       ; has this access; readonly properties are
                                                       ; still excluded, since nothing changed about
                                                       ; not being able to write through those.
    (let [kw-pairs (map (lambda (f)
                          (list (keyword-syntax-of (. f name))
                                `(read-msgpack-field buf ,(. f name))))
                        flds)]
      `(implementation ,rec
         (constructor (from-msgpack-bytes (buf : bytes)) : ,rec
           (,rec ,@(flatten kw-pairs)))))))

(mixin (derive-msgpack-deserialize Account))
```

**Composability check — both derives applied to the same record, plus a `describe` operation from
§1b already sitting on `Account`, testing whether independent derives collide:**

```lisp
(mixin (derive-msgpack-serialize Account))
(mixin (derive-msgpack-deserialize Account))
; Account.describe (§1b, inherent) untouched by either — different implementation blocks,
; different names, no shared declaration surface to collide on.
```

**No gap in composability itself**, per the already-established rule: "two derives may both
implement an operation spelled `serialize`... without creating a global-name collision" — hygiene
already promises this, and nothing about running two independent `mixin` calls against the same
record contradicts it. **Every genuinely new gap this surfaced is named precisely, not glossed:**

- **`fresh-name`** — used to generate the serialize-implementation's name so two derives never
  collide on it. `datum->syntax` distinguishes "hygienically fresh" from "caller-context" identifier
  *construction*, implying a fresh-identifier operation exists, but never names it. Guessed here.
- **`keyword-syntax-of`** — turns a bare field-name symbol into a `:name`-shaped keyword atom for
  record-literal construction (`(rec :name val ...)`). Whether `:x` is the *same* kind of token
  `datum->syntax` already promotes symbols into, or a genuinely different atom shape the reader
  handles specially, is unconfirmed — flagged rather than assumed identical.
- **`map`/`flatten`/`list`** — assumed stdlib names, same category as `to-string`/`append`/`join`
  flagged in §9; plausible, not confirmed. (`filter`/`eq?` dropped from this list — no longer used
  once `fields-of` gained explicit `:include-private`/`:include-properties-readonly` parameters,
  itself found while working through this section: the first draft returned everything
  unconditionally and expected every caller to filter, which meant private fields were visible to
  any derive by default with no way to opt out.)
- **Record-literal construction from a *spliced, variable-length* keyword-argument list**
  (`(,rec ,@(flatten kw-pairs))`) — `,@` splicing into a value position inside an ordinary call is
  established for the `render-all`/pack-forwarding case (§9), but never specifically shown feeding a
  *record constructor's* keyword-argument list; a plausible, not confirmed, extension.

**A real round-trip bug also caught and fixed while writing this, not left in:** the first draft of
`derive-msgpack-deserialize` used `fields-of`'s narrow default (pub fields only), which would have
silently dropped `Account.balance` (module-private) from every reconstructed record — a real
correctness bug in the derive, not just an access-control question, since a mixin-generated
constructor already has full module access per the earlier-established "as if written at that site"
rule. Fixed to `:include-private #t`, matching what the constructor is actually allowed to do.

None of these are the kind of gap that suggests the design doesn't hold up — every one is a small,
nameable missing utility function around an already-solid core (`fields-of`'s `kind` filter,
`datum->syntax`, `mixin`'s eager-escape composition, hygiene's no-collision guarantee). That's a
meaningfully different, better outcome than finding the *mechanism* itself doesn't compose.

## 13. CTFE-of-values: what folds, what never should, however constant its inputs look

Testing the eligibility rule just added to the spec directly: `! pure` gates folding, not whether
arguments happen to be literals.

```lisp
(define (fib (n : int)) : int
  ! pure
  (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))

(define answer (fib 10))   ; eligible: fib is ! pure — folds to 55 at compile time
```

```lisp
(define (factorial (n : int)) : int
  ! pure
  (if (<= n 1) 1 (* n (factorial (- n 1)))))

(define (choose (n : int) (k : int)) : int
  ! pure
  (/ (factorial n) (* (factorial k) (factorial (- n k)))))

(define c (choose 10 3))   ; eligible for the same reason — folds to 120
```

**The negative case this section was actually written to test:**

```lisp
(define (read-file (path : string)) : string
  ! {fs.read(path=path)} throws IoError
  (fs-read-to-string path))

(define config (read-file "config.txt"))
; NOT eligible for CTFE-of-values folding — "config.txt" being a literal is irrelevant. The
; disqualifying part is specifically the {fs.read(path=path)} capability requirement, NOT
; `throws IoError` on its own (corrected in the spec in the same pass this example was checked
; against it: ! pure and throws are orthogonal, so a pure-but-throwing function stays eligible —
; json/parse, used below, is exactly that case). This call happens at ordinary runtime, when
; `config` is actually initialized — never silently executed against the build machine's
; filesystem just because the compiler could see a constant path.
```

No gap in the rule itself, corrected version — `fib`/`choose`/`json/parse` (below)/`read-file` are
exactly the cases the eligibility addition distinguishes, and all four behave the way the corrected
rule says they should. **UNVERIFIED, not glossed:** the exact combined spelling shown above
(`! {fs.read(path=path)} throws IoError`) is a plausible combination by analogy to the
`!`-unification work, not confirmed against a single real example showing a capability and `throws`
together in one row.

## 14. `include-str` + `json/parse` + CTFE + `mixin` — generate a test suite from a data file

The composition Shammah pointed out directly, worked all the way through: embed a JSON fixture at
compile time, parse it, generate one test per entry, splice the whole suite in.

```lisp
(define (generate-tests-from-json (path : string)) : syntax
  ! throws JsonError
  (let [text (include-str path)]
    (let [cases (? (json/parse text))]
      (let [test-forms (map (lambda (c)
                               `(test ,(. c name)
                                  (lambda (ctx)
                                    (assert-eq (compute (. c input)) (. c expected)))))
                             cases)]
        `(test-suite ,path ,@test-forms)))))

(mixin (generate-tests-from-json "test-cases.json"))
```

**Deliberately uses `?`/`throws` for the failure path, not an invented `panic`-in-CTFE mechanism —
this is the point worth being precise about.** `json/parse` returns `result<JSON, Error>`, per
Shammah's correction to prefer that over `throws` for the parser itself (a value-based failure that
never alters control flow until explicitly converted, rather than an exception). `?` propagates that
`result` out of `generate-tests-from-json`, which is why the function is declared `! throws
JsonError` — ordinary, already-established mechanics, reused rather than reinvented. Since this
whole call happens at compile time (`include-str` taints the function `! comptime`, discharged by
`mixin`, and `include-str`'s own eligibility — established just for it — doesn't depend on the
general `! pure` rule at all, since embedding a file is never meaningful at runtime in the first
place), a malformed fixture file surfaces as an ordinary compile error at this `mixin` call, per the
just-added "unhandled throw during CTFE is a compile error" rule — not a runtime surprise on
whichever machine happens to load the fixture later, and not a silently-generated empty test suite.

**No gap in the composition itself** — `include-str`, `json/parse`'s `result` shape, `?`/`throws`
propagation, `map` building a list of `syntax` forms via quasiquote, and `mixin` discharging the
whole chain all fit together exactly as each piece was specified. **What's still unconfirmed, named
precisely rather than assumed:**

- **Field access on a parsed JSON value** (`(. c name)`, `(. c input)`) — assumes a parsed JSON
  object supports the same `.`-access CoLisp records already do; plausible (JSON objects are
  map-shaped, and maps are mentioned as exposing traversal operations through concepts), not
  confirmed against any real example of accessing one.
- **`(test name (lambda (ctx) ...))` and `(test-suite name form...)`'s exact CoLisp call shape** —
  the parity ledger confirms these exist (`(test ...)`, `(test-suite ...)`), the same abstract level
  of confirmation as `match`/`variant`'s ledger entries; the concrete argument shape used here
  (name first, then a context-taking lambda) is inferred from the real `test-suite`/`test` example
  used for `json/parse` itself earlier in the document, not independently confirmed.

## 15. One record, multiple concepts — and why the model isn't traits or classes

**The easy case first — two unrelated concepts, no naming question at all:**

```text
implementation Account : JsonSerializable {
    operation serialize(&self) => json.of(self.id, self.balance)
}
implementation Account : MsgPackSerializable {
    operation serialize(&self) -> bytes => (mixin (derive-msgpack-serialize Account))
}
```

Both operations happen to be spelled `serialize`. No collision, because names live in their
concept's own evidence, not one shared method table — already established, confirmed again here.

**The case that originally motivated named, multiple implementations — and why it's rejected
instead, not accommodated:**

```text
implementation Account : Equal<Account, Account> {
    operation equal(borrow left, borrow right) =>
        (and (== (. left id) (. right id)) (== (. left balance) (. right balance)))
}

; REJECTED — coherence: Account already has an Equal<Account,Account> implementation above.
implementation Account : Equal<Account, Account> {
    operation equal(borrow left, borrow right) => (== (. left id) (. right id))
}
```

This document originally used exactly this example — a same-concept, same-type, "equal by id" vs.
"equal by all fields" split — as the running justification for letting multiple named
implementations of one (concept, type) pair coexist, closer to Haskell's `newtype`-wrapped alternate
instances (`Down` for reverse `Ord`) than to Rust's coherence rule. It doesn't survive scrutiny: it's
the same shape as the canonical/compact-JSON example that motivated the same feature elsewhere in
this document, and collapses the same way once actually needed — "equal by id" is a different,
narrower notion than "equal by all fields," not a second, competing definition of the same one, so
it belongs on a wrapper type, the same as Rust would do it:

```text
record ById(Account)

implementation ById : Equal<ById, ById> {
    operation equal(borrow left, borrow right) => (== (. (. left 0) id) (. (. right 0) id))
}
```

`Account` keeps exactly one, unambiguous notion of equality; "equal by id" becomes a distinct type
with its own single implementation, never a second implementation competing for the same slot.
Finch's coherence rule is now the same as Rust's, not a deliberately different tradeoff — this
section originally argued the opposite, and that argument is what changed, not just this example.

**The difference from classes:** a class fuses data, behavior, and identity (inheritance) into one
declaration — a subclass inherits its parent's methods automatically, and dynamic (virtual) dispatch
is typically the default the moment any method is overridable. Finch keeps these as separate,
additive declarations: `record Account` owns only data and layout; each `implementation` block is a
separate, external declaration linking a concept (or nothing, for inherent operations) to that type.
There's no inheritance hierarchy at all — "named records have nominal identity; matching field names
do not make independently declared records interchangeable" is already established — so there's no
IS-A relationship to reason about, and dispatch is statically monomorphized by default; dynamic
dispatch is the explicit, opt-in `dyn Concept` erasure discussed earlier, never automatic.

**How the two roles stay clean instead of colliding — this is the part worth stating precisely,
not just asserting:** because concept implementations are external and statically resolved by
default, adding one to a record never touches the record's own layout — no vtable pointer gets
implicitly added to every instance the way a C++/Java class picks one up the moment it gains a
virtual method. **UNVERIFIED, flagged rather than asserted as confirmed:** this is a reasonable
inference from record layout and concept-implementation being discussed as entirely separate
concerns everywhere in the document, not a sentence that states it outright anywhere. A record can
be simultaneously plain, trivially-introspectable data (`fields-of` sees it exactly as declared) and
the subject of arbitrarily many concept implementations, and neither role taxes the other — the data
shape a serializer walks is identical to the data shape sitting in memory, whether the record
implements zero concepts or twenty.

## 16. Why `syntax`+`,@`+`mixin` instead of D's template-mixin/string-mixin split

Checked against a real, external D codebase (Shammah's own `gameserver` project,
`source/messages/core.d`) rather than a hypothetical. D's `GenEnum` builds an enum declaration from
a compile-time-discovered list of message types, as a string, then string-mixes it in:

```d
string GenEnum(string Name) {
    bool needsComma = false;
    string code = "enum " ~ Name ~ " {";
    foreach (messageType; AllMessages) {
        code ~= (needsComma ? "," : "") ~ __traits(identifier, messageType) ~ "=" ~ to!string(messageType.opCodeStatic);
        needsComma = true;
    }
    code ~= " }";
    return code;
}
mixin(GenEnum("OpCode"));
```

The Finch equivalent, using exactly `fields-of`/`members-of`-style discovery plus mechanisms already
built out earlier in this file:

```lisp
(define (generate-opcode-enum) : syntax
  (let [entries (map (lambda (mt) `(,(. mt name) ,(. mt opcode))) AllMessages)]
    `(variant OpCode ,@entries)))

(mixin (generate-opcode-enum))
```

No `needsComma` bookkeeping — `,@` splicing a list handles "however many entries there are"
structurally, since it operates on list data rather than text needing manual separator tracking, and
nothing is ever manufactured as text or re-parsed. **This is the actual, verified argument for
S-expressions over D's split**, not an aesthetic preference: D's safe path (template mixins) is
genuinely more awkward to author for this exact case (confirmed separately — `GetModuleMessages` in
the same file hand-writes recursive-template filtering, since D has no compile-time `filter`/`map`),
which is why the easier-but-unsafe path (string mixins) gets reached for in practice, by the same
author who otherwise avoids them. Making the safe path also the easy path removes the reason to want
an escape hatch, rather than just removing the escape hatch and leaving the awkwardness in place.

**UNVERIFIED, flagged rather than assumed:** `AllMessages`/`.name`/`.opcode` here stand in for
whatever `members-of`-style discovery would actually produce for this use case — a real version
would need a records-vs-classes note about what "deriving from a base type" even means in Finch's
concept-based world (there's no inheritance to filter by), not just a syntax substitution. The point
being tested is the splice-versus-string-concatenation ergonomics, not a complete port of `GenEnum`'s
semantics.

## 17. Capability templating, exercised properly — a real correction, not just a gap

**A real mistake worth stating plainly: earlier passes in this file, and earlier in this
conversation, claimed capability wildcarding was "ungrammared."** That was wrong, or at least badly
incomplete — checked properly this time rather than trusted from memory. `path<workspace:
"generated/**">` is a real, working type-level refinement, used concretely in the document's own
`save-report` example, not a hypothetical. There is a named grammar for the whole selector
language: "function effects may contain a restricted selector expression over immutable typed
arguments. The allowed expression nodes are root, literal relative path, refined path argument,
join, and narrow; general string interpolation and user-defined evaluation are forbidden." Five
named node kinds — this is a real, if terse, specification, not a wildcard heuristic bolted on.

```lisp
(define (publish-asset (path : path<workspace:"assets/**">) (data : bytes)) : unit
  ! {fs.write(root=workspace, path="assets/**")}
  (file.write path data))
```

This composes exactly like `save-report` already does — wildcarding lives in the *type* of the
path argument, not as a separate runtime string check, and the effect row names the same pattern.
Calling `publish-asset` with a path outside `assets/**` is a type error at the call site, not a
runtime permission check that might be forgotten.

**The user's original example, `read{path="~/**"}`, needs a real correction, not a small syntax
fix.** `~` means the home directory — outside the workspace root entirely — and the document is
explicit that `path<R>` "is relative to an immutable workspace/project root and rejects traversal
or symlink escape." Reaching outside that root isn't a wider pattern on the same root; it's a
different, more privileged root altogether: `root<host-machine>`, a "distinct host-issued root
resource" a user must deliberately grant, with "the same type and selector rules... apply[ing]
below that root." So the corrected shape is closer to:

```lisp
(define (backup-home (path : path<root<host-machine>:"~/**">) (dest : path<workspace:"backups/**">)) : unit
  ! {fs.read(root=host-machine, path="~/**")} {fs.write(root=workspace, path="backups/**")}
  (file.copy path dest))
```

**UNVERIFIED, flagged precisely rather than presented as confirmed:** `root<host-machine>` and
`path<root<host-machine>:"...">` are inferred compositions from "for example `root<host-machine>`"
and "the same type and selector rules then apply below that root" — the document names the concept
and gives that one example identifier, but never shows a full, worked declaration combining it with
`path<R>` the way `path<workspace:...>` is shown combined. Plausible by direct analogy, not
independently confirmed. Likewise, `join` and `narrow` are named as real grammar nodes but have no
concrete CoLisp syntax anywhere — only "refined path argument" (the `path<workspace:"...">` form)
is ever shown worked out; composing two path fragments or narrowing an existing grant to a
subdirectory has no example to check against.

## 18. Open gaps, current as of this pass — what's still missing and why

- **Panics/traps unwinding across an FFI boundary** (FFI survey pass, new) — a Finch trap occurring
  inside a callback invoked from foreign code is unaddressed anywhere, and this is a real hazard in
  most ABIs unless explicitly caught at the boundary (Rust's documented `catch_unwind` requirement is
  the precedent). More urgent for Finch specifically than for Rust: overflow traps unconditionally in
  every build (per "Numeric types," above), so ordinary arithmetic in a callback can trap far more
  often than the equivalent Rust code would in a release build.
- **Creating a callback: a raw, ABI-stable function pointer from a Finch closure** (FFI survey pass,
  new) — distinct from the already-specified restriction on *variadic* callees (parameter-packs
  section); ordinary non-variadic callback creation, and what happens to a closure's captured
  environment past C's usual `void*`-userdata convention, is unaddressed.
- **Lifting C-style sentinel-return-plus-`errno` error conventions into `result`/`throws`** (FFI
  survey pass, new) — no adapter convention exists for this anywhere in the document.
- **Reentrant calls from a foreign thread the scheduler never spawned** (FFI survey pass, new) — if a
  C library invokes a registered Finch callback from its own worker thread, how that interacts with
  the task/fiber scheduler and the borrow checker's concurrency assumptions is unaddressed.
- **Opaque foreign-handle wrapping** (FFI survey pass, new) — `resource<K>` ("generation-bound
  runtime handle") looks like the intended mechanism for wrapping a C library's opaque pointer, but
  nothing states this explicitly; worth confirming rather than assuming, or inventing a second
  mechanism by accident later.
- **Conditional/bounded generic implementations** (coherence-rewrite pass, new) — whether a generic
  implementation may itself require a bound on its own type parameter (Rust's `impl<T: PartialEq>
  PartialEq for Vec<T>` shape: `implementation List<T : Equal<T,T>> : Equal<List<T>, List<T>> {
  ... }`) has no established syntax anywhere in this document. Separate from the specialization
  question already settled — this gates whether the one body exists for a given `T` at all, never
  chooses between competing bodies for different `T`.
- **Coherence across independently-compiled modules** (coherence-rewrite pass, new) — "at most one
  implementation of a concept per type" is stated as a rule, but not *where* it's checked. Two
  modules that never see each other, each implementing a foreign concept for a foreign type (the
  scenario Rust's orphan rule exists specifically to make rare), could in principle both compile
  cleanly alone and only conflict once a third module depends on both. Whether Finch restricts who
  may implement a concept for a type it doesn't own (Rust's actual answer) or defers the conflict to
  whole-program/link time is undecided.
- **`join`/`narrow` selector-expression syntax** — named in the grammar, never shown concretely.
- **`root<host-machine>` combined with `path<R>`'s refinement syntax** — named separately, never
  shown combined in one worked declaration.
- **`borrow-mut` and in-place field mutation** — one combined open item (§1); a mutable-borrow
  keyword with nothing legal to write through it once you have one is half a feature.
- **Explicit, opt-in C-compatible layout (`repr`), widened from variants to records generally** —
  originally logged narrowly ("discriminant/`repr` for variants," so `WebEvent` in §4 has no `repr`
  clause), but the same missing mechanism applies equally to plain records: default layout has no
  stability guarantee at all (confirmed this pass — deliberately, matching Rust's `repr(Rust)`), so
  systems-language C-ABI interop and manual packing optimization both need the explicit opt-in half
  of that same precedent, `repr(C)`-equivalent, on both records and variants. Neither the annotation
  syntax nor the packing rules it would follow are specified yet.
- **`dynamic-evidence-version` on an `implementation` block** (§19/§20 area, new this pass) — appears
  in two illustrative examples, never explained anywhere. Ruled out as a per-operation vtable-slot
  mechanism for an *ordinary* concept (§21's `#NN` finding — the numbers were on the wrong
  declaration to be that, and an ordinary concept's `dyn` table is rebuilt fresh on every
  recompilation, so nothing needs to survive across versions of it). Once `stable-evidence` concepts
  existed as a real, separate, opt-in mechanism (per-operation author-assigned keys, for network
  dispatch and dynamic module loading specifically), a plausible connection reopened: an
  `implementation`'s own `dynamic-evidence-version` may be the revision identifier that anchors
  which published, sealed key set its table was built against — but that is a new plausibility, not
  a confirmation, and nothing ties the two together explicitly yet.
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
- **`fresh-name`** (§12, new this pass) — a hygienically-fresh-identifier generator is implied by
  `datum->syntax`'s own text ("constructing a hygienically fresh identifier is distinct from...")
  but never itself named. Needed for any derive that must avoid colliding with another derive's
  generated names.
- **`keyword-syntax-of`** (§12, new this pass) — turning a bare symbol into a `:name`-shaped keyword
  atom for record-literal construction; whether this is the same promotion `datum->syntax` already
  does or a genuinely different reader-level atom shape is unconfirmed.
- **Record-literal construction from a spliced, variable-length keyword-argument list** (§12, new
  this pass) — `,@` splicing into an ordinary call's argument list is established for pack forwarding
  (§9); specifically feeding a record constructor's keyword-arguments this way is a plausible,
  unconfirmed extension of that same mechanism, not a new one.
- **A stdlib surface accumulating across §9/§12** (`filter`, `map`, `flatten`, `append`, `join`,
  `list`, `to-string`, `eq?`) — every one plausible, none confirmed against a real example. Worth
  resolving as a batch at some point rather than one at a time per new example, since the pattern of
  "invent a stdlib name, flag it, move on" is now recurring rather than incidental.
- **Tail call guarantee, unresolved** (§13 area, raised directly rather than found by an example) —
  "proper tail calls where marked by the IR" is the only mention anywhere in the document. Doesn't
  say whether TCO is a guarantee (matching Scheme's actual defining property) or a best-effort
  optimization, what marks a call as tail-position, or whether it covers mutual recursion between two
  functions, not just self-recursion. A very different language depending on the answer.
- **Combined capability-requirement-plus-`throws` effect-row spelling** (§13) — a function like
  `read-file` plausibly needs both a capability requirement (`{fs.read(path=path)}`) and
  `throws IoError` in one effect row; no real example shows both together, so §13's `read-file`
  intentionally uses only the confirmed half.
- **Field access on a parsed JSON value, and `test`/`test-suite`'s exact call shape** (§14, new this
  pass) — `(. c name)` on a parsed JSON object is plausible but unconfirmed; `(test name (lambda
  (ctx) ...))` is inferred from the real `json/parse` test example elsewhere in the document, at the
  same confirmation level as the parity ledger's abstract `(test ...)` entry, not independently
  verified.

**Resolved this pass, removed from this list rather than left stale:** compile-time file/data
embedding (added as `include-str`/`include-bytes`, modeled as `! comptime` hooks rather than
ordinary `! pure` functions, since embedding is never meaningful at runtime at all — a real
distinction from `fib`-style CTFE-of-values eligibility) and the question of what happens to an
unhandled `throw` during compile-time execution (a compile error, reusing the same correctness-
signal logic an unhandled `result` error already has, not a new failure mode).

## 19. Concept axioms — a checked law, distinguished from `symmetric`/`commutative`'s trusted ones

Provenance for this one is unusual: it comes from a real, previously-shipped D `std.concepts`
proposal (`isConcept`/`Axioms`/`conceptDiagnostic`, once pitched at Phobos), located and read for
this pass rather than reconstructed from memory. Its `Axioms` mechanism — an optional static
predicate a concept declares, checked against a candidate type during structural matching — is
genuinely useful, but it matches during *structural* candidate discovery, which Finch's model
explicitly does not use (concept satisfaction is always an explicit, named `implementation` block).
The adaptation checked here: keep the "declare a compile-time-checkable law" idea, drop the
structural-matching half, and attach the check to the already-explicit `implementation` site
instead.

```text
concept Codec<Wire, Value> {
    operation encode(borrow value: Value) -> Wire
    operation decode(borrow wire: Wire) -> result<Value, DecodeError>
    axiom wire-and-value-differ = not (Wire == Value)
}

implementation Codec<bytes, User> {
    operation encode = msgpack-encode-user
    operation decode = msgpack-decode-user
}

implementation Codec<User, User> {   ; REJECTED — axiom, not coherence: bytes != User doesn't apply
    operation encode = identity      ; here since Wire=Value=User; this is one legal instantiation
    operation decode = wrap-ok       ; of Codec, distinct from Codec<bytes,User> above, just a bad one
}
```

`Codec<bytes, User>` typechecks: `bytes == User` is false, so the axiom holds. `Codec<User, User>`
is a compile error at its own declaration — not a silently-skipped candidate the way a failing
`Axioms(T)()` would silently disqualify `User` from an implicit match in the D version. The error
names the concept instantiation (`Codec<User, User>`) and the failed axiom
(`wire-and-value-differ`), per the diagnostic-quality commitment in the design doc. This rejection
is the axiom failing, not coherence — `Codec<bytes, User>` and `Codec<User, User>` are different
instantiations of the generic concept (different type arguments), not two implementations
competing for the same one; coherence would only fire if two implementations tried to satisfy the
exact same instantiation.

This also produced a real, useful negative result worth keeping: an axiom checking
`not (Wire == Value)` only catches an *identity* codec, not a merely-useless one — nothing stops
`implementation Codec<bytes, bytes> { operation encode = id-copy; operation decode = wrap-ok-copy;
}` (a distinct, legal instantiation with behaviorally identical wire/value types) from compiling
and satisfying the concept, because "the types differ" is the only thing actually decidable here at
compile time — "the codec does something meaningful" is exactly the kind of runtime-semantic
property compile-time evaluation cannot decide, the same class of unprovable promise `isInputRange`
makes about `.empty`/`.front`/`.popFront`'s real behavior. The design doc's own new axiom section
says this in the abstract ("not a claim about arbitrary runtime instance behavior"); this example
is what that limitation actually looks like in a concrete program, not just in the caveat prose.

## 20. Checked-once bodies, template-style generation — resolving templates vs. generics

Directly from a design conversation about where Shammah actually sits on templates vs. generics:
not per-instantiation specialization/pattern-matching (declined), but the other thing templates give
you — distinct compiled code per instantiation so the optimizer can specialize, which plain
dictionary-passing generics can't offer. Confirmed this is separable rather than a contradiction:
checking (generic, proof-required, no SFINAE) and code generation (template-style, lazy,
per-instantiation) are independent axes, and Rust is the standing existence proof that combining
them is coherent.

```text
; Rejected outright -- not a runtime type error, not a per-instantiation SFINAE failure.
; Nothing declares that T supports +, so the body cannot be checked at all:
(define (foo (x : T)) : int
  (+ x x))

; Checked once against the declared bound; generates like a template thereafter --
; foo<i64> and foo<f64> are two distinct compiled functions, discovered lazily from
; whichever concrete calls actually exist in the program, each fully inlinable/specializable:
(define (foo <T : Add<T,T,Output=int>> (x : T)) : int
  (+ x x))
```

The first form is rejected the same way regardless of whether anyone ever calls `foo` with a
concrete type — there is no instantiation to try compiling, because there is nothing to compile
until a bound exists to check the body against. This is the load-bearing distinction from D's model:
D would accept the unconstrained form and defer the failure to whichever instantiation site first
tries `x + x` on a `T` that doesn't support it (or silently exclude `foo` as a non-viable overload
candidate via SFINAE if another overload exists). Finch's body is either provably valid under its
declared bound or it does not exist as a candidate at all — there is no "maybe it'll work out for
some future caller" state for a generic body to be in.

## 21. Two concepts, one shared free function — the Rust trait-duplication complaint doesn't apply

From a direct complaint about Rust traits: two traits can require operations that happen to do
exactly the same thing, and Rust forces either two identical method bodies or manually routing both
through a free function as a workaround. Checked against the concept model already established this
session (`operation X = some-callable`, never an obligatory inline body): the workaround Rust makes
you reach for is Finch's ordinary case, so the complaint doesn't arise in the first place.

```text
concept HasArea {
    operation area(&self) -> f64
}

concept Measurable {
    operation area(&self) -> f64
    operation perimeter(&self) -> f64
}

(define (rect-area (r : &Rectangle)) : f64
  (* r.width r.height))

(define (rect-perimeter (r : &Rectangle)) : f64
  (* 2.0 (+ r.width r.height)))

implementation Rectangle : HasArea {
    operation area = rect-area
}

implementation Rectangle : Measurable {
    operation area      = rect-area
    operation perimeter = rect-perimeter
}
```

Both `area` operations bind the identical `rect-area` callable — one function, written once,
referenced twice. `HasArea.area(rect)` and `Measurable.area(rect)` are two distinct, concept-
qualified calls (per the document's own "operation names live in their concept evidence" rule
above), so the shared name (`area`) across the two concepts never collides or needs disambiguating
at the call site either. Nothing here is new machinery — it falls directly out of the adapter model
already established for `JsonSerializable`/`Drawable`; it just hadn't been pointed at this specific,
real complaint before.

## 22. `stable-evidence` — opt-in per-operation keys, only for what actually crosses a boundary

Directly reopened from §21's `#NN` finding: Shammah pointed out real cases where slot stability
does matter — network dispatch and dynamic module loading, both scenarios where a `dyn` value's
evidence table is read by code that was never recompiled alongside it — but insisted it stay an
opt-in system, and corrected the mechanism itself: protobuf's actual guarantee is an arbitrary,
author-assigned key per field, decoupled from declaration order entirely, not positional stability
as the earlier phrasing implied.

```text
concept Range<T> stable-evidence {
    associated Item = T
    operation empty?    #1 -> bool
    operation front     #2 -> T
    operation pop-front #3
}

implementation MyListRange<T> : Range {
    operation empty?    = my-list-empty?
    operation front     = my-list-front
    operation pop-front = my-list-pop-front
}
```

An ordinary concept (no `stable-evidence`) still has no `#NN` syntax at all — this is not the
default, and §19's `Codec`, §21's `HasArea`/`Measurable`, and the original `Range`-without-modifier
examples elsewhere in the document are all still correct as plain, unnumbered concepts. The
distinction that decides which one a real concept needs: does any `dyn` value formed from it ever
get read by code that wasn't recompiled with it. `Range` used only within one program, one
compilation, needs nothing extra. A `Range` handed across an RPC boundary or loaded from a plugin
built against last month's revision of the concept does.

## 23. Superseded by coherence — kept as design history, not as the current rule

This section originally asked what stops an accidental copy-paste duplicate (`RectangleMeasurable2`,
identical to `RectangleMeasurable` in every binding) from coexisting with the original under a
different name, and answered it with a structural-identity check: reject two named implementations
of the same concept for the same type only when every binding matches, accept them the moment even
one genuinely differs (the same freedom canonical/compact JSON was assumed to need).

That whole premise is gone, not just patched further. A later conversation asked the more basic
question directly — why would anyone implement the same concept twice for the same type, doing
different things, on purpose? — and no surviving example was found; canonical/compact JSON collapses
into `JsonSerializable`'s own `options` parameter, and every other candidate (orderings, hashing
strategies) collapses into a parameter, a compile-time strategy, or a genuinely different concept the
same way. Once no real case remained, the whole naming apparatus this section's check was built on
went with it: implementations are unnamed, and coherence rejects a *second* implementation of a
concept for a type outright, regardless of whether its bindings differ from the first's:

```text
implementation Rectangle : Measurable {
    operation area      = rect-area
    operation perimeter = rect-perimeter
}

; REJECTED, unconditionally — not because the bindings happen to match, because Rectangle already
; has a Measurable implementation. A genuinely different area calculation still doesn't get in;
; it belongs on a distinct wrapper type instead (§15's ById(Account) is the worked version of this).
implementation Rectangle : Measurable {
    operation area      = rect-area-fast-approx
    operation perimeter = rect-perimeter
}
```

Worth keeping the superseded version on record rather than deleting it silently: the structural
check above was a real, reasonable answer to the question actually asked at the time, and it took a
sharper, more basic question — not a flaw found in the check itself — to reveal that the question
should never have been "how do we tell duplicates apart" at all.

## 24. Sized numeric types, signed indexing, explicit `cast`, and unconditional overflow traps

The primitive-type gap this section closes was real: before this pass, the only integer/float types
anywhere in the document were `int`/`uint` (both fixed 64-bit, "initially") and `float` (fixed
`binary64`, "initially") — no sized family, no stated overflow policy despite the semantic-profile
section explicitly flagging one as required, and no numeric-conversion rules at all. Verified against
a real D compiler along the way (not assumed): D's value-range propagation genuinely does check
width-narrowing better than C, but was confirmed to apply *no* check at all to same-width
signed/unsigned conversion — `uint y = -1;` compiles silently in D — exactly the bug class Bjarne
Stroustrup's "Subscripts and sizes should be signed" describes and a rejected D proposal tried and
failed to fix (Walter Bright declined it for reasons specific to fixing an existing 20-year-old
language, not because the bugs aren't real).

```lisp
(define (last-index (v : &vector<int>)) : int
  (- (len v) 1))                    ; empty vector -> -1, an obviously-wrong sentinel a caller can
                                     ; check, never a wrapped-to-huge-positive index (uint would)

(define (truncate-to-byte (x : int)) : u8
  (cast u8 x))                      ; explicit, required — x is a runtime value, not a
                                     ; compile-time-known constant

(let [ok (cast u8 200)]             ; compiles: literal 200 provably fits u8's range
  ok)

; (let [bad (cast u8 300)])         ; REJECTED at compile time — 300 provably does not fit u8

(define (checked-sum (a : int) (b : int)) : int
  (+ a b))                          ; traps on overflow, unconditionally, the same in every build --
                                     ; never Rust's release-mode silent wraparound
```

Retroactively confirms rather than corrects several earlier examples: `i64`, `u64`, `f64`, and `f32`
were used in a few places earlier this session (§19's `Codec` axiom, the `DistinctPair` axiom
example) before this vocabulary existed — those are now genuinely valid type names rather than
errors needing a fix, since `int`/`uint`/`float` are aliases for `i64`/`u64`/`f64`, not the only
names that exist.

## 25. Module identity from file path, `pkg` visibility, cross-frontend package tree

Directly from a design conversation about wanting D's disk-layout cleanliness without its actual
verified gap (a hand-written `module` declaration that can silently omit or drift from the file's
real location), plus a stated goal that CoLisp and Co-Forth be fully interoperable rather than two
separate module systems glued together.

```text
accounts/
  package.colisp        ; this directory's re-export surface — one canonical file, either frontend
  account.colisp        ; module accounts.account
  ledger.coforth         ; module accounts.ledger — Co-Forth, same package, no conflict
```

```lisp
; accounts/account.colisp
(record Account pub id: string balance: int)

(pkg (define (validate-balance (a : &Account)) : bool   ; visible anywhere under accounts/, not
  (>= (. a balance) 0)))                                  ; outside it — no separate `module` line
                                                            ; needed; this file's path IS accounts.account
```

```forth
\ accounts/ledger.coforth
import: accounts.account ;

: record-transaction ( S Account int -- S bool )
  \ calls the pkg-visible CoLisp helper directly — cross-frontend, no adapter, no ceremony
  over validate-balance
;
```

```lisp
; accounts/package.colisp
(export (from accounts.account :import (Account))
        (from accounts.ledger :import (record-transaction)))
```

```lisp
; outside accounts/ entirely
(import accounts)
(import accounts.account)

; (validate-balance some-account)   ; REJECTED — pkg-visible, not pub; only Account and
                                     ; record-transaction were exported from package.colisp
```

The interoperability claim is not aspirational sugar here — `ledger.coforth` calling
`validate-balance` (a CoLisp-defined, `pkg`-visible function) works because both frontends already
submit through one common elaborator into one typed IR (established in "One parse boundary, modules,
and packages"); by the time either symbol is resolvable, which frontend wrote it isn't part of what
resolution sees. The only new rule this needed was where `pkg` visibility's boundary sits — the
directory, checked the same way regardless of which file inside it is asking.

## 26. Attempting a fixed-size matrix kernel — hits a real, foundational, previously-unnoticed wall

Not a hypothetical: actually tried writing a compile-time-unrolled, monomorphized matrix-multiply
kernel using only already-established machinery (CTFE, `array<T,N>`, generics, "static evidence
generates like a template"), to see where a real numerics use case breaks the model — the same
method that found the templates-vs-generics resolution and the coherence rewrite.

```lisp
; Attempt 1: a fixed-size matrix record
(record Matrix<T, R, C>
  data : array<T, ???>)   ; STOPS HERE
```

This stops immediately, on the most basic possible step. `array<T,N>` has always implied `N` is
*some* kind of parameter (value-model table, session start), but there is no established syntax
anywhere in this document for declaring an ordinary compile-time integer as a generic parameter at
all — every generic parameter shown anywhere is a *type*. The one compile-time-value mechanism that
does exist, `values xs : Ts...` (parameter packs), is a heterogeneous pack tied to a corresponding
type pack — built for variadic argument lists, not for a single scalar dimension like a matrix's
row/column count, and there's no way to compute `R * C` at the type level from it even if it applied.

This is the actual blocker, not a stylistic gap: without a real value-generic-parameter mechanism,
nothing about fixed-size numeric types can be written at all — not just matrices, but anything
sized by a compile-time integer (a fixed-capacity buffer, a stack-allocated small-vector, an
`array<T,N>` used for anything beyond a literal-sized one). This needs its own resolved design
before kernel-generation specifically can go anywhere.

**A second, honest limitation worth flagging alongside it, not glossed over:** even once a kernel
monomorphizes per concrete dimension (which the already-established "generates like a template"
model would give for free once value parameters exist), turning an unrolled scalar loop into actual
SIMD instructions is the backend's auto-vectorizer's job, and the document's own stated native
backend is Cranelift — whose auto-vectorization is real but historically weaker than LLVM's. Whether
Finch needs explicit SIMD lane types (`f32x4`-style) as a deliberate, hand-tunable escape hatch, the
way real numerics libraries often want regardless of how good the auto-vectorizer is, is a second,
separate open question, not something "generates like a template" already answers.
