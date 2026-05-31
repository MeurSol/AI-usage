#!/usr/bin/env bash
# One-time setup: create a self-signed code-signing identity in the login
# keychain so AI-usage can be signed with a STABLE identity across rebuilds.
# That keeps the macOS Keychain "Always Allow" grant for the OAuth token from
# being revoked every time the binary is rebuilt.
#
# This is self-signed (not Apple-notarized): it does not satisfy Gatekeeper for
# distribution, it only gives a stable local identity. Run once, interactively
# (it may ask to unlock your login keychain).
set -euo pipefail

IDENTITY="AI-usage Local"

# Note: a self-signed cert is untrusted for Gatekeeper, so it does NOT appear
# under `find-identity -v` (valid only) — but codesign can still sign with it,
# and the resulting designated requirement is stable. So match without -v.
if security find-identity -p codesigning | grep -q "$IDENTITY"; then
    echo "Signing identity '$IDENTITY' already exists. Nothing to do."
    exit 0
fi

echo "==> Creating self-signed code-signing identity '$IDENTITY'"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/openssl.cnf" <<EOF
[req]
distinguished_name=dn
x509_extensions=v3
prompt=no
[dn]
CN=$IDENTITY
[v3]
basicConstraints=critical,CA:false
keyUsage=critical,digitalSignature
extendedKeyUsage=critical,codeSigning
EOF
openssl req -x509 -newkey rsa:2048 -keyout "$TMP/key.pem" -out "$TMP/cert.pem" \
    -days 3650 -nodes -config "$TMP/openssl.cnf" >/dev/null 2>&1
# -legacy: emit PKCS12 with SHA1 MAC / legacy PBE that macOS `security` reads
# (OpenSSL 3's default MAC fails import with "MAC verification failed").
openssl pkcs12 -export -legacy -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
    -out "$TMP/id.p12" -passout pass:aiusage -name "$IDENTITY" >/dev/null 2>&1

KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
echo "==> Importing into $KEYCHAIN (codesign is granted access)"
security import "$TMP/id.p12" -k "$KEYCHAIN" -P aiusage -T /usr/bin/codesign

echo "Done. '$IDENTITY' is ready. Now run scripts/install.sh to (re)sign."
