use super::*;
use crate::conversions::*;
use anyhow::Result;
use futures::stream::TryStreamExt;
use futures::StreamExt;
use model::meta_data::{AppData, MetaData, MetaDataKind};
use std::borrow::Cow;

#[async_trait::async_trait]
pub trait AppDataStoring: Send + Sync {
    async fn insert_app_data(&self, app_data: &AppData) -> Result<(), InsertionError>;
    async fn get_complete_app_data(&self, filter: &AppDataFilter) -> Result<Option<AppData>>;
    fn app_data<'a>(&'a self, filter: &'a AppDataFilter) -> BoxStream<'a, Result<AppData>>;
    fn meta_data<'a>(&'a self, filter: &'a AppDataFilter) -> BoxStream<'a, Result<MetaData>>;
}

/// Any default value means that this field is unfiltered.
#[derive(Clone, Default)]
pub struct AppDataFilter {
    pub app_data_cid: Option<String>,
}

#[derive(Debug)]
pub enum InsertionError {
    DuplicatedRecord,
    AnyhowError(anyhow::Error),
    DbError(sqlx::Error),
}

impl From<sqlx::Error> for InsertionError {
    fn from(err: sqlx::Error) -> Self {
        Self::DbError(err)
    }
}

impl From<anyhow::Error> for InsertionError {
    fn from(err: anyhow::Error) -> Self {
        Self::AnyhowError(err)
    }
}
#[derive(sqlx::Type)]
#[sqlx(type_name = "MetaDataKind")]
#[sqlx(rename_all = "lowercase")]
pub enum DbMetaDataKind {
    Referrer,
}

impl DbMetaDataKind {
    pub fn from(kind: MetaDataKind) -> Self {
        match kind {
            MetaDataKind::Referrer => Self::Referrer,
        }
    }

    fn into(self) -> MetaDataKind {
        match self {
            Self::Referrer => MetaDataKind::Referrer,
        }
    }
}

#[async_trait::async_trait]
impl AppDataStoring for Postgres {
    async fn insert_app_data(&self, app_data: &AppData) -> Result<(), InsertionError> {
        const QUERY: &str = "\
            INSERT INTO app_data (\
                app_data_cid, version, app_code)\
                VALUES ($1, $2, $3);";
        let app_data_cid = app_data.cid()?;
        sqlx::query(QUERY)
            .bind(app_data_cid.clone().as_bytes())
            .bind(app_data.version.as_bytes())
            .bind(app_data.app_code.as_bytes())
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|err| {
                if let sqlx::Error::Database(db_err) = &err {
                    if let Some(Cow::Borrowed("23505")) = db_err.code() {
                        return InsertionError::DuplicatedRecord;
                    }
                }
                InsertionError::DbError(err)
            })?;
        for (i, meta_data) in app_data.meta_data.iter().enumerate() {
            const QUERY: &str = "\
            INSERT INTO meta_data ( \
                version, kind, referrer, position, app_data_cid)\
                VALUES ($1, $2, $3, $4, $5);";
            sqlx::query(QUERY)
                .bind(meta_data.version.as_bytes())
                .bind(DbMetaDataKind::from(meta_data.kind))
                .bind(meta_data.referrer.as_ref())
                .bind(i as u32)
                .bind(app_data_cid.as_bytes())
                .execute(&self.pool)
                .await
                .map(|_| ())
                .map_err(|err| {
                    if let sqlx::Error::Database(db_err) = &err {
                        if let Some(Cow::Borrowed("23505")) = db_err.code() {
                            return InsertionError::DuplicatedRecord;
                        }
                    }
                    InsertionError::DbError(err)
                })?;
        }
        Ok(())
    }

    async fn get_complete_app_data(&self, filter: &AppDataFilter) -> Result<Option<AppData>> {
        let meta_data = self.meta_data(filter).try_collect::<Vec<_>>().await?;
        let app_data_query_result = self.app_data(filter).try_collect::<Vec<_>>().await?;
        if let Some(app_data) = app_data_query_result.get(0) {
            Ok(Some(AppData {
                version: app_data.version.clone(),
                app_code: app_data.app_code.clone(),
                meta_data,
            }))
        } else {
            Ok(None)
        }
    }

    fn app_data<'a>(&'a self, filter: &'a AppDataFilter) -> BoxStream<'a, Result<AppData>> {
        if let Some(app_data_cid) = &filter.app_data_cid {
            const QUERY: &str = "\
            SELECT version, app_code FROM app_data \
                WHERE app_data_cid = $1;";

            sqlx::query_as(QUERY)
                .bind(app_data_cid.as_bytes())
                .fetch(&self.pool)
                .err_into()
                .and_then(|row: AppDataQueryRow| async move { row.into_app_data() })
                .boxed()
        } else {
            const QUERY: &str = "\
            SELECT version, app_code FROM app_data;";

            sqlx::query_as(QUERY)
                .fetch(&self.pool)
                .err_into()
                .and_then(|row: AppDataQueryRow| async move { row.into_app_data() })
                .boxed()
        }
    }
    fn meta_data<'a>(&'a self, filter: &'a AppDataFilter) -> BoxStream<'a, Result<MetaData>> {
        if let Some(app_data_cid) = &filter.app_data_cid {
            const QUERY: &str = "\
            SELECT version, kind, referrer FROM meta_data \
                WHERE \
                app_data_cid = $1 \
                ORDER BY position ASC;";

            sqlx::query_as(QUERY)
                .bind(app_data_cid.as_bytes())
                .fetch(&self.pool)
                .err_into()
                .and_then(|row: MetaDataQueryRow| async move { row.into_meta_data() })
                .boxed()
        } else {
            const QUERY: &str = "\
            SELECT version, kind, referrer FROM meta_data \
                ORDER BY app_data_cid, position ASC;";
            sqlx::query_as(QUERY)
                .fetch(&self.pool)
                .err_into()
                .and_then(|row: MetaDataQueryRow| async move { row.into_meta_data() })
                .boxed()
        }
    }
}

