#!/usr/bin/env bash
# Watch recent Jira Service Management (on-call) alerts.
#
# Account / Environment / App are not top-level fields on the API response —
# they arrive as "Key: value" strings inside each alert's `tags` array, e.g.
# "Account: 123456789012", "Environment: Dev". tagval() below splits them out.
#
# Times: the API reports UTC. The cutoff comparison stays in UTC (string
# compare against createdAt), and rows are converted to the local timezone
# only when printed.
set -euo pipefail

: "${ATLASSIAN_EMAIL:?set ATLASSIAN_EMAIL}"
: "${JIRA_TOKEN:?set JIRA_TOKEN}"
: "${CLOUD_ID:?set CLOUD_ID}"

BASE="https://api.atlassian.com/jsm/ops/api/$CLOUD_ID/v1/alerts"
WINDOW_MIN="${WINDOW_MIN:-10}"
POLL_SEC="${POLL_SEC:-10}"
PAGE_SIZE="${PAGE_SIZE:-50}"
MAX_PAGES="${MAX_PAGES:-20}"

# tagval("account") -> the value of the "Account: …" tag, case-insensitive on
# the key, "-" when the alert has no such tag. Splitting on the FIRST colon
# only, so values that themselves contain a colon survive intact.
ROW='
def tagval($k):
  [ (.tags // [])[]
    | select(index(":"))
    | { k: (.[0:index(":")] | ascii_downcase | gsub("^\\s+|\\s+$";"")),
        v: (.[index(":")+1:]  | gsub("^\\s+|\\s+$";"")) }
    | select(.k == $k) | .v
    | select(length > 0)
  ] | first // "-";
(.values // .data // [])[]
| [ .createdAt,
    (.tinyId // "-"),
    (.priority // "-"),
    (.status // "?"),
    (if .acknowledged then "ack" else "unack" end),
    tagval("account"),
    tagval("environment"),
    (.message // "")
  ] | @tsv'

fetch() {  # $1 = offset
  curl -sS -G --user "$ATLASSIAN_EMAIL:$JIRA_TOKEN" -H "Accept: application/json" \
    --data-urlencode "sort=createdAt" --data-urlencode "order=desc" \
    --data-urlencode "size=$PAGE_SIZE" --data-urlencode "offset=$1" "$BASE"
}

# UTC ISO timestamp -> local time for display. Falls back to the raw value if
# date(1) can't parse it.
to_local() { date -d "$1" +"%b %d %I:%M:%S %p" 2>/dev/null || printf '%s' "$1"; }

while true; do
  cutoff=$(date -u -d "$WINDOW_MIN min ago" +%Y-%m-%dT%H:%M:%S.000Z)
  all=""
  offset=0; page=0
  while (( page < MAX_PAGES )); do
    resp=$(fetch "$offset") || break
    rows=$(jq -r "$ROW" <<<"$resp")
    [[ -z "$rows" ]] && break
    keep=$(awk -F'\t' -v c="$cutoff" '$1 >= c' <<<"$rows")
    [[ -n "$keep" ]] && all+="$keep"$'\n'
    oldest=$(tail -1 <<<"$rows" | cut -f1)
    [[ "$oldest" < "$cutoff" ]] && break        # whole rest of feed is older
    next=$(jq -r '.links.next // empty' <<<"$resp")
    [[ -z "$next" ]] && break
    offset=$(sed -n 's/.*[?&]offset=\([0-9]*\).*/\1/p' <<<"$next")
    [[ -z "$offset" ]] && break
    page=$(( page + 1 ))
  done

  clear
  printf 'Alerts in the last %s min   (as of %s, refresh %ss)\n' \
    "$WINDOW_MIN" "$(date +'%H:%M:%S %Z')" "$POLL_SEC"
  printf '%-19s %-7s %-3s %-12s %-14s %-10s %s\n' \
    "TIME ($(date +%Z))" ID PRI STATUS ACCOUNT ENV MESSAGE
  echo "---------------------------------------------------------------------------------------------"
  if [[ -z "${all//[$'\n ']}" ]]; then
    echo "(none in window)"
  else
    # sort on the UTC timestamp (still field 1), convert to local on print.
    printf '%s' "$all" | grep -v '^[[:space:]]*$' | sort \
    | while IFS=$'\t' read -r ts tiny pri status ack acct env msg; do
        printf '%-19s %-7s %-3s %-6s/%-5s %-14s %-10s %s\n' \
          "$(to_local "$ts")" "$tiny" "$pri" "$status" "$ack" "$acct" "$env" "$msg"
      done
  fi
  sleep "$POLL_SEC"
done
