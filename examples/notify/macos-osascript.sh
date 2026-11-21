#!/bin/sh
# Generic macOS desktop notification for a kelir [notify] hook (M11).
# Copy into your config as, e.g.:
#   [notify]
#   turn_done = "/path/to/macos-osascript.sh"
# The hook's environment is scrubbed: only the event and message are supplied.
set -eu
event=${KELIR_NOTIFY_EVENT:-kelir}
message=${KELIR_NOTIFY_MESSAGE:-work ended}
# Fold control bytes and quotes so a message cannot break the AppleScript.
safe=$(printf '%s' "$message" | tr -d '\000-\037' | sed "s/[\"\\\\]/ /g")
exec osascript -e "display notification \"$safe\" with title \"kelir: $event\""
