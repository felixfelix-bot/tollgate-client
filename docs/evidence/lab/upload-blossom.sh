#!/bin/bash
# Upload a file to the self-hosted Blossom server (BUD-01, kind 24242 auth).
# Usage: upload-blossom.sh <file> [base_url]
set -eu
FILE=$1
BASE=${2:-https://blossom2.orangesync.tech}
KEY=$(grep -E '^BUZZ_PRIVATE_KEY=' "$HOME/.hermes/profiles/manager/.env" | cut -d= -f2- | tr -d '"'"'"'')
[ -n "$KEY" ] || { echo "no BUZZ_PRIVATE_KEY" >&2; exit 1; }
HASH=$(sha256sum "$FILE" | cut -d' ' -f1)
EXP=$(( $(date +%s) + 600 ))
URL="$BASE/upload"
AUTH=$(nak event -q -k 24242 -c "" \
  -t "t=upload" -t "u=$URL" -t "method=PUT" \
  -t "payload=$HASH" -t "expiration=$EXP" --sec "$KEY" 2>/dev/null)
B64=$(printf '%s' "$AUTH" | base64 -w0)
MIME=$(file --mime-type -b "$FILE")
curl -sS -w '\nHTTP:%{http_code}\n' -X PUT "$URL" \
  -H "Authorization: Nostr $B64" -H "Content-Type: $MIME" \
  --data-binary @"$FILE"
