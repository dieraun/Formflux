#!/usr/bin/env sh
# Fail if a network, telemetry or cloud SDK appears in the dependency graph.
set -eu

FORBIDDEN='reqwest|hyper|ureq|curl|isahc|surf|attohttpc|awc|tungstenite|rustls|native-tls|openssl|sentry|segment|mixpanel|amplitude'

echo "Collecting dependency graph..."
if cargo tree --workspace --prefix none > /tmp/formflux-tree.txt 2>/dev/null; then
  if grep -E "^($FORBIDDEN) v" /tmp/formflux-tree.txt; then
    echo "error: forbidden network/telemetry dependency detected" >&2
    exit 1
  fi
else
  echo "warning: cargo tree failed; cargo-deny remains the authority" >&2
fi

echo "Scanning sources for HTTP clients..."
if grep -RInE '\b(reqwest|ureq|attohttpc)::|https?://[a-zA-Z0-9]' \
  crates --include='*.rs' | grep -vE 'example\.invalid|www\.w3\.org|github\.com/dieraun/Formflux'; then
  echo "error: network code or URL found in source" >&2
  exit 1
fi

echo "offline policy check passed"
