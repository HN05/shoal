#!/bin/sh
# Run from the job's empty working directory; leave the release in source/.
set -eu

python3 - <<'PY'
import os
import re

if not re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", os.environ["RELEASE_TAG"]):
    raise SystemExit("expected stable vX.Y.Z tag")
PY

git init source
git -C source remote add origin "${FORGEJO_SERVER%/}/$REPOSITORY.git"
git -C source fetch --depth 1 origin "refs/tags/$RELEASE_TAG:refs/tags/$RELEASE_TAG"
git -C source checkout --detach "refs/tags/$RELEASE_TAG"
