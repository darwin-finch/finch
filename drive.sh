#!/bin/bash
SESSION="jordan_test2"
tmux send-keys -t $SESSION "$1" C-m
sleep 1
tmux capture-pane -t $SESSION -p
