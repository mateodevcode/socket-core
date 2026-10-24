#!/bin/sh
# Entrypoint del contenedor: verifica que el volumen de datos sea escribible por
# el usuario no-root (UID 10001) antes de arrancar, y luego cede el control.
set -eu

if [ ! -w /app/data ]; then
    echo "FATAL: /app/data no es escribible por UID $(id -u)." >&2
    echo "En el host ejecuta: sudo chown -R 10001:10001 ./data" >&2
    exit 1
fi

if ! touch /app/data/.write-test 2>/dev/null; then
    echo "FATAL: no se pueden crear ficheros en /app/data (SQLite lo necesita)." >&2
    echo "En el host ejecuta: sudo chown -R 10001:10001 ./data" >&2
    exit 1
fi
rm -f /app/data/.write-test

exec /app/socket-core
