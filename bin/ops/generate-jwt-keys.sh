#!/usr/bin/env bash
set -euo pipefail

openssl genpkey -algorithm Ed25519 -out fabro-jwt-private.pem
openssl pkey -in fabro-jwt-private.pem -pubout -out fabro-jwt-public.pem

echo ""
echo "Generated:"
echo "  fabro-jwt-private.pem  (private key — for fabro-web / FABRO_JWT_PRIVATE_KEY)"
echo "  fabro-jwt-public.pem   (public key  — for fabro-workflow / FABRO_JWT_PUBLIC_KEY)"
echo ""
echo "Set env vars with the PEM contents (including header/footer lines):"
echo ""
echo '  export FABRO_JWT_PRIVATE_KEY="$(cat fabro-jwt-private.pem)"'
echo '  export FABRO_JWT_PUBLIC_KEY="$(cat fabro-jwt-public.pem)"'
