#!/usr/bin/env bash
# Fills a fresh sample stack (samples/docker-compose.yml) with everything the
# samples need, through the FluxGate backend REST API:
#
#   - the first admin, team "Samples" and environment "Development"
#   - context keys country and user_tier
#   - a Backend client for the samples and the edge; its SDK key goes to
#     samples/.env and is never printed
#   - an approval policy, the Requester role for the admin, and a second user
#     "sample-approver" to approve deployments
#   - new-checkout:   SIMPLE (boolean) flag, on for country = CA
#   - checkout-theme: CONTEXTUAL flag with string variants classic and express:
#                     express for country = CA and user_tier = plus,
#                     otherwise a weighted split of 80 % classic / 20 % express
#   - deploys the Development stage of both flags (request, approve, deploy)
#
# Usage: samples/setup.sh            (backend at http://localhost:38080)
# Needs curl, jq and openssl. Run it once per fresh stack; to start over:
#   docker compose -f samples/docker-compose.yml --profile edge down -v
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$DIR/.env"
API="${FLUXGATE_API_URL:-http://localhost:38080/api/v1}"
TOKEN=""
BODY=""

for tool in curl jq openssl; do
  command -v "$tool" >/dev/null || { echo "setup.sh needs $tool" >&2; exit 2; }
done

# env_get <name>: value of <name> in samples/.env, or empty.
env_get() { [ -f "$ENV_FILE" ] && sed -n "s/^$1=//p" "$ENV_FILE" | tail -1 || true; }

# env_set <name> <value>: write <name>=<value> to samples/.env (git-ignored, mode 600).
env_set() {
  touch "$ENV_FILE"
  chmod 600 "$ENV_FILE"
  grep -v "^$1=" "$ENV_FILE" > "$ENV_FILE.tmp" || true
  printf '%s=%s\n' "$1" "$2" >> "$ENV_FILE.tmp"
  mv "$ENV_FILE.tmp" "$ENV_FILE"
}

