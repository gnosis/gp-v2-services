use super::*;
use anyhow::Result;
use futures::stream::TryStreamExt;
use futures::StreamExt;
use model::app_data::AppDataBlob;
use primitive_types::{H160, H256};
use serde_json::json;
use std::borrow::Cow;
use thiserror::Error;

#[async_trait::async_trait]
pub trait AppDataStoring: Send + Sync {
    async fn insert_app_data(&self, app_data: &AppDataBlob) -> Result<H256, InsertionError>;
    fn app_data<'a>(&'a self, filter: &'a AppDataFilter) -> BoxStream<'a, Result<AppDataBlob>>;
}

/// Any default value means that this field is unfiltered.
#[derive(Clone, Default)]
pub struct AppDataFilter {
    pub app_data_hash: Option<H256>,
    pub app_code: Option<String>,
    pub referrer: Option<H160>,
}

#[derive(Error, Debug)]
pub enum InsertionError {
    #[error("Parsing the string was not successful:")]
    ParsingStringError(#[from] serde_json::Error),
    #[error("Duplicated record for `{0}`:")]
    DuplicatedRecord(H256),
    #[error("Anyhow error:")]
    AnyhowError(#[from] anyhow::Error),
    #[error("Database error:")]
    DbError(#[from] sqlx::Error),
}

#[async_trait::async_trait]
impl AppDataStoring for Postgres {
    async fn insert_app_data(&self, app_data_value: &AppDataBlob) -> Result<H256, InsertionError> {
        let app_data = app_data_value.get_app_data()?;
        const QUERY: &str = "\
            INSERT INTO app_data (\
                app_data_hash, app_code, referrer, file_blob)\
                VALUES ($1, $2, $3, $4);";
        let app_data_hash = app_data_value.sha_hash()?;
        let referrer = app_data.metadata.unwrap_or_default().referrer;
        let address = referrer.clone().unwrap_or_default().address;
        let referrer_address_bytes;
        if referrer.is_none() {
            // considering none value to not store 0x00..0
            referrer_address_bytes = None::<&[u8]>;
        } else {
            referrer_address_bytes = Some(address.as_ref());
        }
        sqlx::query(QUERY)
            .bind(app_data_hash.clone().as_bytes())
            .bind(app_data.app_code.as_ref().map(|h160| h160.as_bytes()))
            .bind(referrer_address_bytes)
            .bind(json!(app_data_value.0))
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|err| {
                if let sqlx::Error::Database(db_err) = &err {
                    if let Some(Cow::Borrowed("23505")) = db_err.code() {
                        return InsertionError::DuplicatedRecord(app_data_hash);
                    }
                }
                InsertionError::DbError(err)
            })?;
        Ok(app_data_hash)
    }

    fn app_data<'a>(&'a self, filter: &'a AppDataFilter) -> BoxStream<'a, Result<AppDataBlob>> {
        const QUERY: &str = "\
            SELECT file_blob FROM app_data \
                WHERE
                ($1 IS NULL OR app_data_hash = $1) AND \
                ($2 IS NULL OR app_code = $2) AND \
                ($3 IS NULL OR referrer = $3);";

        sqlx::query_as(QUERY)
            .bind(filter.app_data_hash.as_ref().map(|h256| h256.as_bytes()))
            .bind(filter.app_code.as_ref().map(|string| string.as_bytes()))
            .bind(filter.referrer.as_ref().map(|h160| h160.as_bytes()))
            .fetch(&self.pool)
            .err_into()
            .and_then(|row: AppDataQueryRow| async move { row.into_app_data_blob() })
            .boxed()
    }
}

#[derive(sqlx::FromRow)]
struct AppDataQueryRow {
    file_blob: serde_json::value::Value,
}

impl AppDataQueryRow {
    fn into_app_data_blob(self) -> Result<AppDataBlob> {
        Ok(AppDataBlob(self.file_blob))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;
    #[tokio::test]
    #[ignore]
    async fn postgres_insert_same_order_twice_fails() {
        let db = Postgres::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        let json = json!(
        {
            "appCode": "CowSwap",
            "version": "1.0.0",
            "metadata": {
              "referrer": {
                "address":  "0x424a46612794dbb8000194937834250dc723ffa5",
                "version": "0.3.4",
              }
            }
        }
        );
        let app_data_blob = AppDataBlob(json);
        db.insert_app_data(&app_data_blob).await.unwrap();
        match db.insert_app_data(&app_data_blob).await {
            Err(InsertionError::DuplicatedRecord(_hash)) => (),
            _ => panic!("Expecting DuplicatedRecord error"),
        };
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_app_data_roundtrip_with_different_filters() {
        let db = Postgres::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        let filter = AppDataFilter::default();
        assert!(db.app_data(&filter).boxed().next().await.is_none());
        let json = json!(
        {
            "appCode": "CowSwap",
            "version": "1.0.0",
            "metadata": {
              "referrer": {
                "address":  "0x424a46612794dbb8000194937834250dc723ffa5",
                "version": "0.3.4",
              }
            }
        }
        );
        let app_data_blob = AppDataBlob(json);
        db.insert_app_data(&app_data_blob).await.unwrap();
        let new_filter = AppDataFilter {
            app_data_hash: Some(app_data_blob.sha_hash().unwrap()),
            app_code: None,
            referrer: None,
        };
        assert_eq!(
            *db.app_data(&new_filter)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .first()
                .unwrap(),
            app_data_blob
        );
        let json = json!(
        {
            "appCode": null,
            "version": "1.0.0",
            "metadata": {
              "referrer": {
                "address":  "0x224a46612794dbb8000194937834250dc723ffa5",
                "version": "0.3.4",
              }
            }
        }
        );
        let app_data_blob = AppDataBlob(json);
        let new_filter = AppDataFilter {
            app_data_hash: None,
            app_code: None,
            referrer: Some(
                app_data_blob
                    .get_app_data()
                    .unwrap()
                    .metadata
                    .unwrap()
                    .referrer
                    .unwrap()
                    .address,
            ),
        };
        db.insert_app_data(&app_data_blob).await.unwrap();
        assert_eq!(
            *db.app_data(&new_filter)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .first()
                .unwrap(),
            app_data_blob
        );
        let json = json!(
        {
            "appCode": "testing",
            "version": "1.0.0",
            "metadata": null,
        }
        );
        let app_data_blob = AppDataBlob(json);
        let new_filter = AppDataFilter {
            app_data_hash: None,
            app_code: app_data_blob.get_app_data().unwrap().app_code,
            referrer: None,
        };
        db.insert_app_data(&app_data_blob).await.unwrap();
        assert_eq!(
            *db.app_data(&new_filter)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .first()
                .unwrap(),
            app_data_blob
        );
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_app_data_roundtrip_with_minimal_data() {
        let db = Postgres::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        let filter = AppDataFilter::default();
        println!("{:?}", db.app_data(&filter).boxed().next().await);
        assert!(db.app_data(&filter).boxed().next().await.is_none());
        let json = json!(
        {
            "appCode": serde_json::value::Value::Null,
            "version": "1.0.0",
            "metadata": serde_json::value::Value::Null
        }
        );
        let app_data_blob = AppDataBlob(json);
        db.insert_app_data(&app_data_blob).await.unwrap();
        let new_filter = AppDataFilter::default();
        assert_eq!(
            *db.app_data(&new_filter)
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .first()
                .unwrap(),
            app_data_blob
        );
    }
}
