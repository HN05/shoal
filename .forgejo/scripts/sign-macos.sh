#!/usr/bin/env bash
# Sign a macOS release binary with Shoal's certificate and check the result.
# macOS keeps privacy grants such as Full Disk Access for the designated
# requirement, so a release that signs with anything else fails here.
set -euo pipefail

identifier=com.shoal.cli
# The designated requirement of MACOS_SIGNING_PEM's certificate, as rcodesign
# prints it; `codesign -d -r-` shows the same requirement without parentheses.
# After replacing the certificate, sign any Mach-O binary with the new PEM and
# copy the text between "designated(3): 0: " and the final ";" from
# `rcodesign print-signature-info <binary>`.
requirement='(identifier "com.shoal.cli") and (certificate root = H"c53de619d027ecec80916106bbc82fbec9599da6")'

pem=${1:?expected the signing PEM file}
binary=${2:?expected the binary to sign}

rcodesign sign --pem-file "$pem" --binary-identifier "$identifier" --timestamp-url none "$binary"

info=$(rcodesign print-signature-info "$binary")
identifiers=$(sed -n 's/^ *identifier: //p' <<< "$info" | sort -u)
flags=$(sed -n 's/^ *flags: //p' <<< "$info")
requirements=$(sed -n "s/^ *- 'designated([0-9]*): [0-9]*: \(.*\);'\$/\1/p" <<< "$info" | sort -u)
if [ "$identifiers" != "$identifier" ]; then
  echo "Expected identifier $identifier in $binary, found: ${identifiers:-none}" >&2
  exit 1
fi
if [ -z "$flags" ] || grep -q ADHOC <<< "$flags"; then
  echo "Expected a certificate signature on $binary, found flags: ${flags:-none}" >&2
  exit 1
fi
if [ "$requirements" != "$requirement" ]; then
  echo "Expected designated requirement $requirement in $binary, found: ${requirements:-none}" >&2
  echo 'Check the MACOS_SIGNING_PEM secret, or update the requirement in sign-macos.sh for a new certificate' >&2
  exit 1
fi
