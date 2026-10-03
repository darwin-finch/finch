#!/usr/bin/env python3
"""Authoring source for the hand-written fields of the language fixtures.

Each vector is written here once in CoLisp, C-like, and Co-Forth.  Run from ``scripts/language``:

    python3 author_vectors.py && python3 check_language_spec.py --write

The first command rewrites the id, source spellings, context, and notes in
``fixtures/execution-vectors.json``, ``static-rejections.json``, and ``session-vectors.json``;
the second regenerates every derived field.  Review the fixture diff afterwards.
"""
import json
OPS={"log":{"kind":"emit","parameters":1,"result":False},"read-file":{"kind":"effect","parameters":1,"result":True},"sleep":{"kind":"await","parameters":1,"result":True}}
def ctx(host=None,lifecycle=None,ops=("log",)):
    c={"operations":{k:OPS[k] for k in ops}}
    if lifecycle: c["lifecycle"]=lifecycle
    if host: c["host"]=host
    return c
def msg(req,seq,outcome,gen=0): return {"request":req,"generation":gen,"sequence":seq,"outcome":outcome}
TOK=["Token"]
V=[]
def v(id,cl,cf,context=None,why=None,note=None):
    d={"id":id}
    if note: d["note"]=note
    d["colisp"]=cl; d["coforth"]=cf
    if cf is None: d["unpaired_reason"]=why
    if context: d["context"]=context
    V.append(d)
