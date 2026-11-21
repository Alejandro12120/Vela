#!/bin/sh
set -eu
umask 077

user=$(id -un)
if [ "$user" = root ]; then
    echo "Ejecuta este servidor de pruebas con un usuario sin privilegios de administrador." >&2
    exit 1
fi

dir=$(mktemp -d /tmp/vscp-dev-sshd.XXXXXX)
server_pid=
agent_pid=
cleanup() {
    if [ -n "$server_pid" ]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    if [ -n "$agent_pid" ]; then
        kill "$agent_pid" 2>/dev/null || true
        wait "$agent_pid" 2>/dev/null || true
    fi
    rm -rf -- "$dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

ssh-keygen -q -t ed25519 -N '' -f "$dir/host"
ssh-keygen -q -t ed25519 -N '' -f "$dir/client"
ssh-agent -D -a "$dir/agent" >"$dir/agent.log" 2>&1 &
agent_pid=$!
for _ in $(seq 1 100); do
    [ ! -S "$dir/agent" ] || break
    kill -0 "$agent_pid" 2>/dev/null || break
    sleep 0.02
done
if [ ! -S "$dir/agent" ]; then
    cat "$dir/agent.log" >&2
    exit 1
fi
SSH_AUTH_SOCK="$dir/agent" ssh-add "$dir/client" >/dev/null 2>&1

port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
cat >"$dir/sshd_config" <<CONFIG
ListenAddress 127.0.0.1
Port $port
HostKey $dir/host
PidFile $dir/pid
AuthorizedKeysFile $dir/client.pub
UsePAM no
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
MaxAuthTries 1
Subsystem sftp internal-sftp
CONFIG

/usr/sbin/sshd -D -e -f "$dir/sshd_config" >"$dir/sshd.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
    if ! kill -0 "$server_pid" 2>/dev/null; then
        break
    fi
    if nc -z 127.0.0.1 "$port" 2>/dev/null; then
        printf 'Host: 127.0.0.1\nPuerto: %s\nUsuario: %s\nClave pública: %s/client.pub\nSocket del agente: %s/agent\n' \
            "$port" "$user" "$dir" "$dir"
        wait "$server_pid"
        exit
    fi
    sleep 0.02
done
cat "$dir/sshd.log" >&2
exit 1
