CREATE TYPE BalanceFrom AS ENUM ('owner', 'vault_internal', 'vault_external');
CREATE TYPE BalanceTo AS ENUM ('owner', 'vault_internal');

-- While we could have simply added columns, set them to not null and made the update values defaults,
-- This would mean that we will forever have to ensure we don't accidentally insert without specifying
-- these values explicitly. This is especially awkward for the settlement_version, since the default
-- would be the old contract version. For this reason, we have chosen to go with the approach of
-- 1. Add columns,
-- 2. update old records with appropriate values,
-- 3. Set new columns to NOT NULL

ALTER TABLE orders
    ADD COLUMN settlement_version CHAR(42),
    ADD COLUMN balance_from BalanceFrom,
    ADD COLUMN balance_to BalanceTo;

UPDATE orders
SET settlement_version = '0x3328f5f2cEcAF00a2443082B657CedEAf70bfAEf',
    balance_from = 'owner',
    balance_to = 'owner'
WHERE settlement_version IS NULL;

ALTER TABLE orders
    ALTER COLUMN settlement_version SET NOT NULL,
    ALTER COLUMN balance_from SET NOT NULL,
    ALTER COLUMN balance_to SET NOT NULL;
