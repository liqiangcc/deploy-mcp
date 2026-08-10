#!/usr/bin/env bash
set -euo pipefail

ROOT="${RUNNER_TEMP:-/tmp}/deploy-mcp-real-docker-e2e"
PORT=22223
REGISTRY_PORT=5001
PROJECT=deploy-mcp-e2e
SERVICE=app
REPOSITORY="127.0.0.1:${REGISTRY_PORT}/demo-service"

rm -rf "$ROOT"
mkdir -p "$ROOT/build" "$ROOT/scripts"
echo "DEPLOY_MCP_DOCKER_E2E_WORK_ROOT=$ROOT" >> "$GITHUB_ENV"

if id deploy >/dev/null 2>&1; then
  sudo usermod --shell /bin/bash deploy
else
  sudo useradd --create-home --shell /bin/bash deploy
fi
DEPLOY_ACCOUNT_PASSWORD="$(openssl rand -hex 32)"
printf 'deploy:%s\n' "$DEPLOY_ACCOUNT_PASSWORD" | sudo chpasswd
unset DEPLOY_ACCOUNT_PASSWORD

sudo install -d -m 0700 -o deploy -g deploy /home/deploy/.ssh
sudo install -d -m 0755 -o deploy -g deploy /home/deploy/docker /home/deploy/docker/state /home/deploy/e2e-bin

if ! getent group docker >/dev/null 2>&1; then
  sudo groupadd docker
fi
sudo usermod -aG docker deploy

ssh-keygen -q -t ed25519 -N '' -f "$ROOT/client_key"
sudo install -m 0600 -o deploy -g deploy "$ROOT/client_key.pub" /home/deploy/.ssh/authorized_keys
ssh-keygen -q -t ed25519 -N '' -f "$ROOT/sshd_host_key"
cat > "$ROOT/sshd_config" <<EOF2
Port $PORT
ListenAddress 127.0.0.1
HostKey $ROOT/sshd_host_key
PidFile $ROOT/sshd.pid
AuthorizedKeysFile .ssh/authorized_keys
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PubkeyAuthentication yes
UsePAM no
PermitRootLogin no
AllowUsers deploy
Subsystem sftp internal-sftp
LogLevel VERBOSE
EOF2
sudo mkdir -p /run/sshd
sudo /usr/sbin/sshd -f "$ROOT/sshd_config" -E "$ROOT/sshd.log"
for _ in $(seq 1 30); do
  if ssh-keyscan -p "$PORT" 127.0.0.1 > "$ROOT/known_hosts" 2>/dev/null; then
    break
  fi
  sleep 0.2
done
test -s "$ROOT/known_hosts"

