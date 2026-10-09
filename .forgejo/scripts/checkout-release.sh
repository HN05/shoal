#!/bin/sh
# Run from the job's empty working directory; leave the release in source/.
set -eu

python3 - <<'PY'
import os
import sys

sys.path.insert(0, os.getcwd())
import release_metadata

try:
    release_metadata.tag_version(os.environ["RELEASE_TAG"])
except ValueError as error:
    raise SystemExit(str(error))
PY

git init source
git -C source remote add origin "${FORGEJO_SERVER%/}/$REPOSITORY.git"
git -C source fetch --depth 1 origin "refs/tags/$RELEASE_TAG:refs/tags/$RELEASE_TAG"
git -C source checkout --detach "refs/tags/$RELEASE_TAG"
