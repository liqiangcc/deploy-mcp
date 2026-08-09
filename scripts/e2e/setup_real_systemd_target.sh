#!/usr/bin/env bash
set -euo pipefail

ROOT="${RUNNER_TEMP:-/tmp}/deploy-mcp-real-e2e"
PORT=22222
SERVICE=deploy-mcp-e2e.service

rm -rf "$ROOT"
mkdir -p "$ROOT/artifacts"
echo "DEPLOY_MCP_E2E_WORK_ROOT=$ROOT" >> "$GITHUB_ENV"

if id deploy >/dev/null 2>&1; then
  sudo usermod --shell /bin/bash deploy
else
  sudo useradd --create-home --shell /bin/bash deploy
fi
# useradd creates a locked password entry on Ubuntu. Unlock the account so
# sshd permits public-key authentication; password auth remains disabled in
# the dedicated sshd configuration below.
sudo passwd -d deploy

sudo install -d -m 0700 -o deploy -g deploy /home/deploy/.ssh
sudo install -d -m 0755 -o deploy -g deploy /home/deploy/staging /home/deploy/app /home/deploy/backup

ssh-keygen -q -t ed25519 -N '' -f "$ROOT/client_key"
sudo install -m 0600 -o deploy -g deploy "$ROOT/client_key.pub" /home/deploy/.ssh/authorized_keys

ssh-keygen -q -t ed25519 -N '' -f "$ROOT/sshd_host_key"
cat > "$ROOT/sshd_config" <<EOF
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
EOF

sudo mkdir -p /run/sshd
sudo /usr/sbin/sshd -f "$ROOT/sshd_config" -E "$ROOT/sshd.log"

for _ in $(seq 1 30); do
  if ssh-keyscan -p "$PORT" 127.0.0.1 > "$ROOT/known_hosts" 2>/dev/null; then
    break
  fi
  sleep 0.2
done

test -s "$ROOT/known_hosts"
ssh \
  -i "$ROOT/client_key" \
  -o BatchMode=yes \
  -o IdentitiesOnly=yes \
  -o StrictHostKeyChecking=yes \
  -o UserKnownHostsFile="$ROOT/known_hosts" \
  -p "$PORT" \
  deploy@127.0.0.1 whoami | grep -Fx deploy

build_jar() {
  local version="$1"
  local output="$2"
  local work="$ROOT/java-$version"
  mkdir -p "$work/classes"
  cat > "$work/Demo.java" <<EOF
import java.nio.file.Files;
import java.nio.file.Path;

public final class Demo {
    public static void main(String[] args) throws Exception {
        Files.writeString(Path.of("/home/deploy/app/running-version"), "$version\\n");
        while (true) {
            Thread.sleep(1000L);
        }
    }
}
EOF
  javac -d "$work/classes" "$work/Demo.java"
  printf 'Main-Class: Demo\n' > "$work/MANIFEST.MF"
  jar cfm "$output" "$work/MANIFEST.MF" -C "$work/classes" .
}

build_jar v0 "$ROOT/demo-v0.jar"
build_jar v1 "$ROOT/artifacts/demo-v1.jar"
sudo install -m 0644 -o deploy -g deploy "$ROOT/demo-v0.jar" /home/deploy/app/demo.jar

JAVA_BIN="$(readlink -f "$(command -v java)")"
SYSTEMCTL_BIN="$(readlink -f "$(command -v systemctl)")"

cat > "$ROOT/$SERVICE" <<EOF
[Unit]
Description=deploy-mcp disposable E2E service
After=network.target

[Service]
Type=simple
User=deploy
Group=deploy
ExecStart=$JAVA_BIN -jar /home/deploy/app/demo.jar
Restart=no

[Install]
WantedBy=multi-user.target
EOF
sudo install -m 0644 "$ROOT/$SERVICE" "/etc/systemd/system/$SERVICE"

printf 'deploy ALL=(root) NOPASSWD: %s restart %s\n' "$SYSTEMCTL_BIN" "$SERVICE" | sudo tee /etc/sudoers.d/deploy-mcp-e2e >/dev/null
sudo chmod 0440 /etc/sudoers.d/deploy-mcp-e2e
sudo visudo -cf /etc/sudoers.d/deploy-mcp-e2e

sudo systemctl daemon-reload
sudo systemctl restart "$SERVICE"

for _ in $(seq 1 40); do
  if sudo systemctl is-active --quiet "$SERVICE" && [[ "$(cat /home/deploy/app/running-version 2>/dev/null || true)" == "v0" ]]; then
    break
  fi
  sleep 0.25
done

sudo systemctl is-active --quiet "$SERVICE"
test "$(cat /home/deploy/app/running-version)" = "v0"

{
  echo "DEPLOY_MCP_E2E_SSH_PORT=$PORT"
  echo "DEPLOY_MCP_E2E_KNOWN_HOSTS=$ROOT/known_hosts"
  echo "DEPLOY_MCP_E2E_ARTIFACT_ROOT=$ROOT/artifacts"
  echo "DEPLOY_MCP_E2E_ARTIFACT=$ROOT/artifacts/demo-v1.jar"
  echo "DEPLOY_MCP_E2E_RUNNING_VERSION=/home/deploy/app/running-version"
  echo "DEPLOY_MCP_E2E_REMOTE_EXEC_AUDIT=$ROOT/remote-exec-audit.jsonl"
  echo 'REMOTE_EXEC_SSH_KEY<<__DEPLOY_MCP_E2E_KEY__'
  cat "$ROOT/client_key"
  echo '__DEPLOY_MCP_E2E_KEY__'
} >> "$GITHUB_ENV"
