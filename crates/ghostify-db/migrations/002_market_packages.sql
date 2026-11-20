-- Download information of the latest version of each marketplace listing.
CREATE TABLE market_packages (
    listing_id    TEXT PRIMARY KEY NOT NULL,
    version       TEXT NOT NULL,
    download_url  TEXT NOT NULL,
    sha256        TEXT NOT NULL CHECK (length(sha256) = 64),
    size          INTEGER NOT NULL CHECK (size >= 0),
    permissions   TEXT NOT NULL DEFAULT '[]',
    index_version INTEGER NOT NULL
);
