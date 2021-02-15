-- `u256`s are stored as `numeric(78,0)` which is an integer with up to 78 decimal digits.
-- This is the number of digits in `2**256 - 1`.
-- Bytes are stored in `bytea` which is a variable size byte string. There is no way to specifiy a
-- fixed size.
-- `u32`s are stored in `bigint` which is an 8 bytes signed integer because Postgre does not have
-- unsigned integers.
BEGIN;

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'orderkind') THEN
    CREATE TYPE OrderKind AS ENUM ('buy', 'sell');
  END IF;
END
$$;

CREATE TABLE IF NOT EXISTS orders (
    uid bytea PRIMARY KEY,
    owner bytea NOT NULL,
    creation_timestamp timestamptz NOT NULL,

    sell_token bytea NOT NULL,
    buy_token bytea NOT NULL,
    sell_amount numeric(78,0) NOT NULL,
    buy_amount numeric(78,0) NOT NULL,
    valid_to bigint NOT NULL,
    app_data bigint NOT NULL,
    fee_amount numeric(78,0) NOT NULL,
    kind OrderKind NOT NULL,
    partially_fillable boolean NOT NULL,
    signature bytea NOT NULL -- r + s + v
);

-- Trade events from the smart contract.
CREATE TABLE IF NOT EXISTS trades (
    block_number bigint NOT NULL,
    log_index bigint NOT NULL,
    -- Not foreign key because there can be trade events for orders we don't know.
    order_uid bytea NOT NULL,
    sell_amount numeric(78,0) NOT NULL,
    buy_amount numeric(78,0) NOT NULL,
    fee_amount numeric(78,0) NOT NULL,
    PRIMARY KEY (block_number, log_index)
);

-- OrderInvalidated events from the smart contract.
CREATE TABLE IF NOT EXISTS invalidations (
    block_number bigint NOT NULL,
    log_index bigint NOT NULL,
    order_uid bytea NOT NULL,
    PRIMARY KEY (block_number, log_index)
);

-- Indexes for common operations that should be efficient.

-- Get a specific user's orders.
CREATE INDEX IF NOT EXISTS order_owner ON orders USING HASH (owner);

-- Get all valid orders.
CREATE INDEX IF NOT EXISTS order_valid_to ON orders USING BTREE (valid_to);

-- Get all trades belonging to an order.
CREATE INDEX IF NOT EXISTS trade_order_uid on trades USING BTREE (order_uid, block_number, log_index);

-- Get all invalidations belonging to an order.
CREATE INDEX IF NOT EXISTS invalidations_order_uid on invalidations USING BTREE (order_uid, block_number, log_index);

COMMIT