NOCALL="Co-Forth 0.1 has no word that invokes a callable value held in a local; see DESIGN_REVIEW finding 63."
# --- values, sequencing, branching
v("literal-return","42","42")
v("string-literal-escapes",'"a\\n\\u{e9}\\x41"','"a\\n\\u{e9}\\x41"')
v("negative-literal-is-unary-negation","-7","-7")
v("left-to-right-add","(+ 20 22)","20 22 +")
v("argument-effects-run-left-to-right",'(begin (log "left") (+ 1 (begin (log "right") 2)))','"left" log 1 "right" log 2 +',ctx())
v("division-truncates-toward-zero","(/ -7 2)","-7 2 /")
v("conditional-selects-then","(if true 7 9)","true if 7 else 9 then")
v("conditional-selects-else","(if false 7 9)","false if 7 else 9 then")
v("empty-sequence-is-unit","(begin)",None,why="Co-Forth has no empty-sequence spelling; an empty word list elaborates to the unit literal.")
v("discarded-sequence-value-is-dropped","(begin (Token :id 1) 7)","Token{ id: 1 } drop 7",ctx(lifecycle=TOK,ops=()))
# --- bindings, assignment, ownership
v("assignment-rhs-first","(let [mut x : int 1] (begin (set! x 2) x))","{ -- mut x: int } 1 to x 2 to x x")
v("assignment-rhs-failure-keeps-old-value","(define (boom) : int ! inferred (throw 5)) (let [mut x : int 1] (try (begin (set! x (+ 1 (boom))) x) (catch _ x)))",": boom ( -- int ! inferred ) 5 throw ; { -- mut x: int } 1 to x try 1 boom + to x x catch _ of x endof endtry")
v("assignment-drops-replaced-value","(let [mut a : Token (Token :id 1)] (begin (set! a (Token :id 2)) 7))","{ -- mut a: Token } Token{ id: 1 } to a Token{ id: 2 } to a 7",ctx(lifecycle=TOK,ops=()))
v("bindings-drop-in-reverse-order","(let [a : Token (Token :id 1) b : Token (Token :id 2)] 7)","Token{ id: 1 } Token{ id: 2 } { a: Token b: Token -- } 7",ctx(lifecycle=TOK,ops=()))
v("move-transfers-the-drop-obligation","(let [a : Token (Token :id 1)] (let [b : Token a] 7))","Token{ id: 1 } { a: Token -- } a { b: Token -- } 7",ctx(lifecycle=TOK,ops=()))
v("borrow-then-steal-parameter","(define (peek (t : Token)) : int ! plain 1) (define (eat (steal t : Token)) : int ! plain 2) (let [a : Token (Token :id 1)] (+ (peek a) (eat a)))",": peek ( t: Token -- int ! plain ) 1 ; : eat ( steal t: Token -- int ! plain ) 2 ; Token{ id: 1 } { a: Token -- } a peek a eat +",ctx(lifecycle=TOK,ops=()))
v("exclusive-borrow-writes-through","(define (bump (borrow-mut n : int)) : unit ! plain (set! n (+ n 1))) (let [mut x : int 1] (begin (bump x) x))",": bump ( borrow-mut n: int -- ! plain ) n 1 + to n ; { -- mut x: int } 1 to x x bump x")
# --- loops
v("while-loop-state-threading","(let [mut x : int 0] (begin (while (< x 3) (set! x (+ x 1))) x))","{ -- mut x: int } 0 to x begin x 3 < while x 1 + to x repeat x")
v("while-zero-iterations","(let [mut x : int 0] (begin (while (< x 0) (set! x (+ x 1))) x))","{ -- mut x: int } 0 to x begin x 0 < while x 1 + to x repeat x")
v("break-runs-exited-scope-cleanup","(let [mut x : int 0] (begin (while true (scope (on-exit (set! x (+ x 10))) (break))) x))","{ -- mut x: int } 0 to x begin true while scope on-exit [ ( -- ) | x 10 + to x ] break endscope repeat x")
v("continue-skips-rest-of-body","(let [mut x : int 0 mut y : int 0] (begin (while (< x 3) (set! x (+ x 1)) (if (== x 2) (continue) ()) (set! y (+ y x))) y))","{ -- mut x: int mut y: int } 0 to x 0 to y begin x 3 < while x 1 + to x x 2 == if continue then y x + to y repeat y")
v("break-leaves-through-try-and-scope",'(while true (scope (on-exit (log "outer")) (try (break) (catch _ ()))))','begin true while scope on-exit [ ( -- ) | "outer" log ] try break catch _ of endof endtry endscope repeat',ctx())
v("cancellation-at-loop-safepoint",'(scope (on-exit (log "a")) (on-success (log "s")) (on-failure (log "f")) (on-cancel (log "c")) (while true (log "tick")))','scope on-exit [ ( -- ) | "a" log ] on-success [ ( -- ) | "s" log ] on-failure [ ( -- ) | "f" log ] on-cancel [ ( -- ) | "c" log ] begin true while "tick" log repeat endscope',ctx(host={"cancel_at_safepoint":1}))
# --- calls
v("call-enters-and-returns","(define (inc (x : int)) : int ! plain (+ x 1)) (+ (inc 40) 1)",": inc ( x: int -- int ! plain ) x 1 + ; 40 inc 1 +")
v("early-return-leaves-function","(define (clamp (x : int)) : int ! plain (if (< x 0) (return 0) ()) x) (+ (clamp -5) (clamp 5))",": clamp ( x: int -- int ! plain ) x 0 < if 0 return then x ; -5 clamp 5 clamp +")
v("return-runs-success-guards",'(define (f) : int ! inferred (scope (on-success (log "s")) (on-failure (log "f")) (return 1))) (+ (f) 0)',': f ( -- int ! inferred ) scope on-success [ ( -- ) | "s" log ] on-failure [ ( -- ) | "f" log ] 1 return endscope ; f 0 +',ctx())
v("self-tail-call-keeps-frame-count","(define (count (n : int) (acc : int)) : int ! plain (if (== n 0) acc (count (- n 1) (+ acc 1)))) (+ (count 3 0) 0)",": count ( n: int acc: int -- int ! plain ) n 0 == if acc else n 1 - acc 1 + count then ; 3 0 count 0 +")
v("mutual-tail-calls-keep-frame-count","(define (even? (n : int)) : bool ! plain (if (== n 0) true (odd? (- n 1)))) (define (odd? (n : int)) : bool ! plain (if (== n 0) false (even? (- n 1)))) (if (even? 3) 1 0)",": even? ( n: int -- bool ! plain ) n 0 == if true else n 1 - odd? then ; : odd? ( n: int -- bool ! plain ) n 0 == if false else n 1 - even? then ; 3 even? if 1 else 0 then")
v("tail-call-runs-cleanup-before-transfer",'(define (g (x : int)) : int ! inferred x) (define (f (x : int)) : int ! inferred (scope (on-exit (log "before-transfer")) (g x))) (+ (f 1) 0)',': g ( x: int -- int ! inferred ) x ; : f ( x: int -- int ! inferred ) scope on-exit [ ( -- ) | "before-transfer" log ] x g endscope ; 1 f 0 +',ctx())
v("root-tail-call-replaces-entry-frame","(define (inc (x : int)) : int ! plain (+ x 1)) (inc 41)",": inc ( x: int -- int ! plain ) x 1 + ; 41 inc")
v("closure-call-copies-capture","(let [n : int 5] (let [f : callable<(int) -> int> (lambda (:captures (copy n)) ((x : int)) ! plain (+ x n))] (+ (f 1) 0)))","5 { n: int -- } [ captures: { copy n } ( x: int -- int ! plain ) | x n + ] { f: callable<(int) -> int> -- } 1 f 0 +")
v("borrowed-temporary-outlives-the-call","(define (peek (t : Token)) : int ! plain 1) (+ (peek (Token :id 1)) 1)",": peek ( t: Token -- int ! plain ) 1 ; Token{ id: 1 } peek 1 +",ctx(lifecycle=TOK,ops=()))
v("tail-call-adopts-borrowed-temporary","(define (peek (t : Token)) : int ! plain 1) (peek (Token :id 1))",": peek ( t: Token -- int ! plain ) 1 ; Token{ id: 1 } peek",ctx(lifecycle=TOK,ops=()))
v("closure-indirect-tail-call","(define (apply (f : callable<(int) -> int>) (x : int)) : int ! inferred (f x)) (+ (apply (lambda ((x : int)) ! plain (+ x 1)) 41) 0)",": apply ( f: callable<(int) -> int> x: int -- int ! inferred ) x f ; [ ( x: int -- int ! plain ) | x 1 + ] 41 apply 0 +")
v("closure-move-capture-owns-value","(let [t : Token (Token :id 1)] (let [f : callable<() -> int> (lambda :move () ! plain (match t (_ 7)))] (+ (f) 0)))","Token{ id: 1 } { t: Token -- } [ captures: move ( -- int ! plain ) | t match _ of 7 endof endmatch ] { f: callable<() -> int> -- } f 0 +",ctx(lifecycle=TOK,ops=()))
v("anonymous-callable-applied-with-call","((lambda ((x : int)) ! plain (+ x 1)) 41)","41 [ ( x: int -- int ! plain ) | x 1 + ] call")
v("defined-function-passed-by-reference","(define (work) : int ! plain 7) (join (spawn work))",": work ( -- int ! plain ) 7 ; ' work spawn join")
# --- matching
v("match-selects-first-arm","(match 1 (1 10) (_ 20))","1 match 1 of 10 endof _ of 20 endof endmatch")
v("match-selects-default-arm","(match 2 (1 10) (_ 20))","2 match 1 of 10 endof _ of 20 endof endmatch")
v("match-binds-scrutinee","(match 5 (n (+ n 1)))","5 match n of n 1 + endof endmatch")
v("borrow-match-drops-owned-scrutinee-after-body","(match (Token :id 1) (_ 7))","Token{ id: 1 } match _ of 7 endof endmatch",ctx(lifecycle=TOK,ops=()))
v("ownership-match-distributes-fields","(match :steal (Pair :a (Token :id 1) :b (Token :id 2)) ((Pair :a t :b _) 7))","Pair{ a: Token{ id: 1 } b: Token{ id: 2 } } steal match Pair{ a: t b: _ } of 7 endof endmatch",ctx(lifecycle=TOK,ops=()))
v("ownership-match-wildcard-drops-whole-value","(match :steal (Token :id 1) (_ 7))","Token{ id: 1 } steal match _ of 7 endof endmatch",ctx(lifecycle=TOK,ops=()))
# --- exceptions
v("uncaught-throw-fails","(throw 7)","7 throw")
v("try-completes-normally","(try 7 (catch _ 9))","try 7 catch _ of 9 endof endtry")
v("try-catches-raised-value","(try (throw 7) (catch _ 9))","try 7 throw catch _ of 9 endof endtry")
v("catch-binds-exception-value","(try (throw 7) (catch e (+ e 1)))","try 7 throw catch e of e 1 + endof endtry")
v("catch-clauses-tested-in-order","(try (throw 7) (catch 1 10) (catch 7 20))","try 7 throw catch 1 of 10 endof catch 7 of 20 endof endtry")
v("unmatched-catch-keeps-original-envelope","(try (throw 7) (catch 1 10))","try 7 throw catch 1 of 10 endof endtry")
v("rethrow-preserves-active-value","(try (throw 7) (catch _ (rethrow)))","try 7 throw catch _ of rethrow endof endtry")
v("throw-in-catch-arm-targets-outer-handler","(try (try (throw 1) (catch _ (throw 2))) (catch e e))","try try 1 throw catch _ of 2 throw endof endtry catch e of e endof endtry")
v("thrown-value-dropped-after-handler","(try (throw (Token :id 1)) (catch _ 7))","try Token{ id: 1 } throw catch _ of 7 endof endtry",ctx(lifecycle=TOK,ops=()))
v("throw-unwinds-call-frames","(define (boom (x : int)) : int ! inferred (throw x)) (try (+ (boom 3) 1) (catch e e))",": boom ( x: int -- int ! inferred ) x throw ; try 3 boom 1 + catch e of e endof endtry")
# --- scope guards
G='(scope (on-exit (log "a")) (on-success (log "s")) (on-failure (log "f")) (on-cancel (log "c")) %s)'
GF='scope on-exit [ ( -- ) | "a" log ] on-success [ ( -- ) | "s" log ] on-failure [ ( -- ) | "f" log ] on-cancel [ ( -- ) | "c" log ] %s endscope'
v("scope-runs-success-cleanup-lifo",G%"7",GF%"7",ctx())
v("scope-runs-failure-cleanup-lifo",G%"(throw 1)",GF%"1 throw",ctx())
v("guard-failure-on-success-exit-becomes-primary",'(scope (on-success (log "s")) (on-failure (log "f")) (on-exit (throw 9)) 7)','scope on-success [ ( -- ) | "s" log ] on-failure [ ( -- ) | "f" log ] on-exit [ ( -- ) | 9 throw ] 7 endscope',ctx())
v("guard-failure-during-failure-is-suppressed",'(scope (on-exit (log "outer")) (on-exit (throw 9)) (throw 1))','scope on-exit [ ( -- ) | "outer" log ] on-exit [ ( -- ) | 9 throw ] 1 throw endscope',ctx())
v("guard-failure-during-cancel-is-suppressed",'(scope (on-exit (log "outer")) (on-cancel (throw 9)) (while true ()))','scope on-exit [ ( -- ) | "outer" log ] on-cancel [ ( -- ) | 9 throw ] begin true while repeat endscope',ctx(host={"cancel_at_safepoint":0}))
v("guard-failure-during-break-becomes-primary",'(try (while true (scope (on-exit (throw 9)) (break))) (catch e e))',None,why="The result type joins the loop's unit with the caught int only through a typed catch; kept single-spelling because the Co-Forth arms would leave different stack depths.")
# --- records
v("record-construction-evaluates-fields-in-order","(Point :x (+ 1 2) :y 4)","Point{ x: 1 2 + y: 4 }")
v("record-construction-failure-drops-initialized-prefix","(define (boom) : int ! inferred (throw 7)) (Pair :a (Token :id 1) :b (boom))",": boom ( -- int ! inferred ) 7 throw ; Pair{ a: Token{ id: 1 } b: boom }",ctx(lifecycle=TOK,ops=()))
# --- traps
v("integer-overflow-traps","(+ 9223372036854775807 1)","9223372036854775807 1 +")
v("divide-by-zero-trap-runs-failure-cleanup",'(scope (on-failure (log "f")) (on-cancel (log "c")) (/ 1 0))','scope on-failure [ ( -- ) | "f" log ] on-cancel [ ( -- ) | "c" log ] 1 0 / endscope',ctx())
v("trap-is-not-catchable","(try (/ 1 0) (catch _ 9))","try 1 0 / catch _ of 9 endof endtry")
# --- host boundary
E=("log","read-file","sleep")
v("emit-journals-before-exposure",'(log "hello")','"hello" log',ctx())
v("emitted-event-survives-failure",'(begin (log "visible") (throw 1))','"visible" log 1 throw',ctx())
v("effect-request-parks-until-resumed",'(read-file "fixture.txt")','"fixture.txt" read-file',ctx(host={"grants":{"read-file":True}},ops=E))
v("effect-resumes-with-value",'(+ (read-file "f") 1)','"f" read-file 1 +',ctx(host={"grants":{"read-file":True},"resumes":[msg("read-file#0",0,{"value":5})]},ops=E))
v("effect-resume-raise-is-catchable",'(try (read-file "f") (catch e e))','try "f" read-file catch e of e endof endtry',ctx(host={"grants":{"read-file":True},"resumes":[msg("read-file#0",0,{"raise":3})]},ops=E))
v("effect-without-grant-is-denied",'(scope (on-failure (log "f")) (read-file "f"))','scope on-failure [ ( -- ) | "f" log ] "f" read-file endscope',ctx(host={"grants":{}},ops=E))
v("effect-resume-cancel-runs-cancel-guards",'(scope (on-cancel (log "c")) (on-failure (log "f")) (read-file "f"))','scope on-cancel [ ( -- ) | "c" log ] on-failure [ ( -- ) | "f" log ] "f" read-file endscope',ctx(host={"grants":{"read-file":True},"resumes":[msg("read-file#0",0,{"protected":"Cancel"})]},ops=E))
v("effect-resume-resource-exhausted",'(scope (on-failure (log "f")) (read-file "f"))','scope on-failure [ ( -- ) | "f" log ] "f" read-file endscope',ctx(host={"grants":{"read-file":True},"resumes":[msg("read-file#0",0,{"protected":"ResourceExhausted","detail":"host-quota"})]},ops=E))
v("hostile-resumes-are-rejected-or-replayed",'(+ (read-file "a") (read-file "b"))','"a" read-file "b" read-file +',ctx(host={"grants":{"read-file":True},"resumes":[
  msg("read-file#0",0,{"value":1},gen=9), msg("read-file#0",5,{"value":1}), msg("read-file#1",0,{"value":1}), msg("read-file#0",0,{"value":1}),
  msg("read-file#0",0,{"value":1}), msg("read-file#0",0,{"value":99}), msg("read-file#1",1,{"value":2}), msg("read-file#1",1,{"value":2})]},ops=E),
  note="Messages in order: stale generation, skipped sequence, unsolicited request identity, accepted, exact duplicate, conflicting duplicate, accepted, post-terminal.")
