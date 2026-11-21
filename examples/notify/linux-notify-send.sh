#!/usr/bin/env sh
# Generic Linux desktop notification for a kelir [notify] hook (M11).
# Copy into your config as, e.g.:
#   [notify]
#   turn_done = "/path/to/linux-notify-send.sh"
# The hook's environment is scrubbed; the event and message arrive in
# KELIR_NOTIFY_EVENT and KELIR_NOTIFY_MESSAGE. No keys or tokens are present.
set -eu
title=${KELIR_NOTIFY_EVENT:-kelir}
message=${KELIR_NOTIFY_MESSAGE:-work ended}
exec notify-send --app-name=kelir "$title" "$message"
