# Feature tour — small programs, real composition

Working CoLisp sketches, each exercising a cluster of features together rather than in isolation.
Purpose: find places where two independently-specified features don't actually compose. Where a
program needed something the spec doesn't define yet, that's called out inline as **GAP**, not
guessed at.

## 1. Records, construction, properties — now buildable against the ratified spec

Fixed 2026-09-17: the spec now has a bare `implementation Foo { ... }` (inherent implementation) as
the home for `constructor`/`get`/`set`/`operation` declarations — no `@constructor`/`@property`
attributes, no "associated function" category.

```text
(record Account
  pub id: string
  balance: int)                       ; module-private; no plain field named `balance` outside this module

(implementation Account
  (constructor open (id: string) -> Account =>
    (Account { id: id, balance: 0 }))

  (get balance (&self) -> int =>
    self.balance)

  (set balance (&mut self, v: int) -> () ! throws NegativeBalance =>
    (if (< v 0)
        (throw NegativeBalance)
        (set! self.balance v))))

(let ((a (Account.open "acct-1")))
  (assert-eq (a.balance) 0)
  (set! a.balance 50)          ; sugar for (Account.set-balance &mut a 50); goes through the setter, not a raw field write
  (assert-eq (a.balance) 50))
```

## 1b. Inherent operation shadows a concept operation of the same name

Stress-tests the new `.` resolution order (fields → get/set → inherent `operation` → concept
dispatch) added alongside the fix above.

```text
(concept Describable
  (operation describe (&self) -> string))

(implementation AccountDescribable for Account : Describable
  (operation describe (&self) => (format "Account({})" self.id)))

(implementation Account
  (operation describe (&self) -> string =>              ; inherent — same name as the concept operation
    (format "Account #{} (balance {})" self.id self.balance)))

(let ((a (Account.open "acct-2")))
  (assert-eq (a.describe) "Account #acct-2 (balance 0)"))  ; inherent wins — the concept impl is shadowed, not ambiguous
```

No gap: the precedence rule added to the `.` resolution passage answers this deterministically. Worth
noting as a real design tradeoff, not a defect: this means adding an inherent `operation` to a record
*after* a concept implementation already exists can silently change which body a caller reaches —
same hazard Rust accepts for inherent-vs-trait methods, inherited deliberately rather than by
oversight.

## 2. Ownership tour — borrow default, `steal`, `Unique`/`Shared`/`Weak`, `match-type`

```text
(fn describe (borrow x: Foo) -> string                 ; default: read-only, non-escaping
  (foo-name x))

(fn absorb (steal x: Foo) -> Foo                        ; ownership transfer
  x)

(fn log-carrier <O : Owner<Foo>> (steal x: O) -> string ! pure
  (match-type O
    (Unique<Foo> "exclusively owned")
    (Shared<Foo> (retain x) "shared, retained a handle")   ; get-mut would NOT be callable in this arm
    (_ "some other owner")))

(let ((u (Unique.new (Foo.default)))
      (s (Shared.new (Foo.default))))
  (describe (borrow u))               ; borrow, non-escaping — u still owns after this call
  (log-carrier (steal u))             ; ownership transferred; u is dead here on
  (log-carrier (steal s))             ; Shared's steal just moves the handle, not the payload
  (let ((w (Shared.downgrade s)))
    (match (Weak.upgrade w)
      (ok s2 (assert-eq (log-carrier (steal s2)) "shared, retained a handle"))
      (err _ (panic "unreachable: s is still alive")))))
```

This one composes cleanly: `match-type` narrowing, `steal` desugaring to `<O : Owner<Foo>>`, and
`Weak::upgrade`'s CAS-loop-backed `result` all line up with what's specified. No gap found.

## 3. Effects (`!`), `?` propagation, concepts together

```text
(concept JsonSerializable
  (associated Output = bytes)
  (operation serialize (&self, options: &JsonOptions) -> Output))

(record User pub id: string pub name: string)

(implementation UserJson for User : JsonSerializable
  (operation serialize (&self options) =>
    (UserCodec.serialize options self)))

(fn save-user (borrow u: User, opts: &JsonOptions) -> (result () IoError) ! throws IoError
  (let ((bytes (u.serialize opts)))     ; concept dispatch through `.`
    (? (fs-write "user.json" bytes))    ; `?` propagates IoError out of save-user
    (ok ())))

(fn save-user-pure-check () -> () ! pure
  ; would not typecheck if body called save-user — pure/throws mismatch caught here, not at the call site
  ())
```

No gap: `.` dispatching to a concept operation, `!`-unified effect rows, and `?` all compose as
documented. One thing worth flagging as a **readability** cost rather than a buildability bug: a
reader has to already know `User : JsonSerializable` is implemented somewhere else in the module to
know `u.serialize` resolves at all — nothing at the call site marks it as concept dispatch versus a
field/inherent-method hit. That's an argument for tooling (go-to-implementation), not a spec defect.

## 4. Variant (tagged union) with record-shaped arms + destructuring

```text
(variant WebEvent
  PageLoad
  PageUnload
  (KeyPress char)
  (Paste string)
  (Click { x: int, y: int }))          ; record-shaped arm

(fn handle (borrow e: WebEvent) -> string
  (match e
    (WebEvent.PageLoad "loaded")
    (WebEvent.PageUnload "unloaded")
    (WebEvent.KeyPress k (string-from-char k))
    (WebEvent.Paste s s)
    (WebEvent.Click { x, y } (format "click at {},{}" x y))))
```

No gap: this is the Rust `enum WebEvent {...}` example from earlier in the session, and it maps
directly onto one `variant` with mixed unit/tuple/record arms, matching the "D/TypeScript enums are
just all-unit variants" resolution. `match` destructuring on the record-shaped arm reads no
differently than destructuring an ordinary `record` value — no second mechanism needed.

## 5. What I did NOT write, and why

- **Capability requests with wildcarded paths** (`read{path="~/**"}`) — grepped the whole document;
  the only wildcard language present is a passing mention of "broad selectors such as workspace `**`"
  (line ~4136) with no grammar for it. This was asked about earlier in the session and, as far as I
  can tell, never actually got specified. Flagging rather than inventing syntax for it.
- **`borrow-mut`** — no worked declaration exists yet (already logged in `PROGRESS.md` as blocked on
  you naming the primitive), so no example calls it.
- **Explicit discriminant/`repr` for variants** — not specified, so `WebEvent` above has no `repr`
  clause; skipped rather than guessed.
- **Variadic/parameter-pack example** (`types Ts...`, `params ps...`, `rest<T>`) — not attempted yet;
  next candidate for this file once the constructor/property fix above is settled, since a
  variadic-forwarding constructor is the natural place those two features meet.
