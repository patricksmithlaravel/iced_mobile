#!/usr/bin/env bash
# Makes throwaway signing material for icm's macOS release tests: a new
# keychain holding a self-signed code-signing identity, for ICM_KEYCHAIN.
#
#   test-identity.sh <keychain path> <keychain password> <common name>
#
# Prints `keychain=<path>` and `sha1=<SHA-1 of the certificate>`. The
# keychain is unlocked, codesign may use the key without a prompt, and the
# keychain does not join the user's search list (checked; the script fails
# if the list changed). Delete it with `security delete-keychain <path>`.
# The certificate is not trusted (CSSMERR_TP_NOT_TRUSTED) and expires in
# two days: it can sign, but nothing outside this machine accepts it.
set -euo pipefail

keychain=$1
password=$2
name=$3
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
before=$(security list-keychains -d user)

cat >"$dir/cert.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = ext
[dn]
CN = $name
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF
# The system's LibreSSL: its PKCS#12 encryption is what `security import`
# reads (OpenSSL 3's default is not).
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 2 -config "$dir/cert.cnf" \
    -keyout "$dir/key.pem" -out "$dir/cert.pem" >/dev/null 2>&1
/usr/bin/openssl pkcs12 -export -inkey "$dir/key.pem" -in "$dir/cert.pem" \
    -out "$dir/id.p12" -passout "pass:$password" -name "$name" >/dev/null 2>&1

mkdir -p "$(dirname "$keychain")"
[ -f "$keychain" ] || security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$dir/id.p12" -k "$keychain" -P "$password" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null

if [ "$(security list-keychains -d user)" != "$before" ]; then
    echo "the user's keychain search list changed" >&2
    exit 1
fi
printf 'keychain=%s\n' "$keychain"
printf 'sha1=%s\n' "$(/usr/bin/openssl x509 -in "$dir/cert.pem" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
