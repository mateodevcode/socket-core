#!/usr/bin/env bash
# S4: valida el filtro con fail2ban-regex sobre líneas de ejemplo.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG="$ROOT/fail2ban/test/sample.log"
FILTER="$ROOT/fail2ban/filter.d/socketcore-auth.conf"

OUT="$(fail2ban-regex "$LOG" "$FILTER" --print-all-matched)"
printf '%s\n' "$OUT"

RESUMEN="$(printf '%s\n' "$OUT" | grep -E '^Lines:' | tail -1 || true)"
if [ -z "$RESUMEN" ]; then
    echo "❌ no se encontró el resumen de fail2ban-regex" >&2
    exit 1
fi
if ! printf '%s\n' "$RESUMEN" | grep -q '2 matched'; then
    echo "❌ se esperaban 2 coincidencias (401/403): $RESUMEN" >&2
    exit 1
fi
if ! printf '%s\n' "$RESUMEN" | grep -q '3 missed'; then
    echo "❌ se esperaban 3 líneas no coincidentes (200/429/tricky): $RESUMEN" >&2
    exit 1
fi
# Las líneas coincidentes deben ser solo 401/403.
MATCHED="$(printf '%s\n' "$OUT" | sed -n '/Matched line(s)/,/Missed line(s)/p' | grep -E '^\|  ' || true)"
if [ "$(printf '%s\n' "$MATCHED" | grep -cE '" (401|403) [0-9]+ ')" -ne 2 ]; then
    echo "❌ las 2 coincidencias no son 401/403" >&2
    exit 1
fi
echo "✅ fail2ban-regex OK: 2 coincidencias (401/403), 200/429/tricky fuera"
