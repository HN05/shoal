-- This daemon's Ed25519 key pair, as PKCS#8, which proves it to the hub it
-- joins or, as the hub, to its members.
CREATE TABLE hub_identity (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    private_key BLOB NOT NULL
);

-- The machines and devices paired with this daemon as the hub, its own
-- machine among them, each with the public key its links prove. A revoked
-- member stays, so that its credential is refused as revoked.
CREATE TABLE hub_members (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('machine','device')),
    name TEXT NOT NULL,
    public_key TEXT NOT NULL UNIQUE,
    joined_at INTEGER NOT NULL,
    version TEXT,
    last_seen INTEGER,
    revoked_at INTEGER
);

-- The hub this daemon joined: the ID it gave this machine, the hub's key
-- each link checks, and the URL it paired at, none through a command. A
-- revoked membership stays until the machine leaves or joins again, so
-- that the daemon tells why it no longer connects.
CREATE TABLE hub_membership (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    member_id TEXT NOT NULL,
    name TEXT NOT NULL,
    hub_key TEXT NOT NULL,
    url TEXT,
    joined_at INTEGER NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1))
);
