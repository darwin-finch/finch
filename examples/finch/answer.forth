#!/usr/local/bin/finch --exec
\ Run with: cargo run -- --exec examples/finch/answer.forth --json
s" The answer is " say
20 22 + int-to-string say
