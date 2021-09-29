#!/bin/sh
# The purpose of this script is to set a log regex to forward error logs to stderr and other logs to
# stdout. This allows us to easily configure alerts when any message goes to stderr.

# The main command is run in the background so that it receives interrupt signals
# The stream-split command deliberately ignores interrupts (otherwise we may lose logs on teardown). It terminates automatically
# once the main command (stdin) terminates.
("$@" &) | (trap '' INT; regex-stream-split '^\d+-\d+-\d+T\d+:\d+:\d+\.\d+Z\s+(TRACE|DEBUG|INFO|WARN)' '^\d+-\d+-\d+T\d+:\d+:\d+\.\d+Z\s+ERROR')

