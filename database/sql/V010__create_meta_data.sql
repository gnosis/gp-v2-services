-- Bytes are stored in `bytea` which is a variable size byte string. There is no way to specify a
-- fixed size.

CREATE TYPE MetaDataKind AS ENUM ('referrer');

CREATE TABLE meta_data (
    version bytea NOT NULL,
    kind MetaDataKind NOT NULL,
    referrer bytea NOT NULL,
    position bigint NOT NULL,
    app_data_cid bytea NOT NULL,
    PRIMARY KEY (app_data_cid, position)
);

-- Get a referrer's meta_data.
CREATE INDEX referrer_meta_data ON meta_data USING HASH (referrer);