SSH=(ssh -i "$ROOT/client_key" -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$ROOT/known_hosts" -p "$PORT" deploy@127.0.0.1)
"${SSH[@]}" whoami | grep -Fx deploy
"${SSH[@]}" id -nG | tr ' ' '\n' | grep -Fx docker

# Run a disposable localhost registry so candidate releases have real immutable
# manifest digests instead of mutable tags or local image IDs.
docker rm -f deploy-mcp-e2e-registry >/dev/null 2>&1 || true
docker run -d --rm --name deploy-mcp-e2e-registry -p "127.0.0.1:${REGISTRY_PORT}:5000" registry:2 >/dev/null
for _ in $(seq 1 40); do
  if curl -fsS "http://127.0.0.1:${REGISTRY_PORT}/v2/" >/dev/null; then
    break
  fi
  sleep 0.25
done
curl -fsS "http://127.0.0.1:${REGISTRY_PORT}/v2/" >/dev/null

cat > "$ROOT/build/Dockerfile" <<'EOF2'
FROM alpine:3.20
ARG VERSION
ENV APP_VERSION=$VERSION
CMD ["sh", "-c", "printf '%s\\n' \"$APP_VERSION\" > /state/running-version && exec sleep 3600"]
EOF2

build_and_push() {
  local version="$1"
  local tag="$REPOSITORY:$version"
  docker build --quiet --build-arg "VERSION=$version" -t "$tag" "$ROOT/build" >/dev/null
  docker push "$tag" >/dev/null
  local ref
  ref="$(docker inspect --format='{{index .RepoDigests 0}}' "$tag")"
  test "${ref%%@*}" = "$REPOSITORY"
  printf '%s\n' "${ref#*@}"
}

DIGEST_V0="$(build_and_push v0)"
DIGEST_V1="$(build_and_push v1)"
[[ "$DIGEST_V0" =~ ^sha256:[0-9a-f]{64}$ ]]
[[ "$DIGEST_V1" =~ ^sha256:[0-9a-f]{64}$ ]]
test "$DIGEST_V0" != "$DIGEST_V1"

cat > "$ROOT/compose.yml" <<'EOF2'
services:
  app:
    image: ${DEPLOY_IMAGE:?DEPLOY_IMAGE must be set}
    volumes:
      - /home/deploy/docker/state:/state
EOF2
printf 'DEPLOY_IMAGE=%s@%s\n' "$REPOSITORY" "$DIGEST_V0" > "$ROOT/.env"
sudo install -m 0644 -o deploy -g deploy "$ROOT/compose.yml" /home/deploy/docker/compose.yml
sudo install -m 0644 -o deploy -g deploy "$ROOT/.env" /home/deploy/docker/.env

cat > "$ROOT/scripts/common" <<EOF2
#!/usr/bin/env bash
set -euo pipefail
EXPECTED_REPOSITORY='$REPOSITORY'
EXPECTED_PROJECT='$PROJECT'
EXPECTED_SERVICE='$SERVICE'
COMPOSE_FILE=/home/deploy/docker/compose.yml
ENV_FILE=/home/deploy/docker/.env
validate_service() {
  test "\$1" = "\$EXPECTED_PROJECT"
  test "\$2" = "\$EXPECTED_SERVICE"
}
validate_candidate() {
  test "\$1" = "\$EXPECTED_REPOSITORY"
  [[ "\$2" =~ ^sha256:[0-9a-f]{64}$ ]]
  validate_service "\$3" "\$4"
}
EOF2

cat > "$ROOT/scripts/compose-precheck" <<'EOF2'
#!/usr/bin/env bash
set -euo pipefail
source /home/deploy/e2e-bin/common
validate_service "$1" "$2"
docker compose -p "$1" -f "$COMPOSE_FILE" --env-file "$ENV_FILE" config --quiet
EOF2

cat > "$ROOT/scripts/compose-prepare" <<'EOF2'
#!/usr/bin/env bash
set -euo pipefail
source /home/deploy/e2e-bin/common
validate_candidate "$1" "$2" "$3" "$4"
docker pull "$1@$2" >/dev/null
EOF2

cat > "$ROOT/scripts/compose-current" <<'EOF2'
#!/usr/bin/env bash
set -euo pipefail
source /home/deploy/e2e-bin/common
validate_service "$1" "$2"
ref="$(sed -n 's/^DEPLOY_IMAGE=//p' "$ENV_FILE")"
test "${ref%%@*}" = "$EXPECTED_REPOSITORY"
digest="${ref#*@}"
[[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]
printf '%s\n' "$digest"
EOF2

cat > "$ROOT/scripts/compose-apply" <<'EOF2'
#!/usr/bin/env bash
set -euo pipefail
source /home/deploy/e2e-bin/common
validate_candidate "$1" "$2" "$3" "$4"
printf 'DEPLOY_IMAGE=%s@%s\n' "$1" "$2" > "$ENV_FILE.next"
mv "$ENV_FILE.next" "$ENV_FILE"
EOF2

cat > "$ROOT/scripts/compose-up" <<'EOF2'
#!/usr/bin/env bash
set -euo pipefail
source /home/deploy/e2e-bin/common
validate_service "$1" "$2"
docker compose -p "$1" -f "$COMPOSE_FILE" --env-file "$ENV_FILE" up -d --force-recreate "$2" >/dev/null
EOF2

cat > "$ROOT/scripts/compose-health" <<'EOF2'
#!/usr/bin/env bash
set -euo pipefail
source /home/deploy/e2e-bin/common
validate_service "$1" "$2"
docker compose -p "$1" -f "$COMPOSE_FILE" --env-file "$ENV_FILE" ps --status running --services | grep -Fx "$2" >/dev/null
test -s /home/deploy/docker/state/running-version
EOF2

cat > "$ROOT/scripts/compose-rollback" <<'EOF2'
#!/usr/bin/env bash
set -euo pipefail
source /home/deploy/e2e-bin/common
validate_candidate "$1" "$2" "$3" "$4"
printf 'DEPLOY_IMAGE=%s@%s\n' "$1" "$2" > "$ENV_FILE.next"
mv "$ENV_FILE.next" "$ENV_FILE"
EOF2

sudo install -m 0755 -o root -g root "$ROOT/scripts/common" /home/deploy/e2e-bin/common
for script in compose-precheck compose-prepare compose-current compose-apply compose-up compose-health compose-rollback; do
  sudo install -m 0755 -o root -g root "$ROOT/scripts/$script" "/home/deploy/e2e-bin/$script"
done

"${SSH[@]}" /home/deploy/e2e-bin/compose-up "$PROJECT" "$SERVICE"
for _ in $(seq 1 80); do
  if [[ "$(cat /home/deploy/docker/state/running-version 2>/dev/null || true)" == "v0" ]]; then
    break
  fi
  sleep 0.25
done
test "$(cat /home/deploy/docker/state/running-version)" = "v0"

{
  echo "DEPLOY_MCP_DOCKER_E2E_SSH_PORT=$PORT"
  echo "DEPLOY_MCP_DOCKER_E2E_KNOWN_HOSTS=$ROOT/known_hosts"
  echo "DEPLOY_MCP_DOCKER_E2E_REPOSITORY=$REPOSITORY"
  echo "DEPLOY_MCP_DOCKER_E2E_DIGEST_V0=$DIGEST_V0"
  echo "DEPLOY_MCP_DOCKER_E2E_DIGEST_V1=$DIGEST_V1"
  echo "DEPLOY_MCP_DOCKER_E2E_RUNNING_VERSION=/home/deploy/docker/state/running-version"
  echo "DEPLOY_MCP_DOCKER_E2E_REMOTE_EXEC_AUDIT=$ROOT/remote-exec-audit.jsonl"
  echo 'REMOTE_EXEC_SSH_KEY<<__DEPLOY_MCP_DOCKER_E2E_KEY__'
  cat "$ROOT/client_key"
  echo '__DEPLOY_MCP_DOCKER_E2E_KEY__'
} >> "$GITHUB_ENV"
