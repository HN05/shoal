#!/bin/sh
# Install Shoal @VERSION@, the hub's version, from its release, start its
# daemon and join the hub with the pairing code given:
#   curl -fsSL <hub url>/install | sh -s -- '<pairing code>'
set -eu

version='@VERSION@'
releases='@RELEASES@'

fail() {
	echo "shoal install: $*" >&2
	exit 1
}

[ $# -eq 1 ] || fail "give the pairing code from shoal hub pair: sh -s -- '<code>'"
code=$1

case $(uname -s) in
Linux) os=linux ;;
Darwin) os=macos ;;
*) fail "Shoal releases do not support $(uname -s)" ;;
esac
case $(uname -m) in
arm64 | aarch64) arch=arm64 ;;
x86_64 | amd64) arch=x86_64 ;;
*) fail "Shoal releases do not support $(uname -m)" ;;
esac

bin=$HOME/.local/bin
if [ "$os" = macos ]; then
	asset=shoal-v$version-$os-$arch-app.tar.gz
	entry=Shoal.app
	place=$HOME/Applications
else
	asset=shoal-v$version-$os-$arch
	entry=shoal
	place=$bin
fi

for existing in "$place/$entry" "$bin/shoal"; do
	if [ -e "$existing" ] || [ -L "$existing" ]; then
		fail "$existing exists already; where Shoal is installed, run shoal hub join '<code>' instead"
	fi
done

download() {
	curl -q -fsSL --proto '=https,file' --proto-redir '=https' -o "$2" "$releases/download/v$version/$1"
}

checksum() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d ' ' -f 1
	else
		shasum -a 256 "$1" | cut -d ' ' -f 1
	fi
}

staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT
download SHA256SUMS "$staging/SHA256SUMS" || fail "could not download the checksums of Shoal $version"
download "$asset" "$staging/$asset" || fail "could not download $asset"
expected=$(awk -v asset="$asset" '$2 == asset { print $1 }' "$staging/SHA256SUMS")
[ -n "$expected" ] || fail "Shoal $version has no $asset"
[ "$(checksum "$staging/$asset")" = "$expected" ] || fail "$asset does not match its published checksum"

if [ "$os" = macos ]; then
	tar -xzf "$staging/$asset" -C "$staging" Shoal.app
	executable=$place/Shoal.app/Contents/MacOS/shoal
	unpacked=$staging/Shoal.app/Contents/MacOS/shoal
else
	mv "$staging/$asset" "$staging/shoal"
	chmod 755 "$staging/shoal"
	executable=$place/shoal
	unpacked=$staging/shoal
fi
reported=$("$unpacked" --version) || fail "the downloaded shoal does not run here"
[ "$reported" = "shoal $version" ] || fail "the downloaded shoal reports $reported instead of version $version"

mkdir -p "$place" "$bin"
mv "$staging/$entry" "$place/$entry"
if [ "$os" = macos ]; then
	ln -s "$executable" "$bin/shoal"
fi
echo "Installed Shoal $version at $place/$entry"
case ":$PATH:" in
*":$bin:"*) ;;
*) echo "Add $bin to PATH to run shoal there" ;;
esac

"$executable" daemon install
"$executable" hub join "$code"