v("await-request-resumes-with-value",'(+ (sleep 1) 1)','1 sleep 1 +',ctx(host={"resumes":[msg("sleep#0",0,{"value":0})]},ops=E))
v("await-request-parks",'(sleep 1)','1 sleep',ctx(ops=E))
v("await-resume-raise-fails",'(sleep 1)','1 sleep',ctx(host={"resumes":[msg("sleep#0",0,{"raise":4})]},ops=E))
v("await-resume-cancel",'(sleep 1)','1 sleep',ctx(host={"resumes":[msg("sleep#0",0,{"protected":"Cancel"})]},ops=E))
# --- fibers
# --- tasks
v("spawn-publishes-handle-before-child-runs",'(let [t (spawn (lambda :move () ! inferred (log "child") 7))] (begin (log "parent") (join t)))','[ captures: move ( -- int ! inferred ) | "child" log 7 ] spawn { t -- } "parent" log t join',ctx())
v("spawn-capacity-failure-leaves-callable-with-parent",'(scope (on-failure (log "f")) (let [k : Token (Token :id 1)] (spawn (lambda :move () ! plain (match k (_ 7))))))','scope on-failure [ ( -- ) | "f" log ] Token{ id: 1 } { k: Token -- } [ captures: move ( -- int ! plain ) | k match _ of 7 endof endmatch ] spawn endscope',ctx(host={"task_capacity":0},lifecycle=TOK))
v("join-propagates-child-exception",'(try (join (spawn (lambda :move () ! inferred (scope (on-exit (log "child-cleanup")) (throw 4))))) (catch e e))','try [ captures: move ( -- int ! inferred ) | scope on-exit [ ( -- ) | "child-cleanup" log ] 4 throw endscope ] spawn join catch e of e endof endtry',ctx())
v("join-propagates-child-trap",'(join (spawn (lambda :move () ! plain (/ 1 0))))','[ captures: move ( -- int ! plain ) | 1 0 / ] spawn join')
v("child-grant-is-attenuated-by-host-policy",'(join (spawn (lambda :move () ! inferred (read-file "f"))))','[ captures: move ( -- int ! inferred ) | "f" read-file ] spawn join',ctx(host={"grants":{"read-file":True},"child_grants":{}},ops=E))
v("child-effect-resumes-through-parent-join",'(+ (join (spawn (lambda :move () ! inferred (read-file "f")))) 1)','[ captures: move ( -- int ! inferred ) | "f" read-file ] spawn join 1 +',ctx(host={"grants":{"read-file":True},"child_grants":{"read-file":True},"resumes":[msg("read-file#0",0,{"value":8})]},ops=E))
v("cancel-of-unstarted-task-drops-its-captures",'(let [k : Token (Token :id 1)] (let [t (spawn (lambda :move () ! plain (match k (_ 7))))] (begin (cancel t) 1)))','Token{ id: 1 } { k: Token -- } [ captures: move ( -- int ! plain ) | k match _ of 7 endof endmatch ] spawn { t -- } t cancel 1',ctx(lifecycle=TOK,ops=()))
v("dropped-task-handle-goes-to-reaper",'(let [t (spawn (lambda :move () ! plain 7))] 1)','[ captures: move ( -- int ! plain ) | 7 ] spawn { t -- } 1')
v("grant-revoked-between-dispatches-denies-the-next",'(scope (on-failure (log "f")) (+ (read-file "a") (read-file "b")))','scope on-failure [ ( -- ) | "f" log ] "a" read-file "b" read-file + endscope',ctx(host={"grants":{"read-file":True},"revocations":[{"after_sequence":0,"operation":"read-file"}],"resumes":[msg("read-file#0",0,{"value":1})]},ops=E))
v("child-grant-revoked-mid-run-denies-its-next-dispatch",'(join (spawn (lambda :move () ! inferred (+ (read-file "a") (read-file "b")))))','[ captures: move ( -- int ! inferred ) | "a" read-file "b" read-file + ] spawn join',ctx(host={"grants":{"read-file":True},"child_grants":{"read-file":True},"revocations":[{"after_sequence":0,"operation":"read-file"}],"resumes":[msg("read-file#0",0,{"value":1})]},ops=E))
v("effectful-callable-type-carries-its-contract",'(define (twice (f : callable<(int) -> int ! effects-infer | nothrow | non-suspending>) (x : int)) : int ! inferred (f (f x))) (+ (twice (lambda ((x : int)) ! plain (+ x 1)) 40) 0)',': twice ( f: callable<(int) -> int ! effects-infer | nothrow | non-suspending> x: int -- int ! inferred ) x f f ; [ ( x: int -- int ! plain ) | x 1 + ] 40 twice 0 +')
# --- variants and members
VD="(variant Shape (Empty) (Circle int)) "; VF="variant: Shape cases{ Empty Circle( int ) } ; "
v("variant-construction-with-payload",VD+"(match (Circle 3) ((Circle r) r) ((Empty) 0))",VF+"3 Circle match Circle( r ) of r endof Empty( ) of 0 endof endmatch")
v("variant-construction-without-payload",VD+"(match (Empty) ((Circle r) r) ((Empty) 0))",VF+"Empty match Circle( r ) of r endof Empty( ) of 0 endof endmatch")
v("member-read-copies-a-copy-field","(let [p : Point (Point :x 3 :y 4)] (+ (. p x) (. p y)))","Point{ x: 3 y: 4 } { p: Point -- } p .x p .y +")
v("member-read-of-temporary-drops-the-aggregate","(. (Pair :a 2 :b (Token :id 1)) a)","Pair{ a: 2 b: Token{ id: 1 } } .a",ctx(lifecycle=TOK,ops=()))
v("member-read-borrows-a-non-copy-field","(define (peek (t : Token)) : int ! plain 1) (let [p : Pair (Pair :a (Token :id 1) :b 2)] (+ (peek (. p a)) (. p b)))",": peek ( t: Token -- int ! plain ) 1 ; Pair{ a: Token{ id: 1 } b: 2 } { p: Pair -- } p .a peek p .b +",ctx(lifecycle=TOK,ops=()))

