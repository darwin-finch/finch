#!/usr/local/bin/finch --exec
\ A self-contained typed Co-Forth recursive definition.
: factorial ( S n:int -- S int ! pure )
  n 1 <= if
    1
  else
    n n 1 - factorial *
  then
;

s" 6! = " say
6 factorial int-to-string say
s"\n" say
