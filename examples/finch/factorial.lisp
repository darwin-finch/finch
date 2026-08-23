#!/usr/local/bin/finch --exec
; A self-contained typed recursive Lisp program.
(define (factorial (n : int)) : int
  (if (<= n 1)
      1
      (* n (factorial (- n 1)))))

(begin
  (say "6! = ")
  (say (int-to-string (factorial 6)))
  (say "\n"))