CLIKE={'literal-return': '42', 'string-literal-escapes': '"a\\n\\u{e9}\\x41"', 'negative-literal-is-unary-negation': '-7', 'left-to-right-add': '20 + 22', 'argument-effects-run-left-to-right': 'log("left"); 1 + { log("right"); 2 }', 'division-truncates-toward-zero': '-7 / 2', 'conditional-selects-then': 'if (true) 7 else 9', 'conditional-selects-else': 'if (false) 7 else 9', 'empty-sequence-is-unit': '{ }', 'discarded-sequence-value-is-dropped': 'Token { id: 1 }; 7', 'assignment-rhs-first': 'mut int x = 1; x = 2; x', 'assignment-rhs-failure-keeps-old-value': 'int boom() inferred { throw 5 } mut int x = 1; try { x = 1 + boom(); x } catch (_) x', 'assignment-drops-replaced-value': 'mut Token a = Token { id: 1 }; a = Token { id: 2 }; 7', 'bindings-drop-in-reverse-order': 'Token a = Token { id: 1 }; Token b = Token { id: 2 }; 7', 'move-transfers-the-drop-obligation': 'Token a = Token { id: 1 }; Token b = a; 7', 'borrow-then-steal-parameter': 'int peek(Token t) plain { 1 } int eat(move Token t) plain { 2 } Token a = Token { id: 1 }; peek(a) + eat(a)', 'exclusive-borrow-writes-through': 'unit bump(mut int n) plain { n = n + 1 } mut int x = 1; bump(x); x', 'while-loop-state-threading': 'mut int x = 0; while (x < 3) x = x + 1; x', 'while-zero-iterations': 'mut int x = 0; while (x < 0) x = x + 1; x', 'break-runs-exited-scope-cleanup': 'mut int x = 0; while (true) scope { onExit x = x + 10; break }; x', 'continue-skips-rest-of-body': 'mut int x = 0; mut int y = 0; while (x < 3) { x = x + 1; if (x == 2) continue; y = y + x }; y', 'break-leaves-through-try-and-scope': 'while (true) scope { onExit log("outer"); try break catch (_) () }', 'cancellation-at-loop-safepoint': 'scope { onExit log("a"); onSuccess log("s"); onFailure log("f"); onCancel log("c"); while (true) log("tick") }', 'call-enters-and-returns': 'int inc(int x) plain { x + 1 } inc(40) + 1', 'early-return-leaves-function': 'int clamp(int x) plain { if (x < 0) return 0; x } clamp(-5) + clamp(5)', 'return-runs-success-guards': 'int f() inferred { scope { onSuccess log("s"); onFailure log("f"); return 1 } } f() + 0', 'self-tail-call-keeps-frame-count': 'int count(int n, int acc) plain { if (n == 0) acc else count(n - 1, acc + 1) } count(3, 0) + 0', 'mutual-tail-calls-keep-frame-count': 'bool `even?`(int n) plain { if (n == 0) true else `odd?`(n - 1) } bool `odd?`(int n) plain { if (n == 0) false else `even?`(n - 1) } if (`even?`(3)) 1 else 0', 'tail-call-runs-cleanup-before-transfer': 'int g(int x) inferred { x } int f(int x) inferred { scope { onExit log("before-transfer"); g(x) } } f(1) + 0', 'root-tail-call-replaces-entry-frame': 'int inc(int x) plain { x + 1 } inc(41)', 'closure-call-copies-capture': 'int n = 5; int function(int) f = [copy n](int x) plain => x + n; f(1) + 0', 'borrowed-temporary-outlives-the-call': 'int peek(Token t) plain { 1 } peek(Token { id: 1 }) + 1', 'tail-call-adopts-borrowed-temporary': 'int peek(Token t) plain { 1 } peek(Token { id: 1 })', 'closure-indirect-tail-call': 'int apply(int function(int) f, int x) inferred { f(x) } apply((int x) plain => x + 1, 41) + 0', 'closure-move-capture-owns-value': 'Token t = Token { id: 1 }; int function() f = [move]() plain => match (t) { _ => 7 }; f() + 0', 'anonymous-callable-applied-with-call': '((int x) plain => x + 1)(41)', 'defined-function-passed-by-reference': 'int work() plain { 7 } join(spawn(work))', 'match-selects-first-arm': 'match (1) { 1 => 10, _ => 20 }', 'match-selects-default-arm': 'match (2) { 1 => 10, _ => 20 }', 'match-binds-scrutinee': 'match (5) { n => n + 1 }', 'borrow-match-drops-owned-scrutinee-after-body': 'match (Token { id: 1 }) { _ => 7 }', 'ownership-match-distributes-fields': 'match move (Pair { a: Token { id: 1 }, b: Token { id: 2 } }) { Pair { a: t, b: _ } => 7 }', 'ownership-match-wildcard-drops-whole-value': 'match move (Token { id: 1 }) { _ => 7 }', 'uncaught-throw-fails': 'throw 7', 'try-completes-normally': 'try 7 catch (_) 9', 'try-catches-raised-value': 'try throw 7 catch (_) 9', 'catch-binds-exception-value': 'try throw 7 catch (e) e + 1', 'catch-clauses-tested-in-order': 'try throw 7 catch (1) 10 catch (7) 20', 'unmatched-catch-keeps-original-envelope': 'try throw 7 catch (1) 10', 'rethrow-preserves-active-value': 'try throw 7 catch (_) rethrow', 'throw-in-catch-arm-targets-outer-handler': 'try { try throw 1 catch (_) throw 2 } catch (e) e', 'thrown-value-dropped-after-handler': 'try throw Token { id: 1 } catch (_) 7', 'throw-unwinds-call-frames': 'int boom(int x) inferred { throw x } try boom(3) + 1 catch (e) e', 'scope-runs-success-cleanup-lifo': 'scope { onExit log("a"); onSuccess log("s"); onFailure log("f"); onCancel log("c"); 7 }', 'scope-runs-failure-cleanup-lifo': 'scope { onExit log("a"); onSuccess log("s"); onFailure log("f"); onCancel log("c"); throw 1 }', 'guard-failure-on-success-exit-becomes-primary': 'scope { onSuccess log("s"); onFailure log("f"); onExit throw 9; 7 }', 'guard-failure-during-failure-is-suppressed': 'scope { onExit log("outer"); onExit throw 9; throw 1 }', 'guard-failure-during-cancel-is-suppressed': 'scope { onExit log("outer"); onCancel throw 9; while (true) () }', 'guard-failure-during-break-becomes-primary': 'try while (true) scope { onExit throw 9; break } catch (e) e', 'record-construction-evaluates-fields-in-order': 'Point { x: 1 + 2, y: 4 }', 'record-construction-failure-drops-initialized-prefix': 'int boom() inferred { throw 7 } Pair { a: Token { id: 1 }, b: boom() }', 'integer-overflow-traps': '9223372036854775807 + 1', 'divide-by-zero-trap-runs-failure-cleanup': 'scope { onFailure log("f"); onCancel log("c"); 1 / 0 }', 'trap-is-not-catchable': 'try 1 / 0 catch (_) 9', 'emit-journals-before-exposure': 'log("hello")', 'emitted-event-survives-failure': 'log("visible"); throw 1', 'effect-request-parks-until-resumed': 'readFile("fixture.txt")', 'effect-resumes-with-value': 'readFile("f") + 1', 'effect-resume-raise-is-catchable': 'try readFile("f") catch (e) e', 'effect-without-grant-is-denied': 'scope { onFailure log("f"); readFile("f") }', 'effect-resume-cancel-runs-cancel-guards': 'scope { onCancel log("c"); onFailure log("f"); readFile("f") }', 'effect-resume-resource-exhausted': 'scope { onFailure log("f"); readFile("f") }', 'hostile-resumes-are-rejected-or-replayed': 'readFile("a") + readFile("b")', 'await-request-resumes-with-value': 'sleep(1) + 1', 'await-request-parks': 'sleep(1)', 'await-resume-raise-fails': 'sleep(1)', 'await-resume-cancel': 'sleep(1)', 'fiber-construction-does-not-run-body': 'auto f = fiber (int reply) inferred { log("ran"); 1 }; 7', 'fiber-yield-returns-one-successor': 'match move (step(fiber (int reply) inferred { yield 1; 3 }, 0)) { yielded(v, next) => v, returned(r) => r }', 'fiber-return-has-no-successor': 'match move (step(fiber (int reply) inferred { reply }, 5)) { returned(r) => r, yielded(v, _) => v }', 'fiber-resume-value-is-yield-result': 'auto f = fiber (int reply) inferred { (yield 1) + 100 }; match move (step(f, 0)) { yielded(v, next) => match move (step(next, 7)) { returned(r) => v + r, yielded(_, _) => 0 }, returned(r) => r }', 'fiber-raise-consumes-handle-and-propagates': 'try match move (step(fiber (int reply) inferred { scope { onExit log("fiber-cleanup"); throw 4 } }, 0)) { returned(r) => r, yielded(v, _) => v } catch (e) e', 'fiber-trap-propagates-as-protected': 'match move (step(fiber (int reply) inferred { scope { onExit log("fiber-cleanup"); reply / 0 } }, 1)) { returned(r) => r, yielded(v, _) => v }', 'fiber-host-await-is-invisible-to-stepper': 'match move (step(fiber (int reply) inferred { readFile("f") }, 0)) { returned(r) => r, yielded(v, _) => v }', 'spawn-publishes-handle-before-child-runs': 'auto t = spawn([move]() inferred => { log("child"); 7 }); log("parent"); join(t)', 'spawn-capacity-failure-leaves-callable-with-parent': 'scope { onFailure log("f"); Token k = Token { id: 1 }; spawn([move]() plain => match (k) { _ => 7 }) }', 'join-propagates-child-exception': 'try join(spawn([move]() inferred => scope { onExit log("child-cleanup"); throw 4 })) catch (e) e', 'join-propagates-child-trap': 'join(spawn([move]() plain => 1 / 0))', 'child-grant-is-attenuated-by-host-policy': 'join(spawn([move]() inferred => readFile("f")))', 'child-effect-resumes-through-parent-join': 'join(spawn([move]() inferred => readFile("f"))) + 1', 'cancel-of-unstarted-task-drops-its-captures': 'Token k = Token { id: 1 }; auto t = spawn([move]() plain => match (k) { _ => 7 }); cancel(t); 1', 'dropped-task-handle-goes-to-reaper': 'auto t = spawn([move]() plain => 7); 1', 'grant-revoked-between-dispatches-denies-the-next': 'scope { onFailure log("f"); readFile("a") + readFile("b") }', 'child-grant-revoked-mid-run-denies-its-next-dispatch': 'join(spawn([move]() inferred => readFile("a") + readFile("b")))', 'effectful-callable-type-carries-its-contract': 'int twice(int function(int) effectsInfer nothrow nonSuspending f, int x) inferred { f(f(x)) } twice((int x) plain => x + 1, 40) + 0', 'variant-construction-with-payload': 'variant Shape { Empty, Circle(int) } match (Circle(3)) { Circle(r) => r, Empty() => 0 }', 'variant-construction-without-payload': 'variant Shape { Empty, Circle(int) } match (Empty()) { Circle(r) => r, Empty() => 0 }', 'member-read-copies-a-copy-field': 'Point p = Point { x: 3, y: 4 }; p.x + p.y', 'member-read-of-temporary-drops-the-aggregate': 'Pair { a: 2, b: Token { id: 1 } }.a', 'member-read-borrows-a-non-copy-field': 'int peek(Token t) plain { 1 } Pair p = Pair { a: Token { id: 1 }, b: 2 }; peek(p.a) + p.b'}
CLIKE_REJECT={'break-outside-loop': 'break', 'continue-outside-loop': 'continue', 'break-cannot-cross-function-boundary': 'unit f() plain { break } while (true) f()', 'break-cannot-leave-guard-body': 'while (true) scope { onExit break; 1 }', 'return-outside-callable': 'return 1', 'return-cannot-leave-guard-body': 'int f() plain { scope { onExit return 2; 1 } } f() + 0', 'rethrow-outside-catch': 'rethrow', 'rethrow-cannot-cross-function-boundary': 'int f() inferred { rethrow } try throw 1 catch (_) f() + 0', 'yield-outside-fiber': 'yield 1', 'assignment-to-immutable-local': 'int x = 1; x = 2; x', 'use-after-move': 'Token a = Token { id: 1 }; Token b = a; match (a) { _ => 7 }', 'step-of-consumed-fiber-handle': 'auto f = fiber (int reply) inferred { 1 }; step(f, 0); match move (step(f, 0)) { returned(r) => r, yielded(v, _) => v }', 'tail-call-cannot-pass-loan-of-discarded-frame': 'unit g(mut int n) plain { n = 1 } unit f() plain { mut int x = 0; g(x) } f()', 'stealing-parameter-cannot-take-a-borrow': 'int eat(move Token t) plain { 2 } int pass(Token t) plain { eat(t) + 0 } pass(Token { id: 1 }) + 0', 'field-cannot-be-moved-out-of-aggregate': 'Pair { a: Token { id: 1 }, b: 2 }.a', 'constructor-arity-mismatch': 'variant Shape { Empty, Circle(int) } match (Circle()) { Circle(r) => r, Empty() => 0 }', 'unknown-member': 'Point { x: 3, y: 4 }.z', 'spawned-callable-cannot-capture-a-loan': 'int n = 1; join(spawn(() plain => n))', 'join-of-consumed-task-handle': 'auto t = spawn([move]() plain => 7); cancel(t); join(t)', 'non-exhaustive-match': 'match (2) { 1 => 10 }', 'unbound-name': 'missing + 1', 'exact-capture-list-omits-used-binding': 'int n = 5; [](int x) plain => x + n', 'copy-capture-requires-copy-evidence': 'Token t = Token { id: 1 }; [copy t]() plain => 1'}
for _gone in ['fiber-construction-does-not-run-body', 'fiber-yield-returns-one-successor', 'fiber-return-has-no-successor', 'fiber-resume-value-is-yield-result', 'fiber-raise-consumes-handle-and-propagates', 'fiber-trap-propagates-as-protected', 'fiber-host-await-is-invisible-to-stepper']:
    CLIKE.pop(_gone, None)
