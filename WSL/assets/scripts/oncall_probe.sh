#!/usr/bin/env bash
# Probe the Jira Service Management Operations on-call API, read-only.
#
# This answers the questions the GUI cannot answer from the published docs,
# which have to be settled before any of it can say "you are on call":
#
#   1. Does this token reach the schedules API at all, or does it 403 on
#      scope? Alerts and schedules are separate permissions -- a token that
#      reads the alert feed today may still be refused here.
#   2. Which schedules can it see, and are you actually a responder on one?
#      The API only lists a schedule if you administer it, are on its team,
#      or are in one of its rotations.
#   3. What identifier does the on-call feed use for you? If it reports
#      account ids rather than emails, the app has to look yours up first --
#      it only knows your OS username and the configured Atlassian email.
#   4. Does .../schedules/on-calls/<userIdentifier>.ics work, and with which
#      identifier? Atlassian documents the path without ever saying what
#      userIdentifier is, so this tries each candidate and reports which one
#      the server accepts.
#
# It also dumps one alert's field names, to confirm `owner` / `responders` /
# `seen` / `snoozed` are really there before src/alerts.rs starts parsing them.
#
# Credentials go to curl on stdin (-K -), never argv, so the token stays out
# of the process list -- the same rule src/alerts.rs follows. Nothing here
# writes: every request is a GET.
#
# Usage:
#   export ATLASSIAN_EMAIL='you@corp.com'
#   export JIRA_TOKEN='...'
#   export CLOUD_ID='...'
#   ./oncall_probe.sh
#
# CLOUD_ID is the same value alerts_10min.sh uses.

# Deliberately not `set -e`: a 403 on one probe must not stop the rest, since
# knowing which parts are refused is the point of running this.
set -uo pipefail

: "${ATLASSIAN_EMAIL:?set ATLASSIAN_EMAIL}"
: "${JIRA_TOKEN:?set JIRA_TOKEN}"
: "${CLOUD_ID:?set CLOUD_ID}"

for tool in curl jq; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 1; }
done

OPS="https://api.atlassian.com/jsm/ops/api/${CLOUD_ID}/v1"
JIRA="https://api.atlassian.com/ex/jira/${CLOUD_ID}"
# The Jira *issue* API may be on a different site from the alert feed's
# tenant -- an org can run Jira on its own domain. JIRA_BASE_URL matches the
# app's own override; unset, this probes the alerts tenant, exactly as the
# app does with no jira.base_url configured.
JIRA_SITE="${JIRA_BASE_URL:-$JIRA}"
JIRA_SITE="${JIRA_SITE%/}"
JIRA_SITE="${JIRA_SITE%/rest/api/3}"
JIRA_SITE="${JIRA_SITE%/rest/api/2}"
case "$JIRA_SITE" in https://*|http://*) ;; *) JIRA_SITE="https://${JIRA_SITE}" ;; esac

STATUS=""
BODY=""

# get <url> [accept-header] -- sets STATUS (HTTP code, "000" if curl never
# connected) and BODY (the response, status marker stripped).
get() {
  local url="$1" accept="${2:-application/json}" raw
  raw=$(curl -sS -K - -H "Accept: ${accept}" \
             -w $'\n__STATUS__%{http_code}' "$url" \
        <<<"user = \"${ATLASSIAN_EMAIL}:${JIRA_TOKEN}\"" 2>&1)
  STATUS="${raw##*__STATUS__}"
  BODY="${raw%$'\n'__STATUS__*}"
}

# Short, readable failure line for a non-200: the code plus the first bit of
# whatever the server said, which for Atlassian is usually a JSON message.
explain_failure() {
  local msg
  msg=$(jq -r '(.message // .errorMessage // .errors[0].message // empty)' <<<"$BODY" 2>/dev/null)
  [[ -z "$msg" ]] && msg=$(printf '%s' "$BODY" | tr -d '\r' | head -c 300)
  printf '  HTTP %s  %s\n' "$STATUS" "$msg"
}

rule() { printf '\n==== %s %s\n' "$1" "$(printf '=%.0s' $(seq 1 $((60 - ${#1}))))"; }

ACCOUNT_ID=""
SCHEDULE_IDS=()
SCHEDULE_NAMES=()
ON_CALL_NOW=""
ICS_IDENTIFIER=""

