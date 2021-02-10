use super::Database;
use crate::integer_conversions::*;

use anyhow::{anyhow, Context, Result};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use ethcontract::{H160, U256};
use futures::StreamExt;

#[derive(PartialEq, Debug, Clone, Copy)]
pub struct MinFeeMeasurement {
    token: H160,
    expiry: DateTime<Utc>,
    min_fee: U256,
}

impl Database {
    pub async fn save_fee_measurement(&self, measurement: MinFeeMeasurement) -> Result<()> {
        const QUERY: &str =
            "INSERT INTO min_fee_measurements (token, expiration_timestamp, min_fee) VALUES ($1, $2, $3);";
        sqlx::query(QUERY)
            .bind(measurement.token.as_bytes())
            .bind(measurement.expiry)
            .bind(u256_to_big_decimal(&measurement.min_fee))
            .execute(&self.pool)
            .await
            .context("insert MinFeeMeasurement failed")
            .map(|_| ())
    }

    pub async fn load_fee_measurements(
        &self,
        token: H160,
        min_expiry: DateTime<Utc>,
    ) -> Vec<MinFeeMeasurement> {
        const QUERY: &str = "\
            SELECT expiration_timestamp, min_fee FROM min_fee_measurements \
            WHERE token = $1 AND expiration_timestamp >= $2
            ";

        let results = sqlx::query_as(QUERY)
            .bind(token.as_bytes())
            .bind(min_expiry)
            .fetch(&self.pool)
            .collect::<Vec<_>>()
            .await;

        results
            .into_iter()
            .filter_map(
                |result: Result<MinFeeMeasurementQueryRow, _>| match result {
                    Ok(row) => row.into_measurement(token).ok(),
                    Err(err) => {
                        tracing::error!(?err, "Fetching min fee from db");
                        None
                    }
                },
            )
            .collect()
    }
}

#[derive(sqlx::FromRow)]
struct MinFeeMeasurementQueryRow {
    expiration_timestamp: DateTime<Utc>,
    min_fee: BigDecimal,
}

impl MinFeeMeasurementQueryRow {
    fn into_measurement(self, token: H160) -> Result<MinFeeMeasurement> {
        Ok(MinFeeMeasurement {
            token,
            expiry: self.expiration_timestamp,
            min_fee: big_decimal_to_u256(&self.min_fee)
                .ok_or_else(|| anyhow!("min_fee is not an unsigned integer"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    #[tokio::test]
    #[ignore]
    async fn save_and_load_fee_measurements() {
        let db = Database::new("postgresql://").unwrap();
        db.clear().await.unwrap();

        let now = Utc::now();
        let token_a = H160::from_low_u64_be(1);
        let token_b = H160::from_low_u64_be(2);
        let token_a_measurement = MinFeeMeasurement {
            token: token_a,
            min_fee: 100.into(),
            expiry: now,
        };

        let another_token_a_measurement = MinFeeMeasurement {
            token: token_a,
            min_fee: 200.into(),
            expiry: now + Duration::seconds(60),
        };

        let token_b_measurement = MinFeeMeasurement {
            token: token_b,
            min_fee: 10.into(),
            expiry: now,
        };

        db.save_fee_measurement(token_a_measurement).await.unwrap();
        db.save_fee_measurement(another_token_a_measurement)
            .await
            .unwrap();
        db.save_fee_measurement(token_b_measurement).await.unwrap();

        assert_eq!(db.load_fee_measurements(token_a, now).await.len(), 2);
        let results = db
            .load_fee_measurements(token_a, now + Duration::seconds(30))
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], another_token_a_measurement);

        let results = db.load_fee_measurements(token_b, now).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], token_b_measurement);
        assert_eq!(
            db.load_fee_measurements(token_b, now + Duration::seconds(30))
                .await
                .len(),
            0
        );
    }
}
