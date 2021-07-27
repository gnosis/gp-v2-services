CREATE TYPE FundLocation AS ENUM ('owner', 'vault_internal', 'vault_external');

ALTER TABLE orders
    ADD COLUMN settlement_version bytea,
    ADD COLUMN balance_from FundLocation,
    ADD COLUMN balance_to FundLocation;

UPDATE orders
SET settlement_version = '0x3328f5f2cEcAF00a2443082B657CedEAf70bfAEf',
    balance_from = 'owner',
    balance_to = 'owner'
WHERE settlement_version IS NULL;

ALTER TABLE orders
    ALTER COLUMN settlement_version SET NOT NULL,
    ALTER COLUMN balance_from SET NOT NULL,
    ALTER COLUMN balance_to SET NOT NULL;
