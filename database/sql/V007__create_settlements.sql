CREATE TABLE settlements
(
    tx_hash   bytea  NOT NULL,
    block_number bigint NOT NULL,
    log_index bigint NOT NULL,
    solver    bytea NOT NULL,

    PRIMARY KEY (tx_hash, log_index)
);