for _gone in ['yield-outside-fiber','step-of-consumed-fiber-handle']:
    CLIKE_REJECT.pop(_gone, None)
def with_clike(items, table):
    out=[]
    for item in items:
        assert item["id"] in table, item["id"]
        ordered={}
        for key,value in item.items():
            ordered[key]=value
            if key=="colisp": ordered["clike"]=table[item["id"]]
        out.append(ordered)
    assert set(table)=={i["id"] for i in items}, set(table)-{i["id"] for i in items}
    return out
# --- drops at joins
def tri(id,cl,ck,cf,context=None,note=None):
    v(id,cl,cf,context,note=note); CLIKE[id]=ck
JD='(define (eat (steal t : Token)) : int ! plain 2) (define (boom) : int ! inferred (throw 9)) '
JK='int eat(move Token t) plain { 2 } int boom() inferred { throw 9 } '
JF=': eat ( steal t: Token -- int ! plain ) 2 ; : boom ( -- int ! inferred ) 9 throw ; '
JC=ctx(lifecycle=TOK)
tri("value-moved-on-one-if-arm-drops-at-the-join-on-the-other",JD+'(let [a : Token (Token :id 1)] (begin (if true 0 (eat a)) (log "after") 7))',
    JK+'Token a = Token { id: 1 }; if (true) 0 else eat(a); log("after"); 7',
    JF+'Token{ id: 1 } { a: Token -- } true if 0 else a eat then drop "after" log 7',JC,
    note="The arm that does not move a drops it as that arm ends, so the drop precedes the later event rather than waiting for the scope to end.")
tri("value-moved-on-one-match-arm-drops-at-the-join-on-the-other",JD+'(let [a : Token (Token :id 1)] (begin (match 1 (1 0) (_ (eat a))) (log "after") 7))',
    JK+'Token a = Token { id: 1 }; match (1) { 1 => 0, _ => eat(a) }; log("after"); 7',
    JF+'Token{ id: 1 } { a: Token -- } 1 match 1 of 0 endof _ of a eat endof endmatch drop "after" log 7',JC)
tri("raise-before-the-move-in-a-try-body-drops-the-value-before-the-handler",JD+'(let [a : Token (Token :id 1)] (begin (try (+ (boom) (eat a)) (catch _ (begin (log "caught") 0))) (log "after") 7))',
    JK+'Token a = Token { id: 1 }; try boom() + eat(a) catch (_) { log("caught"); 0 }; log("after"); 7',
    JF+'Token{ id: 1 } { a: Token -- } try boom a eat + catch _ of "caught" log 0 endof endtry drop "after" log 7',JC,
    note="A binding the try body moves is dropped while control leaves the body if the body had not moved it yet; no handler arm sees it.")
tri("raise-after-the-move-in-a-try-body-does-not-drop-again",JD+'(let [a : Token (Token :id 1)] (begin (try (+ (eat a) (boom)) (catch _ (begin (log "caught") 0))) (log "after") 7))',
    JK+'Token a = Token { id: 1 }; try eat(a) + boom() catch (_) { log("caught"); 0 }; log("after"); 7',
    JF+'Token{ id: 1 } { a: Token -- } try a eat boom + catch _ of "caught" log 0 endof endtry drop "after" log 7',JC)
tri("value-moved-only-in-a-catch-arm-drops-at-the-join-when-the-body-completes",JD+'(let [a : Token (Token :id 1)] (begin (try 5 (catch _ (eat a))) (log "after") 7))',
    JK+'Token a = Token { id: 1 }; try 5 catch (_) eat(a); log("after"); 7',
    JF+'Token{ id: 1 } { a: Token -- } try 5 catch _ of a eat endof endtry drop "after" log 7',JC)
tri("trap-in-a-try-body-drops-the-unmoved-value-before-outer-guards",JD+'(let [a : Token (Token :id 1)] (scope (on-exit (log "guard")) (try (+ (/ 1 0) (eat a)) (catch _ 0))))',
    JK+'Token a = Token { id: 1 }; scope { onExit log("guard"); try 1 / 0 + eat(a) catch (_) 0 }',
    JF+'Token{ id: 1 } { a: Token -- } scope on-exit [ ( -- ) | "guard" log ] try 1 0 / a eat + catch _ of 0 endof endtry endscope',JC,
    note="A trap is not catchable, yet it still leaves the try body, so the same drop runs there.")
# --- tail position and adoption; fiber resume ownership
TG='(define (g (t : Token)) : int ! plain 1) '; TGK='int g(Token t) plain { 1 } '; TGF=': g ( t: Token -- int ! plain ) 1 ; '
TC0=ctx(lifecycle=TOK,ops=())
tri("tail-call-forwards-an-adopted-borrow",TG+'(define (peek (t : Token)) : int ! plain (g t)) (peek (Token :id 1))',
    TGK+'int peek(Token t) plain { g(t) } peek(Token { id: 1 })',
    TGF+': peek ( t: Token -- int ! plain ) t g ; Token{ id: 1 } peek',TC0,
    note="peek's frame adopted the temporary. Its own tail call passes the loan on, so the adoption follows the loan into g's frame and the value is dropped when g returns. peek's body means the same as it does when its caller keeps the value.")
tri("tail-call-drops-an-adopted-value-it-does-not-forward",'(define (g (n : int)) : int ! plain (begin (log "in g") n)) (define (peek (t : Token)) : int ! plain (g 4)) (peek (Token :id 1))',
    'int g(int n) plain { log("in g"); n } int peek(Token t) plain { g(4) } peek(Token { id: 1 })',
    ': g ( n: int -- int ! plain ) "in g" log n ; : peek ( t: Token -- int ! plain ) 4 g ; Token{ id: 1 } peek',JC)
tri("adopted-values-stay-bounded-across-a-tail-loop",'(define (loop (t : Token) (u : Token) (n : int)) : int ! plain (if (== n 0) 0 (loop t (Token :id n) (- n 1)))) (loop (Token :id 9) (Token :id 8) 2)',
    'int loop(Token t, Token u, int n) plain { if (n == 0) 0 else loop(t, Token { id: n }, n - 1) } loop(Token { id: 9 }, Token { id: 8 }, 2)',
    ': loop ( t: Token u: Token n: int -- int ! plain ) n 0 == if 0 else t Token{ id: n } n 1 - loop then ; Token{ id: 9 } Token{ id: 8 } 2 loop',TC0,
    note="Each iteration forwards t and adopts a fresh u. The u it received is not forwarded, so it is dropped before the next iteration is entered and a frame never holds more adopted values than it has parameters.")
tri("return-inside-a-try-body-is-not-a-tail-call",'(define (g) : int ! inferred (throw 5)) (define (f) : int ! inferred (try (return (g)) (catch _ 9))) (+ (f) 0)',
    'int g() inferred { throw 5 } int f() inferred { try return g() catch (_) 9 } f() + 0',
    ': g ( -- int ! inferred ) 5 throw ; : f ( -- int ! inferred ) try g return catch _ of 9 endof endtry ; f 0 +',
    note="The handler still has to see what g raises, so the call keeps f's frame.")
tri("catch-arm-call-is-not-a-tail-call",'(define (g (n : int)) : int ! plain (begin (log "in g") n)) (define (f) : int ! inferred (try (throw (Token :id 1)) (catch _ (g 4)))) (+ (f) 0)',
    'int g(int n) plain { log("in g"); n } int f() inferred { try throw Token { id: 1 } catch (_) g(4) } f() + 0',
    ': g ( n: int -- int ! plain ) "in g" log n ; : f ( -- int ! inferred ) try Token{ id: 1 } throw catch _ of 4 g endof endtry ; f 0 +',JC,
    note="A handler arm is not tail position: it still owes the release of the exception it caught, so g runs and returns before that value is dropped.")
tri("declaration-after-a-local-is-still-a-top-level-item",'(define (f) : int ! plain 1) (let [k : int 5] (+ (f) k))','int k = 5; int f() plain { 1 } f() + k','5 { k: int -- } : f ( -- int ! plain ) 1 ; f k +',
    note="A declaration written after a local declaration is an item of the submission, not of that local's scope, in every syntax.")
CT='callable<() -> int ! effects<> | nothrow | non-suspending>'
PK='(define (peek (t : Token)) : int ! plain (begin (log "in peek") 1)) '; PKK='int peek(Token t) plain { log("in peek"); 1 } '; PKF=': peek ( t: Token -- int ! plain ) "in peek" log 1 ; '
tri("tail-call-through-a-lent-closure-carries-what-it-borrows",PK+'(define (apply (f : '+CT+')) : int ! plain (begin (log "in apply") (f))) (define (h (a : Token)) : int ! plain (apply (lambda () ! plain (peek a)))) (h (Token :id 1))',
    PKK+'int apply(int function() plain f) plain { log("in apply"); f() } int h(Token a) plain { apply(() plain => peek(a)) } h(Token { id: 1 })',
    PKF+': apply ( f: '+CT+' -- int ! plain ) "in apply" log f ; : h ( a: Token -- int ! plain ) [ ( -- int ! plain ) | a peek ] apply ; Token{ id: 1 } h',JC,
    note="Three tail calls in a row. h's frame adopted the token; the closure borrows it and is itself handed to apply, which tail-calls it, and the closure body tail-calls peek with the loan. The adoption follows the loan each time, so the token is dropped only after peek has run.")
