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

(let ((a (Account.open "acct-1")))
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

(let ((a (Account.open "acct-2")))
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

(define (log-carrier (steal x : O)) : string                 ; UNVERIFIED: generic-header placement
  ! pure                                                      ; for a hand-written `<O : Owner<Foo>>`
  (match-type O                                               ; is not shown anywhere in the document —
    (Unique<Foo> "exclusively owned")                         ; every real match-type example matches on
    (Shared<Foo> (retain x) "shared, retained a handle")      ; a parameter already in scope, never shows
    (_ "some other owner")))                                  ; the enclosing signature that bound it.

(let ((u (Unique.new (Foo.default)))
      (s (Shared.new (Foo.default))))
  (describe u)                        ; borrow, non-escaping — u still owns after this call
  (log-carrier (steal u))             ; ownership transferred; u is dead from here on
  (log-carrier (steal s))             ; Shared's steal just moves the handle, not the payload
  (let ((w (Shared.downgrade s)))
    (match (Weak.upgrade w)
      (ok s2 (assert-eq (log-carrier (steal s2)) "shared, retained a handle"))
      (err _ (panic "unreachable: s is still alive")))))
```

Composes cleanly apart from the flagged header-placement uncertainty: `match-type` narrowing, `steal`
desugaring to `<O : Owner<Foo>>`, and `Weak::upgrade`'s CAS-loop-backed `result` all line up with
what's specified.

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
  (let ((bytes (serialize u opts)))          ; bare-name concept dispatch + one default-imported evidence,
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

## 5. What I did NOT write, and why

- **Capability requests with wildcarded paths** (`read{path="~/**"}`) — grepped the whole document;
  the only wildcard language present is a passing mention of "broad selectors such as workspace `**`"
  with no grammar for it. Asked about earlier this session and, as far as I can tell, never actually
  specified. Flagging rather than inventing syntax.
- **`borrow-mut` and in-place field mutation** — see the GAP note under §1; now understood as one
  combined open item, not two.
- **Explicit discriminant/`repr` for variants** — not specified, so `WebEvent` above has no `repr`
  clause.
- **Variadic/parameter-pack example** (`types Ts...`, `params ps...`, `rest<T>`) — not attempted yet;
  next candidate once a real generic-header CoLisp example resolves the §2 UNVERIFIED note, since a
  variadic-forwarding constructor is exactly where packs and generic headers meet.
