#!/usr/bin/env bash
# Sign a macOS release binary with Shoal's certificate, package a copy as
# Shoal.app, sign the bundle too and check both. macOS keeps privacy grants
# such as Removable Volumes for the bundle's identifier and designated
# requirement, and for the bare binary's path and requirement, so a release
# that signs with anything else fails here.
set -euo pipefail

identifier=com.shoal.cli
# The designated requirement of MACOS_SIGNING_PEM's certificate, as rcodesign
# prints it; `codesign -d -r-` shows the same requirement without parentheses.
# After replacing the certificate, sign any Mach-O binary with the new PEM and
# copy the text between "designated(3): 0: " and the final ";" from
# `rcodesign print-signature-info <binary>`.
requirement='(identifier "com.shoal.cli") and (certificate root = H"c53de619d027ecec80916106bbc82fbec9599da6")'

pem=${1:?expected the signing PEM file}
binary=${2:?expected the binary to sign and package}
version=${3:?expected the release version}
bundle=${4:?expected the Shoal.app path to create}

# Check that rcodesign signed $1 as Shoal, with its main executable at $2.
check() {
  local signed=$1 executable=$2 info identifiers flags requirements
  info=$(rcodesign print-signature-info "$signed")
  identifiers=$(sed -n 's/^ *identifier: //p' <<< "$info" | sort -u)
  flags=$(sed -n 's/^ *flags: //p' <<< "$info")
  requirements=$(sed -n "s/^ *- 'designated([0-9]*): [0-9]*: \(.*\);'\$/\1/p" <<< "$info" | sort -u)
  if ! grep -qx -- "- path: $executable" <<< "$info"; then
    echo "Expected a signed $executable in $signed" >&2
    exit 1
  fi
  if [ "$identifiers" != "$identifier" ]; then
    echo "Expected identifier $identifier in $signed, found: ${identifiers:-none}" >&2
    exit 1
  fi
  if [ -z "$flags" ] || grep -q ADHOC <<< "$flags"; then
    echo "Expected a certificate signature on $signed, found flags: ${flags:-none}" >&2
    exit 1
  fi
  if [ "$requirements" != "$requirement" ]; then
    echo "Expected designated requirement $requirement in $signed, found: ${requirements:-none}" >&2
    echo 'Check the MACOS_SIGNING_PEM secret, or update the requirement in sign-macos.sh for a new certificate' >&2
    exit 1
  fi
}

rcodesign sign --pem-file "$pem" --binary-identifier "$identifier" --timestamp-url none "$binary"
check "$binary" "$binary"

rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS"
cp "$binary" "$bundle/Contents/MacOS/shoal"
# LSUIElement keeps Shoal out of the Dock should anything open the bundle.
cat > "$bundle/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>shoal</string>
	<key>CFBundleIdentifier</key>
	<string>$identifier</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Shoal</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSUIElement</key>
	<true/>
</dict>
</plist>
EOF

rcodesign sign --pem-file "$pem" --timestamp-url none "$bundle"
check "$bundle" Contents/MacOS/shoal