#[derive(sqlx::FromRow)]
struct AppDataQueryRow {
    version: Vec<u8>,
    app_code: Vec<u8>,
}

#[derive(sqlx::FromRow)]
struct MetaDataQueryRow {
    version: Vec<u8>,
    kind: DbMetaDataKind,
    referrer: Vec<u8>,
}

impl AppDataQueryRow {
    fn into_app_data(self) -> Result<AppData> {
        let version = String::from_utf8(self.version)?;
        let app_code = String::from_utf8(self.app_code)?;
        Ok(AppData {
            version,
            app_code,
            meta_data: Vec::new(),
        })
    }
}
impl MetaDataQueryRow {
    fn into_meta_data(self) -> Result<MetaData> {
        let version = String::from_utf8(self.version)?;
        let kind = self.kind.into();
        let referrer = h160_from_vec(self.referrer)?;
        Ok(MetaData {
            version,
            kind,
            referrer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    #[ignore]
    async fn postgres_insert_same_order_twice_fails() {
        let db = Postgres::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        let app_data = AppData {
            version: String::from("1.0.0"),
            app_code: String::from("CowSwap"),
            meta_data: vec![MetaData {
                version: String::from("1.2.3"),
                kind: MetaDataKind::Referrer,
                referrer: "0x424a46612794dbb8000194937834250dc723ffa5"
                    .parse()
                    .unwrap(),
            }],
        };
        db.insert_app_data(&app_data).await.unwrap();
        match db.insert_app_data(&app_data).await {
            Err(InsertionError::DuplicatedRecord) => (),
            _ => panic!("Expecting DuplicatedRecord error"),
        };
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_meta_data_roundtrip() {
        let db = Postgres::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        let filter = AppDataFilter::default();
        assert!(db.meta_data(&filter).boxed().next().await.is_none());
        assert!(db.app_data(&filter).boxed().next().await.is_none());
        let app_data = AppData {
            version: String::from("1.0.0"),
            app_code: String::from("CowSwap"),
            meta_data: vec![
                MetaData {
                    version: String::from("1.2.3"),
                    kind: MetaDataKind::Referrer,
                    referrer: "0x424a46612794dbb8000194937834250dc723ffa5"
                        .parse()
                        .unwrap(),
                },
                MetaData {
                    version: String::from("1.2.5"),
                    kind: MetaDataKind::Referrer,
                    referrer: "0x424a46612794dbb8000194937834250dc723ffa5"
                        .parse()
                        .unwrap(),
                },
            ],
        };
        db.insert_app_data(&app_data).await.unwrap();
        let new_filter = AppDataFilter {
            app_data_cid: Some(app_data.cid().unwrap()),
        };
        assert_eq!(
            db.get_complete_app_data(&new_filter)
                .await
                .unwrap()
                .unwrap(),
            app_data
        );
    }
}