# call <METHOD> <path> <expected status> [json]: REST call; sets BODY, fails on another status.
call() {
  local status args=(-sS -o "$DIR/.setup-body" -w '%{http_code}' -X "$1")
  [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
  [ -n "${4:-}" ] && args+=(-H 'Content-Type: application/json' --data "$4")
  status=$(curl "${args[@]}" "$API$2") || status=000
  BODY=$(cat "$DIR/.setup-body"); rm -f "$DIR/.setup-body"
  if [ "$status" != "$3" ]; then
    echo "$1 $2: expected HTTP $3, got $status: $BODY" >&2
    exit 1
  fi
}

# items: the list in BODY, whether the API wraps it or not.
items() { printf '%s' "$BODY" | jq -c 'if type == "array" then . else (.data // .items // .content // []) end'; }

login() {
  TOKEN=""
  call POST /auth/login 200 "$(jq -nc --arg u "$1" --arg p "$2" '{username: $u, password: $p}')"
  TOKEN=$(printf '%s' "$BODY" | jq -r .token)
  USER_ID=$(printf '%s' "$BODY" | jq -r .user.id)
}

echo "Waiting for the backend at $API ..."
for _ in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' "${API%/api/v1}/health" || true)
  [ "$code" != "000" ] && break
  sleep 2
done

ADMIN_USER=$(env_get SAMPLE_ADMIN_USER); ADMIN_USER=${ADMIN_USER:-sample-admin}
ADMIN_PASSWORD=$(env_get SAMPLE_ADMIN_PASSWORD)
if [ -z "$ADMIN_PASSWORD" ]; then
  ADMIN_PASSWORD="Sample-$(openssl rand -hex 8)"
fi
env_set SAMPLE_ADMIN_USER "$ADMIN_USER"
env_set SAMPLE_ADMIN_PASSWORD "$ADMIN_PASSWORD"

echo "Creating the first admin ($ADMIN_USER) ..."
call POST /admins 201 "$(jq -nc --arg u "$ADMIN_USER" --arg p "$ADMIN_PASSWORD" \
  '{username: $u, password: $p, firstName: "Sample", lastName: "Admin", email: "sample-admin@example.test"}')"
login "$ADMIN_USER" "$ADMIN_PASSWORD"
ADMIN_ID=$USER_ID

echo "Creating team Samples and environment Development ..."
call POST /teams 201 '{"name":"Samples","description":"Flags for the FluxGate Rust SDK samples"}'
TEAM_ID=$(printf '%s' "$BODY" | jq -r .id)
call POST "/teams/$TEAM_ID/environments" 201 '{"name":"Development","active":true,"environmentType":"Development"}'
ENV_ID=$(printf '%s' "$BODY" | jq -r .id)

echo "Creating context keys country and user_tier ..."
call POST "/teams/$TEAM_ID/contexts" 201 '{"key":"country","entries":["CA","US","DE"]}'
call POST "/teams/$TEAM_ID/contexts" 201 '{"key":"user_tier","entries":["free","plus"]}'

echo "Creating Backend client rust-sdk-samples ..."
call POST "/teams/$TEAM_ID/clients" 201 "$(jq -nc --arg e "$ENV_ID" \
  '{name: "rust-sdk-samples", description: "Edge server and Rust samples", clientType: "BACKEND", environmentId: $e, webOrigins: []}')"
env_set FLUXGATE_SDK_KEY "$(printf '%s' "$BODY" | jq -r '"\(.id).\(.apiKey)"')"

echo "Creating approval policy and approver user ..."
call GET /roles 200
APPROVER_ROLE_ID=$(items | jq -r '[.[] | select(.name == "Approver")][0].id')
REQUESTER_ROLE_ID=$(items | jq -r '[.[] | select(.name == "Requester")][0].id')
# The admin requests deployments; a requester cannot approve their own request.
call POST "/users/$ADMIN_ID/roles" 200 "$(jq -nc --arg r "$REQUESTER_ROLE_ID" '{roleIds: [$r]}')"
call POST "/users/$ADMIN_ID/teams" 200 "$(jq -nc --arg t "$TEAM_ID" '{teamIds: [$t]}')"
login "$ADMIN_USER" "$ADMIN_PASSWORD"   # pick up the new role
call POST "/teams/$TEAM_ID/approval-policies" 201 "$(jq -nc --arg r "$APPROVER_ROLE_ID" \
  '{name: "Sample approvals", appliesTo: "all", requiredApprovers: 1, approverRoleIds: [$r], approverUserIds: []}')"
APPROVER_PASSWORD="Sample-$(openssl rand -hex 8)"
env_set SAMPLE_APPROVER_USER sample-approver
env_set SAMPLE_APPROVER_PASSWORD "$APPROVER_PASSWORD"
call POST /users 201 "$(jq -nc --arg p "$APPROVER_PASSWORD" \
  '{username: "sample-approver", password: $p, firstName: "Sample", lastName: "Approver", email: "sample-approver@example.test", isTemporaryPassword: false}')"
APPROVER_ID=$(printf '%s' "$BODY" | jq -r .id)
call POST "/users/$APPROVER_ID/teams" 200 "$(jq -nc --arg t "$TEAM_ID" '{teamIds: [$t]}')"
call POST "/users/$APPROVER_ID/roles" 200 "$(jq -nc --arg r "$APPROVER_ROLE_ID" '{roleIds: [$r]}')"

# create_flag <json>: create a feature; sets STAGE_ID to its Development stage.
create_flag() {
  call POST "/teams/$TEAM_ID/features" 201 "$1"
  call GET "/features/$(printf '%s' "$BODY" | jq -r .id)" 200
  STAGE_ID=$(printf '%s' "$BODY" | jq -r --arg e "$ENV_ID" '[.stages[] | select(.environment.id == $e)][0].id')
}

STAGE='[{"environmentId":"'"$ENV_ID"'","orderIndex":0,"position":"1","bucketingKey":"user_id"}]'

echo "Creating flag new-checkout (boolean) ..."
create_flag '{"key":"new-checkout","description":"Boolean flag used by the Rust SDK samples","featureType":"SIMPLE","enabled":true,"lifecycleStage":"ACTIVE","dependencies":[],"relationships":[],"stages":'"$STAGE"',"variants":[]}'
NEW_CHECKOUT_STAGE=$STAGE_ID
call PUT "/stages/$NEW_CHECKOUT_STAGE/criteria" 200 \
  '[{"priority":0,"ruleGroups":[{"logicOperator":"AND","conditions":[{"contextKey":"country","operator":"EQUALS","value":"CA"}]}]}]'

echo "Creating flag checkout-theme (contextual) ..."
create_flag '{"key":"checkout-theme","description":"Contextual flag used by the Rust SDK samples","featureType":"CONTEXTUAL","enabled":true,"lifecycleStage":"ACTIVE","dependencies":[],"relationships":[],"stages":'"$STAGE"',"variants":[{"control":"classic","value":"classic","valueType":"STRING"},{"control":"express","value":"express","valueType":"STRING"}]}'
THEME_STAGE=$STAGE_ID
call PUT "/stages/$THEME_STAGE/criteria" 200 '[
  {"priority":0,"variantSelectionMode":"SPECIFIC_VARIANT","selectedVariantControl":"express",
   "ruleGroups":[{"logicOperator":"AND","conditions":[
     {"contextKey":"country","operator":"EQUALS","value":"CA","orderIndex":0},
     {"contextKey":"user_tier","operator":"EQUALS","value":"plus","orderIndex":1}]}]},
  {"priority":1,"variantSelectionMode":"WEIGHTED_SPLIT",
   "variantAllocations":[{"variantControl":"classic","weight":80},{"variantControl":"express","weight":20}]}]'

echo "Deploying the Development stages ..."
for stage in "$NEW_CHECKOUT_STAGE" "$THEME_STAGE"; do
  call POST "/stages/$stage/request-change" 200 '{"request":"DEPLOYMENT_REQUESTED","reason":"Sample setup"}'
done
login sample-approver "$APPROVER_PASSWORD"
call GET "/teams/$TEAM_ID/approval-requests?statuses=PENDING" 200
for request in $(items | jq -r '.[].id'); do
  call POST "/approval-requests/$request/approve" 200 '{"comment":"Sample setup"}'
done
login "$ADMIN_USER" "$ADMIN_PASSWORD"
for stage in "$NEW_CHECKOUT_STAGE" "$THEME_STAGE"; do
  call POST "/stages/$stage/request-change" 200 '{"request":"DEPLOYED","reason":"Sample setup"}'
done

echo
echo "Done. samples/.env now holds FLUXGATE_SDK_KEY and the sample users' passwords."
echo "Admin UI: http://localhost:38000 (user $ADMIN_USER, password in samples/.env)"
echo "Next: docker compose -f samples/docker-compose.yml --profile edge up -d edge"
