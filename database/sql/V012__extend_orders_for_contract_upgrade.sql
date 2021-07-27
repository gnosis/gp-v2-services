CREATE TYPE FundLocation AS ENUM ('owner', 'vault_internal', 'vault_external');

ALTER TABLE orders
    ADD COLUMN version_id INT,
    ADD COLUMN balance_from FundLocation,
    ADD COLUMN balance_to FundLocation,
    ADD CONSTRAINT fk_version
        FOREIGN KEY (version_id)
            REFERENCES settlement_version (version_id);


UPDATE orders
SET settlement_version = 1,
    balance_from = 'owner',
    balance_to = 'owner'
WHERE settlement_version IS NULL;

ALTER TABLE orders
    ALTER COLUMN settlement_version SET NOT NULL,
    ALTER COLUMN balance_from SET NOT NULL,
    ALTER COLUMN balance_to SET NOT NULL;