tri("tail-call-through-an-owned-closure-moves-it-into-the-new-frame",PK+'(define (take (steal f : '+CT+')) : int ! plain (begin (log "in take") (f))) (let [t : Token (Token :id 7)] (take (lambda :move () ! plain (peek t))))',
    PKK+'int take(move int function() plain f) plain { log("in take"); f() } Token t = Token { id: 7 }; take([move] () plain => peek(t))',
    PKF+': take ( steal f: '+CT+' -- int ! plain ) "in take" log f ; Token{ id: 7 } { t: Token -- } [ captures: move ( -- int ! plain ) | t peek ] take',JC,
    note="take owns the closure and calls it in tail position. The closure moves into the frame that runs it, which drops it, and the token it owns, only when the chain of tail calls it starts has returned.")
# --- generators: a function whose own body yields; calling it makes a dormant fiber that is a range
CNT='(define (count (n : int)) : int ! inferred (begin (log "a") (yield n) (log "b") (yield (+ n 1)) (log "c") (+ n 2))) '
CNTK='int count(int n) inferred { log("a"); yield n; log("b"); yield n + 1; log("c"); n + 2 } '
CNTF=': count ( n: int -- int ! inferred ) "a" log n yield drop "b" log n 1 + yield drop "c" log n 2 + ; '
tri("calling-a-generator-runs-none-of-it",CNT+'(let [g (count 1)] 7)',CNTK+'auto g = count(1); 7',CNTF+'1 count { g -- } 7',ctx(),
    note="A function whose own body contains yield is a generator. Calling it evaluates the arguments and makes a dormant fiber; no part of the body runs. Dropping it unstarted drops its arguments.")
tri("generator-is-primed-by-its-first-read-and-advanced-only-by-pop-front",
    CNT+'(let [g (count 1)] (begin (log "made") (let [a (front g)] (begin (pop-front g) (let [b (front g)] (begin (pop-front g) (let [c (front g)] (begin (pop-front g) (if (empty? g) (+ a (+ b c)) 0)))))))))',
    CNTK+'auto g = count(1); log("made"); auto a = front(g); popFront(g); auto b = front(g); popFront(g); auto c = front(g); popFront(g); if (`empty?`(g)) a + (b + c) else 0',
    CNTF+'1 count { g -- } "made" log g front { a -- } g pop-front g front { b -- } g pop-front g front { c -- } g pop-front g empty? if a b c + + else 0 then',ctx(),
    note="The first front runs the body to its first yield. front never advances; each pop-front advances once. The returned value has the item type, so it is the last item, and the generator is empty only after that item is popped.")
ACC='(define (acc (n : int)) : unit ! inferred (let [mut k : int n] (while true (match :steal (yield k) ((some v) (set! k v)) ((none) (set! k (+ k 1))))))) '
ACCK='unit acc(int n) inferred { mut int k = n; while (true) match move (yield k) { some(v) => k = v, none() => k = k + 1 } } '
ACCF=': acc ( n: int -- ! inferred ) n { mut k: int -- } begin true while k yield steal match some( v ) of v to k endof none( ) of k 1 + to k endof endmatch repeat ; '
tri("reply-sends-fresh-arguments-and-returns-the-next-item",ACC+'(let [g (acc 1)] (let [a (front g)] (let [b (reply g 50)] (begin (pop-front g) (+ a (+ b (front g)))))))',
    ACCK+'auto g = acc(1); auto a = front(g); auto b = reply(g, 50); popFront(g); a + (b + front(g))',
    ACCF+'1 acc { g -- } g front { a -- } g 50 reply { b -- } g pop-front a b g front + +',
    note="yield evaluates to an option of the generator's own parameters: some when the consumer replied, none when it only popped. reply has the generator's signature: it hands over the arguments, advances once, and returns the new front.")
tri("dropping-a-suspended-generator-runs-its-cleanup-at-the-drop",
    '(define (gen) : unit ! inferred (scope (on-cancel (log "cancelled")) (on-exit (log "exit")) (yield 1) (yield 2))) (let [g (gen)] (begin (front g) (log "leaving") 0))',
    'unit gen() inferred { scope { onCancel log("cancelled"); onExit log("exit"); yield 1; yield 2 } } auto g = gen(); front(g); log("leaving"); 0',
    ': gen ( -- ! inferred ) scope on-cancel [ ( -- ) | "cancelled" log ] on-exit [ ( -- ) | "exit" log ] 1 yield drop 2 yield drop endscope ; gen { g -- } g front drop "leaving" log 0',ctx(),
    note="Nobody can advance the generator once its handle is gone, so the drop cancels it: its guards run with exit class cancel, in the dropping execution, before that execution continues.")
ONE='(define (one) : unit ! inferred (yield 1)) '; ONEK='unit one() inferred { yield 1 } '; ONEF=': one ( -- ! inferred ) 1 yield drop ; '
tri("reading-a-finished-generator-raises-range-empty",ONE+'(let [g (one)] (begin (pop-front g) (try (front g) (catch _ 7))))',ONEK+'auto g = one(); popFront(g); try front(g) catch (_) 7',ONEF+'one { g -- } g pop-front try g front catch _ of 7 endof endtry',
    note="Reading past the end is the exception RangeEmpty, raised in the reader and caught by its handler like any other. It is not a trap.")
tri("uncaught-range-empty-fails-like-any-exception",ONE+'(let [g (one)] (begin (pop-front g) (pop-front g) 0))',ONEK+'auto g = one(); popFront(g); popFront(g); 0',ONEF+'one { g -- } g pop-front g pop-front 0')
tri("start-runs-to-the-first-yield-at-once",CNT+'(let [g (start (count 1))] (begin (log "after start") (front g)))',CNTK+'auto g = start(count(1)); log("after start"); front(g)',CNTF+'1 count start { g -- } "after start" log g front',ctx())
BAD='(define (bad (n : int)) : int ! inferred (begin (if (< n 0) (throw 9) ()) (yield n) n)) '
BADK='int bad(int n) inferred { if (n < 0) throw 9; yield n; n } '
BADF=': bad ( n: int -- int ! inferred ) n 0 < if 9 throw then n yield drop n ; '
tri("start-keeps-a-failure-for-the-first-read",BAD+'(let [g (start (bad -1))] (begin (log "after start") (try (front g) (catch e e))))',
    BADK+'auto g = start(bad(-1)); log("after start"); try front(g) catch (e) e',
    BADF+'-1 bad start { g -- } "after start" log try g front catch e of e endof endtry',ctx(),
    note="The body fails before its first yield. start does not raise; the failure is stored and raised by the first read, inside that reader's try.")
tri("generator-failure-is-raised-in-the-reader",BAD+'(try (front (bad -1)) (catch e e))',BADK+'try front(bad(-1)) catch (e) e',BADF+'try -1 bad front catch e of e endof endtry')
tri("generator-owns-a-yielded-item-until-it-is-popped",
    '(define (toks) : unit ! inferred (begin (yield (Token :id 1)) (yield (Token :id 2)))) (let [g (toks)] (begin (front g) (pop-front g) (log "second") 0))',
    'unit toks() inferred { yield Token { id: 1 }; yield Token { id: 2 } } auto g = toks(); front(g); popFront(g); log("second"); 0',
    ': toks ( -- ! inferred ) Token{ id: 1 } yield drop Token{ id: 2 } yield drop ; toks { g -- } g front drop g pop-front "second" log 0',JC,
    note="front lends the item; pop-front drops it. The item still buffered when the handle is dropped is dropped with it.")
tri("host-request-inside-a-generator-is-invisible-to-its-reader",
    '(define (rd) : unit ! inferred (yield (read-file "f"))) (let [g (rd)] (front g))',
    'unit rd() inferred { yield readFile("f") } auto g = rd(); front(g)',
    ': rd ( -- ! inferred ) "f" read-file yield drop ; rd { g -- } g front',
    ctx(host={"grants":{"read-file":True},"resumes":[msg("read-file#0",0,{"value":8})]},ops=E))
tri("return-inside-a-generator-is-not-a-tail-call",
    '(define (three) : int ! inferred 3) (define (gen) : int ! inferred (begin (yield 1) (return (three)))) (let [g (gen)] (begin (pop-front g) (front g)))',
    'int three() inferred { 3 } int gen() inferred { yield 1; return three() } auto g = gen(); popFront(g); front(g)',
    ': three ( -- int ! inferred ) 3 ; : gen ( -- int ! inferred ) 1 yield drop three return ; gen { g -- } g pop-front g front',
    note="A generator's frame belongs to its handle, so the call keeps it; the returned value is the last item.")
# --- admission
PRE={"mode":"preflight"}
def adm(id,cl,ck,cf,host,note=None):
    v(id,cl,cf,ctx(host=host,ops=E),note=note); CLIKE[id]=ck
adm("lazy-admission-fails-at-the-first-dispatch",'(begin (log "started") (read-file "f"))','log("started"); readFile("f")','"started" log "f" read-file',{"grants":{}},
    note="Without preflight the program starts, its first event is already out, and the missing grant surfaces at the request.")
adm("preflight-refuses-before-anything-runs",'(begin (log "started") (read-file "f"))','log("started"); readFile("f")','"started" log "f" read-file',{"grants":{},"admission":PRE},
    note="The same program under preflight admission performs no transition at all.")
