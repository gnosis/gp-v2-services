CREATE TABLE settlement_version
(
    version_id INT GENERATED ALWAYS AS IDENTITY,
    contract_address CHAR(42) NOT NULL,
    PRIMARY KEY (version_id)
);

INSERT INTO settlement_version(contract_address)
VALUES ('0x3328f5f2cEcAF00a2443082B657CedEAf70bfAEf');
