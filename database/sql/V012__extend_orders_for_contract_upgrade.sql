CREATE TYPE SellTokenSource AS ENUM ('erc20', 'internal', 'external');
CREATE TYPE BuyTokenDestination AS ENUM ('erc20', 'internal');

-- While we could have simply added columns, set them to not null and made the update values defaults,
-- This would mean that we will forever have to ensure we don't accidentally insert without specifying
-- these values explicitly. This is especially awkward for the settlement_contract, since the default
-- would be the old contract version. For this reason, we have chosen to go with the approach of
-- 1. Add columns, setting them not null with default values,
ALTER TABLE orders
    ADD COLUMN settlement_contract bytea NOT NULL default '\x3328f5f2cEcAF00a2443082B657CedEAf70bfAEf',
    ADD COLUMN sell_token_balance SellTokenSource NOT NULL default 'erc20',
    ADD COLUMN buy_token_balance BuyTokenDestination NOT NULL default 'erc20';

-- 2. Drop defaults
ALTER TABLE orders
    ALTER COLUMN settlement_contract DROP DEFAULT,
    ALTER COLUMN sell_token_balance DROP DEFAULT,
    ALTER COLUMN buy_token_balance DROP DEFAULT;

CREATE INDEX version_idx ON orders USING BTREE (settlement_contract);