adm("preflight-prompt-allow-admits",'(+ (read-file "f") 1)','readFile("f") + 1','"f" read-file 1 +',{"grants":{},"admission":{"mode":"preflight","prompt":{"read-file":"allow"}},"resumes":[msg("read-file#0",0,{"value":5})]})
adm("preflight-prompt-deny-refuses",'(+ (read-file "f") 1)','readFile("f") + 1','"f" read-file 1 +',{"grants":{},"admission":{"mode":"preflight","prompt":{"read-file":"deny"}}})
adm("manifest-covers-untaken-branches",'(if false (read-file "f") 1)','if (false) readFile("f") else 1','false if "f" read-file else 1 then',{"grants":{},"admission":PRE},
    note="The request would never execute, but it is in the entry's effect row, so the program is not admitted.")
adm("manifest-covers-closures-and-callees",'(define (fetch) : int ! inferred (read-file "inner")) (let [k (lambda :move () ! inferred (fetch))] 1)','int fetch() inferred { readFile("inner") } auto k = [move]() inferred => fetch(); 1',': fetch ( -- int ! inferred ) "inner" read-file ; [ captures: move ( -- int ! inferred ) | fetch ] { k -- } 1',{"grants":{},"admission":PRE},
    note="The closure is never called. Its body, and the function it calls, are still reachable code.")
adm("unreferenced-function-is-outside-the-manifest",'(define (unused) : int ! inferred (read-file "x")) 1','int unused() inferred { readFile("x") } 1',': unused ( -- int ! inferred ) "x" read-file ; 1',{"grants":{},"admission":PRE})
ONLY_A={"read-file":{"arguments":[["a.txt"]]}}
adm("static-argument-outside-the-grant-is-refused-upfront",'(read-file "b.txt")','readFile("b.txt")','"b.txt" read-file',{"grants":ONLY_A,"admission":PRE})
adm("static-argument-inside-the-grant-is-admitted",'(read-file "a.txt")','readFile("a.txt")','"a.txt" read-file',{"grants":ONLY_A,"admission":PRE,"resumes":[msg("read-file#0",0,{"value":3})]})
adm("non-static-argument-needs-an-unrestricted-grant",'(let [p : string "b.txt"] (read-file p))','string p = "b.txt"; readFile(p)','"b.txt" { p: string -- } p read-file',{"grants":ONLY_A,"admission":PRE},
    note="Nothing bounds the argument statically, so a grant limited to particular arguments does not cover the request and it is refused upfront.")
adm("lazy-admission-checks-the-actual-argument-at-dispatch",'(let [p : string "b.txt"] (read-file p))','string p = "b.txt"; readFile(p)','"b.txt" { p: string -- } p read-file',{"grants":ONLY_A},
    note="Without preflight the same program starts, and the dispatch check sees the real argument.")
adm("each-uncovered-request-is-prompted",'(let [p : string "b.txt"] (+ (read-file "a.txt") (read-file p)))','string p = "b.txt"; readFile("a.txt") + readFile(p)','"b.txt" { p: string -- } "a.txt" read-file p read-file +',{"grants":{},"admission":{"mode":"preflight","prompt":{"read-file":"allow"}},"resumes":[msg("read-file#0",0,{"value":1}),msg("read-file#1",1,{"value":2})]},
    note="Two requests are prompted. Allowing the static one grants that argument only; allowing the non-static one grants the operation.")
adm("admitted-program-is-still-rechecked-at-dispatch",'(+ (read-file "a") (read-file "b"))','readFile("a") + readFile("b")','"a" read-file "b" read-file +',{"grants":{"read-file":True},"admission":PRE,"revocations":[{"after_sequence":0,"operation":"read-file"}],"resumes":[msg("read-file#0",0,{"value":1})]})
V=with_clike(V,CLIKE)
json.dump({"schema_version":2,"language_version":"0.1-draft",
 "description":"Execution vectors for the dynamic semantics. Each vector gives one program in every frontend spelling (CoLisp, C-like, and Co-Forth), the normalized AST every reader must construct, the semantic digest every event stream must share, and the complete observable outcome of interpreting semantics/transitions.json: every transition in order, the terminal, and the final machine state. `context.operations` declares host operations by transition kind, `context.lifecycle` names record types whose drop is observable, and `context.host` scripts grants, cancellation, and resume messages. Regenerate derived fields only with scripts/language/check_language_spec.py --write and review the diff.",
 "vectors":V},open('../../docs/language/fixtures/execution-vectors.json','w'),ensure_ascii=False,indent=2)
R=[]
def r(id,code,cl,cf,context=None,why=None):
    d={"id":id,"code":code,"colisp":cl,"coforth":cf}
    if cf is None: d["unpaired_reason"]=why
    if context: d["context"]=context
    R.append(d)
r("break-outside-loop","F-DIAG-LOOP-TARGET","(break)","break")
r("continue-outside-loop","F-DIAG-LOOP-TARGET","(continue)","continue")
r("break-cannot-cross-function-boundary","F-DIAG-LOOP-TARGET","(define (f) : unit ! plain (break)) (while true (f))",": f ( -- ! plain ) break ; begin true while f repeat")
r("break-cannot-leave-guard-body","F-DIAG-LOOP-TARGET","(while true (scope (on-exit (break)) 1))","begin true while scope on-exit [ ( -- ) | break ] 1 drop endscope repeat")
r("return-outside-callable","F-DIAG-RETURN-TARGET","(return 1)",None,why="A Co-Forth submission body has no result signature for `return` to consume.")
r("return-cannot-leave-guard-body","F-DIAG-GUARD-ESCAPE","(define (f) : int ! plain (scope (on-exit (return 2)) 1)) (+ (f) 0)",None,why="A Co-Forth guard is a quotation whose own signature bounds `return`.")
r("rethrow-outside-catch","F-DIAG-RETHROW-TARGET","(rethrow)","rethrow")
r("rethrow-cannot-cross-function-boundary","F-DIAG-RETHROW-TARGET","(define (f) : int ! inferred (rethrow)) (try (throw 1) (catch _ (+ (f) 0)))",": f ( -- int ! inferred ) rethrow ; try 1 throw catch _ of f 0 + endof endtry")
r("assignment-to-immutable-local","F-DIAG-ASSIGN-IMMUTABLE","(let [x : int 1] (begin (set! x 2) x))","1 { x: int -- } 2 to x x")
r("use-after-move","F-DIAG-USE-AFTER-MOVE","(let [a : Token (Token :id 1)] (let [b : Token a] (match a (_ 7))))","Token{ id: 1 } { a: Token -- } a { b: Token -- } a match _ of 7 endof endmatch",{"lifecycle":["Token"]})
r("tail-call-cannot-pass-loan-of-discarded-frame","F-DIAG-LOAN-ESCAPES-FRAME","(define (g (borrow-mut n : int)) : unit ! plain (set! n 1)) (define (f) : unit ! plain (let [mut x : int 0] (g x))) (f)",": g ( borrow-mut n: int -- ! plain ) 1 to n ; : f ( -- ! plain ) { -- mut x: int } 0 to x x g ; f")
r("stealing-parameter-cannot-take-a-borrow","F-DIAG-MOVE-FROM-BORROW","(define (eat (steal t : Token)) : int ! plain 2) (define (pass (t : Token)) : int ! plain (+ (eat t) 0)) (+ (pass (Token :id 1)) 0)",": eat ( steal t: Token -- int ! plain ) 2 ; : pass ( t: Token -- int ! plain ) t eat 0 + ; Token{ id: 1 } pass 0 +",{"lifecycle":["Token"]})
r("spawned-callable-cannot-capture-a-loan","F-DIAG-TRANSFER-REQUIRES-OWNED","(let [n : int 1] (join (spawn (lambda () ! plain n))))","1 { n: int -- } [ ( -- int ! plain ) | n ] spawn join")
r("join-of-consumed-task-handle","F-DIAG-USE-AFTER-MOVE","(let [t (spawn (lambda :move () ! plain 7))] (begin (cancel t) (join t)))","[ captures: move ( -- int ! plain ) | 7 ] spawn { t -- } t cancel t join")
r("field-cannot-be-moved-out-of-aggregate","F-DIAG-PARTIAL-MOVE","(. (Pair :a (Token :id 1) :b 2) a)","Pair{ a: Token{ id: 1 } b: 2 } .a",{"lifecycle":["Token"]})
r("constructor-arity-mismatch","F-DIAG-CONSTRUCTOR-ARITY","(variant Shape (Empty) (Circle int)) (match (Circle) ((Circle r) r) ((Empty) 0))",None,why="Co-Forth applies a constructor to exactly its declared payload, so an arity mismatch is a stack error found during construction.")
r("unknown-member","F-DIAG-UNKNOWN-MEMBER","(. (Point :x 3 :y 4) z)","Point{ x: 3 y: 4 } .z")
r("non-exhaustive-match","F-DIAG-MATCH-NOT-EXHAUSTIVE","(match 2 (1 10))","2 match 1 of 10 endof endmatch")
r("unbound-name","F-DIAG-UNBOUND-NAME","(+ missing 1)","missing 1 +")
r("exact-capture-list-omits-used-binding","F-DIAG-CAPTURE-UNLISTED","(let [n : int 5] (lambda (:captures) ((x : int)) ! plain (+ x n)))","5 { n: int -- } [ captures: { } ( x: int -- int ! plain ) | x n + ]")
r("copy-capture-requires-copy-evidence","F-DIAG-COPY-EVIDENCE","(let [t : Token (Token :id 1)] (lambda (:captures (copy t)) () ! plain 1))","Token{ id: 1 } { t: Token -- } [ captures: { copy t } ( -- int ! plain ) | 1 ]",{"lifecycle":["Token"]})
r("declaration-inside-an-expression","F-DIAG-NESTED-DECLARATION",'(let [x : int 1] (begin (define (f) : int ! plain x) (f)))',None,why="The Co-Forth grammar admits a definition only among the top-level words, so it has no spelling for this program.")
CLIKE_REJECT["declaration-inside-an-expression"]='int x = 1; { int f() plain { x } f() }'
r("contract-states-one-axis-only","F-DIAG-CONTRACT-AXES",'(define (f) : int ! nothrow 1) (f)',': f ( -- int ! nothrow ) 1 ; f')
CLIKE_REJECT["contract-states-one-axis-only"]='int f() nothrow { 1 } f()'
r("contract-states-an-axis-twice","F-DIAG-CONTRACT-AXES",'(define (f) : int ! pure | nothrow | suspends | non-suspending 1) (f)',': f ( -- int ! pure | nothrow | suspends | non-suspending ) 1 ; f')
CLIKE_REJECT["contract-states-an-axis-twice"]='int f() pure nothrow suspends nonSuspending { 1 } f()'
r("tail-call-cannot-pass-a-closure-that-borrows-the-discarded-frame","F-DIAG-LOAN-ESCAPES-FRAME",'(define (peek (t : Token)) : int ! plain 1) (define (apply (f : callable<() -> int ! effects<> | nothrow | non-suspending>)) : int ! plain (f)) (define (h) : int ! plain (let [a : Token (Token :id 1)] (apply (lambda () ! plain (peek a))))) (+ (h) 0)',': peek ( t: Token -- int ! plain ) 1 ; : apply ( f: callable<() -> int ! effects<> | nothrow | non-suspending> -- int ! plain ) f ; : h ( -- int ! plain ) Token{ id: 1 } { a: Token -- } [ ( -- int ! plain ) | a peek ] apply ; h 0 +',ctx(lifecycle=TOK,ops=()))
CLIKE_REJECT["tail-call-cannot-pass-a-closure-that-borrows-the-discarded-frame"]='int peek(Token t) plain { 1 } int apply(int function() plain f) plain { f() } int h() plain { Token a = Token { id: 1 }; apply(() plain => peek(a)) } h() + 0'
r("yield-outside-a-function","F-DIAG-YIELD-TARGET","(yield 1)","1 yield")
CLIKE_REJECT["yield-outside-a-function"]='yield 1'
r("generator-cannot-hold-a-borrowed-argument","F-DIAG-TRANSFER-REQUIRES-OWNED",'(define (g (t : Token)) : unit ! inferred (yield 1)) (let [a : Token (Token :id 1)] (let [x (g a)] 0))',': g ( t: Token -- ! inferred ) 1 yield drop ; Token{ id: 1 } { a: Token -- } a g { x -- } 0',ctx(lifecycle=TOK,ops=()))
CLIKE_REJECT["generator-cannot-hold-a-borrowed-argument"]='unit g(Token t) inferred { yield 1 } Token a = Token { id: 1 }; auto x = g(a); 0'
R=with_clike(R,CLIKE_REJECT)
json.dump({"schema_version":1,"language_version":"0.1-draft",
 "description":"Programs a conforming compiler MUST reject before execution, each with the stable diagnostic code of its principal error. Both spellings must build the stored AST. The reference machine detects these only as violated execution invariants; a conforming implementation reports them statically and never starts the program.",
 "vectors":R},open('../../docs/language/fixtures/static-rejections.json','w'),ensure_ascii=False,indent=2)