# ---------------------------------------------------------------- 1. identity
rule "1. who this token is"
get "${JIRA}/rest/api/3/myself"
if [[ "$STATUS" == "200" ]]; then
  ACCOUNT_ID=$(jq -r '.accountId // empty' <<<"$BODY")
  jq -r '"  accountId  : \(.accountId // "-")
  display    : \(.displayName // "-")
  email      : \(.emailAddress // "(hidden by account privacy settings)")"' <<<"$BODY"
else
  echo "  could not read /rest/api/3/myself -- the app will have to be told your"
  echo "  account id, or match on email if the on-call feed returns one."
  explain_failure
fi

# ------------------------------------------------------------- 2. schedules
rule "2. schedules this token can see"
get "${OPS}/schedules?size=100"
if [[ "$STATUS" == "200" ]]; then
  count=$(jq -r '(.values // .data // []) | length' <<<"$BODY")
  echo "  ${count} schedule(s)"
  jq -r '(.values // .data // [])[]
         | "  - \(.name // "?")  [id=\(.id // "?") tz=\(.timezone // "?") enabled=\(.enabled)]"' <<<"$BODY"
  while IFS=$'\t' read -r sid sname; do
    [[ -z "$sid" ]] && continue
    SCHEDULE_IDS+=("$sid")
    SCHEDULE_NAMES+=("$sname")
  done < <(jq -r '(.values // .data // [])[] | [.id, (.name // "?")] | @tsv' <<<"$BODY")
else
  echo "  schedules are NOT readable with this token."
  echo "  A 403 here with a working alert feed means the token lacks the"
  echo "  ops-config read scope, or you are not on any schedule's team."
  explain_failure
fi

# ------------------------------------------ 3. who is on call, per schedule
rule "3. on-call right now"
if [[ ${#SCHEDULE_IDS[@]} -eq 0 ]]; then
  echo "  skipped -- no schedules listed above."
else
  for i in "${!SCHEDULE_IDS[@]}"; do
    sid="${SCHEDULE_IDS[$i]}"
    sname="${SCHEDULE_NAMES[$i]}"
    echo "  -- ${sname}"

    get "${OPS}/schedules/${sid}/on-calls?flat=true"
    if [[ "$STATUS" == "200" ]]; then
      echo "     flat=true  : $(jq -c '.' <<<"$BODY")"
      # The exact key is undocumented and has moved between versions, so pull
      # every string leaf rather than guessing at a path.
      if [[ -n "$ACCOUNT_ID" ]] && jq -e --arg me "$ACCOUNT_ID" \
           '[.. | strings] | index($me)' <<<"$BODY" >/dev/null 2>&1; then
        ON_CALL_NOW="yes"
        echo "     >> YOUR account id is on call on this schedule right now"
      fi
      if jq -e --arg me "$ATLASSIAN_EMAIL" \
           '[.. | strings] | index($me)' <<<"$BODY" >/dev/null 2>&1; then
        ON_CALL_NOW="yes"
        echo "     >> YOUR EMAIL appears in the on-call feed (so email matching works)"
      fi
    else
      explain_failure
    fi

    get "${OPS}/schedules/${sid}/on-calls?flat=false"
    [[ "$STATUS" == "200" ]] && echo "     flat=false : $(jq -c '.' <<<"$BODY")"

    get "${OPS}/schedules/${sid}/next-on-calls?flat=true"
    if [[ "$STATUS" == "200" ]]; then
      echo "     next       : $(jq -c '.' <<<"$BODY")"
    else
      echo "     next       : HTTP $STATUS"
    fi
  done
fi

# ------------------------------------------------ 4. the .ics identifier form
rule "4. personal on-call calendar (.ics)"
echo "  Atlassian documents the path but not what userIdentifier is, so each"
echo "  candidate is tried until one is accepted."
tried_any=0
for candidate in "$ACCOUNT_ID" "$ATLASSIAN_EMAIL" "${ATLASSIAN_EMAIL%%@*}"; do
  [[ -z "$candidate" ]] && continue
  tried_any=1
  get "${OPS}/schedules/on-calls/${candidate}.ics" "text/calendar"
  if [[ "$STATUS" == "200" ]]; then
    events=$(printf '%s' "$BODY" | grep -c '^BEGIN:VEVENT' || true)
    echo "  OK   ${candidate}  -> HTTP 200, ${events} VEVENT(s)"
    [[ -z "$ICS_IDENTIFIER" ]] && ICS_IDENTIFIER="$candidate"
    # The next few shifts, which is what a "you go on call in 30 min" warning
    # would be built on.
    printf '%s' "$BODY" | grep -E '^(SUMMARY|DTSTART|DTEND)' | head -12 | sed 's/^/       /'
  else
    echo "  FAIL ${candidate}  -> HTTP ${STATUS}"
  fi
done
[[ "$tried_any" == "0" ]] && echo "  skipped -- no identifier candidates available."

# --------------------------------------------- 5. alert fields the app needs
rule "5. alert fields (owner / responders / seen / snoozed)"
get "${OPS}/alerts?size=1&sort=createdAt&order=desc"
if [[ "$STATUS" == "200" ]]; then
  one=$(jq -c '(.values // .data // [])[0] // empty' <<<"$BODY")
  if [[ -z "$one" ]]; then
    echo "  the feed is empty right now -- rerun when an alert exists."
  else
    echo "  keys present on the newest alert:"
    jq -r 'keys_unsorted[]' <<<"$one" | paste -sd' ' - | fold -sw 68 | sed 's/^/    /'
    echo "  values the app would key on:"
    jq -r '"    owner      : \(.owner // "(absent)")
    responders : \(.responders // "(absent)" | tostring)
    seen       : \(.seen // "(absent)" | tostring)
    snoozed    : \(.snoozed // "(absent)" | tostring)"' <<<"$one"
  fi
else
  echo "  the alert feed itself is not readable -- check ATLASSIAN_EMAIL/JIRA_TOKEN/CLOUD_ID"
  echo "  against alerts_10min.sh before reading anything above as an on-call problem."
  explain_failure
fi

# ------------------------------------------------------- N. jira tickets API
# The Jira Tickets button reads the *issue* API. Three things it depends on
# are worth confirming against a real tenant rather than taken on trust:
#
#   * the **site** -- Jira is not necessarily on the alert feed's tenant, and
#     pointing at the wrong one returns a valid, entirely empty ticket list.
#   * `/search/jql`, not the old `/search`, and by **POST**.
#   * API **v3**, whose description arrives as ADF rather than text.
rule "jira tickets API (the Jira Tickets button)"
echo "  site: ${JIRA_SITE}"
[[ -n "${JIRA_BASE_URL:-}" ]] && echo "  (from JIRA_BASE_URL)" \
                             || echo "  (from CLOUD_ID -- set JIRA_BASE_URL if tickets live elsewhere)"
JIRA_TICKETS="no"
JQL='assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC'
BODY=$(curl -sS -K - -X POST -H "Content-Type: application/json" \
            -H "Accept: application/json" \
            -w $'\n__STATUS__%{http_code}' \
            "${JIRA_SITE}/rest/api/3/search/jql" \
            -d "$(jq -n --arg q "$JQL" \
                    '{jql:$q,fields:["summary","status","priority","issuetype","project","updated"],maxResults:20}')" \
       <<<"user = \"${ATLASSIAN_EMAIL}:${JIRA_TOKEN}\"" 2>&1)
STATUS="${BODY##*__STATUS__}"
BODY="${BODY%$'\n'__STATUS__*}"
if [[ "$STATUS" == "200" ]]; then
  n=$(jq -r '(.issues // []) | length' <<<"$BODY")
  echo "  POST /rest/api/3/search/jql works -- ${n} open ticket(s) for this token"
  JIRA_TICKETS="yes"
  jq -r '(.issues // [])[] | "    \(.key)  \(.fields.status.name // "?")  \(.fields.summary // "")"' <<<"$BODY"
  FIRST_KEY=$(jq -r '(.issues // [])[0].key // empty' <<<"$BODY")
  if [[ -n "$FIRST_KEY" ]]; then
    # The description is the field the app has to flatten, so it is the one
    # worth reading back.
    get "${JIRA_SITE}/rest/api/3/issue/${FIRST_KEY}?fields=description"
    if [[ "$STATUS" == "200" ]]; then
      dtype=$(jq -r '.fields.description | type' <<<"$BODY")
      echo "  description arrives as: ${dtype} (object = ADF, string = v2-style; the app reads both)"
    else
      echo "  could not read ${FIRST_KEY} itself"
      explain_failure
    fi
    get "${JIRA_SITE}/rest/api/3/issue/${FIRST_KEY}/transitions"
    if [[ "$STATUS" == "200" ]]; then
      echo "  transitions available on ${FIRST_KEY}:"
      jq -r '(.transitions // [])[] | "    \(.id)  \(.name)  -> \(.to.name // "?")"' <<<"$BODY"
    else
      echo "  could not list transitions for ${FIRST_KEY} -- the ticket view would still"
      echo "  render, with a note where its buttons go."
      explain_failure
    fi
  fi
else
  printf '  search failed: HTTP %s\n' "$STATUS"
  echo "  A 404 usually means the site is wrong (this is the failure that returns"
  echo "  an empty ticket list rather than an error); 401/403 is credentials."
  explain_failure
fi

# ------------------------------------------------------------------ verdict
rule "verdict"
printf '  account id known      : %s\n' "${ACCOUNT_ID:-NO}"
printf '  schedules readable    : %s\n' "$([[ ${#SCHEDULE_IDS[@]} -gt 0 ]] && echo "yes (${#SCHEDULE_IDS[@]})" || echo NO)"
printf '  on call right now     : %s\n' "${ON_CALL_NOW:-no (or not detected)}"
printf '  .ics identifier       : %s\n' "${ICS_IDENTIFIER:-NONE ACCEPTED}"
printf '  jira tickets readable : %s\n' "${JIRA_TICKETS:-no}"
echo
if [[ -n "$ICS_IDENTIFIER" ]]; then
  echo "  One .ics request covers every schedule and carries shift start AND end,"
  echo "  so the app can poll that alone -- no enumerating schedules, and it can"
  echo "  warn ahead of a shift rather than only noticing after it starts."
elif [[ ${#SCHEDULE_IDS[@]} -gt 0 ]]; then
  echo "  No .ics identifier was accepted, but the per-schedule on-calls endpoint"
  echo "  works. The app would poll each schedule instead: more requests, and it"
  echo "  only sees the current shift, not when it ends."
else
  echo "  Nothing on-call is reachable with this token. Settle that before any"
  echo "  app work -- it is a permissions question, not a code one."
fi
