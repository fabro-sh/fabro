#!/bin/sh
set -eu

# When started as root (the default), ensure the storage volume is writable
# by the unprivileged fabro user, then drop privileges.
if [ "$(id -u)" = 0 ]; then
    mkdir -p "${FABRO_HOME:-/storage/.home}"
    chown fabro:fabro /storage
    chown -R fabro:fabro "${FABRO_HOME:-/storage/.home}"
    exec su-exec fabro "$@"
fi

exec "$@"
