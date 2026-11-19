-- The nonces of the requests the hub forwarded to a local server, each
-- until no request signed when it was is taken any longer, so a request
-- is never forwarded twice, across restarts too.
CREATE TABLE hub_forward_nonces (
    nonce TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL
);