print(len(V),len(R))

# ---------------------------------------------------------------- sessions
SESS=[]
def T(cl,ck,cf,context=None):
    d={"colisp":cl,"clike":ck,"coforth":cf}
    if context: d["context"]=context
    return d
def sess(id,note,turns,context=None):
    d={"id":id,"note":note}
    if context: d["context"]=context
    d["turns"]=turns; SESS.append(d)
INC=("(define (inc (x : int)) : int ! plain (+ x 1))","int inc(int x) plain { x + 1 }",": inc ( x: int -- int ! plain ) x 1 + ;")
sess("definitions-persist-across-turns","A function declared in one turn is callable in the next.",[
  T(INC[0]+" (inc 1)",INC[1]+" inc(1)",INC[2]+" 1 inc"),
  T("(+ (inc 41) 0)","inc(41) + 0","41 inc 0 +")])
sess("failed-turn-commits-nothing","The first turn declares a function and then fails. Its declaration is not visible afterwards.",[
  T(INC[0]+" (throw 1)",INC[1]+" throw 1",INC[2]+" 1 throw"),
  T("(+ (inc 41) 0)","inc(41) + 0","41 inc 0 +")])
sess("trapped-turn-commits-nothing","A protected outcome discards the turn's declarations like any other failure.",[
  T(INC[0]+" (/ 1 0)",INC[1]+" 1 / 0",INC[2]+" 1 0 /"),
  T("(+ (inc 41) 0)","inc(41) + 0","41 inc 0 +")])
sess("rejected-turn-commits-nothing","A turn the compiler rejects never runs, so nothing in it is committed, including its valid declarations.",[
  T(INC[0]+" (+ missing 1)",INC[1]+" missing + 1",INC[2]+" missing 1 +"),
  T("(+ (inc 41) 0)","inc(41) + 0","41 inc 0 +")])
sess("refused-turn-commits-nothing","A turn refused at admission performs no transition and commits nothing.",[
  T(INC[0]+' (read-file "f")',INC[1]+' readFile("f")',INC[2]+' "f" read-file',{"host":{"grants":{},"admission":{"mode":"preflight"}}}),
  T("(+ (inc 41) 0)","inc(41) + 0","41 inc 0 +")],context=ctx(ops=E))
sess("events-of-a-failed-turn-are-not-retracted","The turn emits, then fails. The event already reached the host; only the declarations are discarded.",[
  T(INC[0]+' (log "sent") (throw 1)',INC[1]+' log("sent"); throw 1',INC[2]+' "sent" log 1 throw'),
  T("(+ (inc 41) 0)","inc(41) + 0","41 inc 0 +")],context=ctx())
sess("later-definition-shadows-without-changing-earlier-code","base is declared again in turn two. twice was checked against the first base and keeps calling it; new code sees the second.",[
  T("(define (base) : int ! plain 1) (define (twice) : int ! plain (+ (base) (base))) (twice)","int base() plain { 1 } int twice() plain { base() + base() } twice()",": base ( -- int ! plain ) 1 ; : twice ( -- int ! plain ) base base + ; twice"),
  T("(define (base) : int ! plain 10) (+ (twice) (base))","int base() plain { 10 } twice() + base()",": base ( -- int ! plain ) 10 ; twice base +"),
  T("(+ (base) 0)","base() + 0","base 0 +")])
sess("failed-redefinition-leaves-the-earlier-one-visible","A turn that redeclares a function and fails does not disturb the committed revision.",[
  T("(define (base) : int ! plain 1) (base)","int base() plain { 1 } base()",": base ( -- int ! plain ) 1 ; base"),
  T("(define (base) : int ! plain 10) (throw (base))","int base() plain { 10 } throw base()",": base ( -- int ! plain ) 10 ; base throw"),
  T("(+ (base) 0)","base() + 0","base 0 +")])
sess("declarations-in-one-turn-see-each-other-and-earlier-turns","Two new functions call each other and an older one.",[
  T(INC[0],INC[1],INC[2]),
  T("(define (even? (n : int)) : bool ! plain (if (== n 0) true (odd? (- n 1)))) (define (odd? (n : int)) : bool ! plain (if (== n 0) false (even? (- n 1)))) (if (even? (inc 3)) 1 0)",
    "bool `even?`(int n) plain { if (n == 0) true else `odd?`(n - 1) } bool `odd?`(int n) plain { if (n == 0) false else `even?`(n - 1) } if (`even?`(inc(3))) 1 else 0",
    ": even? ( n: int -- bool ! plain ) n 0 == if true else n 1 - odd? then ; : odd? ( n: int -- bool ! plain ) n 0 == if false else n 1 - even? then ; 3 inc even? if 1 else 0 then")])
sess("variant-persists-and-cannot-be-redeclared","A variant declared in a committed turn is usable later. Declaring it again is rejected: a nominal type has one identity in a session.",[
  T("(variant Shape (Empty) (Circle int)) (match (Circle 3) ((Circle r) r) ((Empty) 0))","variant Shape { Empty, Circle(int) } match (Circle(3)) { Circle(r) => r, Empty() => 0 }","variant: Shape cases{ Empty Circle( int ) } ; 3 Circle match Circle( r ) of r endof Empty( ) of 0 endof endmatch"),
  T("(match (Circle 4) ((Circle r) r) ((Empty) 0))","match (Circle(4)) { Circle(r) => r, Empty() => 0 }","4 Circle match Circle( r ) of r endof Empty( ) of 0 endof endmatch"),
  T("(variant Shape (Empty)) 1","variant Shape { Empty } 1","variant: Shape cases{ Empty } ; 1")])
sess("a-local-binding-does-not-leak-into-the-session","A let at the top of a submission scopes the rest of that submission only.",[
  T("(let [k : int 5] (+ k 1))","int k = 5; k + 1","5 { k: int -- } k 1 +"),
  T("(+ k 1)","k + 1","k 1 +")])
json.dump({"schema_version":1,"language_version":"0.1-draft",
 "description":"Sessions: several submissions against one persistent set of declarations, as a host keeps between turns of a REPL. Each turn gives its submission in every frontend spelling and the expected terminal, observable sequence, whether it committed, and the names visible afterwards with the revision each resolves to. A turn the compiler rejects has terminal kind `rejected` with its diagnostic code.",
 "sessions":SESS},open('../../docs/language/fixtures/session-vectors.json','w'),ensure_ascii=False,indent=2)
print("sessions",len(SESS))
