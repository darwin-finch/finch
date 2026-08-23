#!/usr/local/bin/finch --exec
; Run with: cargo run -- --exec examples/finch/answer.lisp --json
(begin
  (say "The answer is ")
  (say (int-to-string (+ 20 22))))
