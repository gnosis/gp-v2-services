#!/bin/sh
# The purpose of this script is to set a log regex to forward error logs to stderr and other logs to
# stdout. This allows us to easily configure alerts when any message goes to stderr.

# Run both commands in the background, so that we can receive signals and forward it to the main process (for this we capture the pid).
# The stream-split command deliberately ignores interrupts (otherwise we may lose logs on teardown). It terminates automatically
# once the main command (stdin) terminates.
( "$@" & echo $! > main_pid ) | (trap '' TERM INT; regex-stream-split '^\d+-\d+-\d+T\d+:\d+:\d+\.\d+Z\s+(TRACE|DEBUG|INFO|WARN)' '^\d+-\d+-\d+T\d+:\d+:\d+\.\d+Z\s+ERROR') &

pid=$(cat main_pid)
trap 'kill -15 $pid' TERM
trap 'kill -2 $pid' INT

wait
