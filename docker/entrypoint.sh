#!/bin/sh
set -eu

docker_socket_path() {
    case "${DOCKER_HOST:-}" in
        "") printf '%s\n' /var/run/docker.sock ;;
        unix://*) printf '%s\n' "${DOCKER_HOST#unix://}" ;;
        *) return 1 ;;
    esac
}

ensure_docker_socket_group() {
    socket_path="$(docker_socket_path)" || return 0
    [ -S "$socket_path" ] || return 0

    socket_gid="$(stat -c '%g' "$socket_path")"
    case " $(id -G fabro) " in
        *" $socket_gid "*) return 0 ;;
    esac

    socket_group="$(
        awk -F: -v gid="$socket_gid" '$3 == gid { print $1; exit }' /etc/group || true
    )"
    if [ -z "$socket_group" ]; then
        socket_group="docker-sock-$socket_gid"
        addgroup -S -g "$socket_gid" "$socket_group"
    fi

    addgroup fabro "$socket_group"
}

# When started as root (the default), ensure the storage volume is writable
# by the unprivileged fabro user, then drop privileges.
if [ "$(id -u)" = 0 ]; then
    ensure_docker_socket_group
    mkdir -p "${FABRO_HOME:-/storage/.home}"
    chown fabro:fabro /storage
    chown -R fabro:fabro "${FABRO_HOME:-/storage/.home}"
    exec su-exec fabro "$@"
fi

exec "$@"
