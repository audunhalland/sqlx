use sqlx::connection::{Connect, Connection};
use sqlx::describe::Describe;
use sqlx::executor::{Executor, RefExecutor};
use url::Url;

use std::fmt::{self, Display, Formatter};

use crate::database::DatabaseExt;
use std::fs::File;
use syn::export::Span;

#[cfg_attr(feature = "offline", derive(serde::Deserialize, serde::Serialize))]
pub struct QueryData {
    pub(super) query: String,
    pub(super) input_types: Vec<Option<String>>,
    pub(super) outputs: Vec<(String, String)>,
}

impl QueryData {
    pub fn from_db(db_url: &str, query: &str) -> crate::Result<Self> {
        crate::runtime::block_on(async {
            let db_url = db_url.parse::<Url>()?;

            match db_url.scheme() {
                #[cfg(feature = "sqlite")]
                "sqlite" => {
                    let mut conn = sqlx::sqlite::SqliteConnection::connect(db_url.as_str())
                        .await
                        .map_err(|e| format!("failed to connect to database: {}", e))?;

                    describe_query(conn, query).await
                }
                #[cfg(not(feature = "sqlite"))]
                "sqlite" => Err(format!(
                    "database URL {} has the scheme of a SQLite database but the `sqlite` \
                     feature of sqlx was not enabled",
                    db_url
                )
                .into()),
                #[cfg(feature = "postgres")]
                "postgresql" | "postgres" => {
                    let mut conn = sqlx::postgres::PgConnection::connect(db_url.as_str())
                        .await
                        .map_err(|e| format!("failed to connect to database: {}", e))?;

                    describe_query(conn, query).await
                }
                #[cfg(not(feature = "postgres"))]
                "postgresql" | "postgres" => Err(format!(
                    "database URL {} has the scheme of a Postgres database but the `postgres` \
                     feature of sqlx was not enabled",
                    db_url
                )
                .into()),
                #[cfg(feature = "mysql")]
                "mysql" | "mariadb" => {
                    let mut conn = sqlx::mysql::MySqlConnection::connect(db_url.as_str())
                        .await
                        .map_err(|e| format!("failed to connect to database: {}", e))?;

                    describe_query(conn, query).await
                }
                #[cfg(not(feature = "mysql"))]
                "mysql" | "mariadb" => Err(format!(
                    "database URL {} has the scheme of a MySQL/MariaDB database but the `mysql` \
                     feature of sqlx was not enabled",
                    db_url
                )
                .into()),
                scheme => {
                    Err(format!("unexpected scheme {:?} in database URL {}", scheme, db_url).into())
                }
            }
        })
    }
}

async fn describe_query<C: Connection>(mut conn: C, query: &str) -> crate::Result<QueryData>
where
    <C as Executor>::Database: DatabaseExt,
{
    let describe: Describe<<C as Executor>::Database> = conn.describe(query).await?;

    let input_types = describe
        .param_types
        .iter()
        .map(|param_ty| {
            <<C as Executor>::Database as DatabaseExt>::param_type_for_id(param_ty.as_ref()?)
                .map(Into::into)
        })
        .collect();

    let outputs = describe
        .result_columns
        .iter()
        .enumerate()
        .map(|(i, column)| -> crate::Result<_> {
            let name = column
                .name
                .cloned()
                .ok_or_else(|| format!("column at position {} must have a name", i))?;

            let type_info = column.type_info.ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    format!(
                        "database couldn't tell us the type of {col}; \
                     this can happen for columns that are the result of an expression",
                        col = DisplayColumn {
                            idx: i,
                            name: column.name.as_deref()
                        }
                    ),
                )
            })?;

            let type_ = <<C as Executor>::Database as DatabaseExt>::return_type_for_id(&type_info)
                .ok_or_else(|| {
                    let message = if let Some(feature_gate) =
                        <<C as Executor>::Database as DatabaseExt>::get_feature_gate(&type_info)
                    {
                        format!(
                            "optional feature `{feat}` required for type {ty} of {col}",
                            ty = &type_info,
                            feat = feature_gate,
                            col = DisplayColumn {
                                idx: i,
                                name: column.name.as_deref()
                            }
                        )
                    } else {
                        format!(
                            "unsupported type {ty} of {col}",
                            ty = type_info,
                            col = DisplayColumn {
                                idx: i,
                                name: column.name.as_deref()
                            }
                        )
                    };
                    syn::Error::new(Span::call_site(), message)
                })?;

            let type_ = if column.non_null.unwrap_or(false) {
                format!("Option<{}>", type_)
            } else {
                type_.into()
            };

            Ok((name, type_))
        })
        .collect::<crate::Result<Vec<_>>>()?;

    Ok(QueryData {
        query: query.into(),
        input_types,
        outputs,
    })
}

struct DisplayColumn<'a> {
    // zero-based index, converted to 1-based number
    idx: usize,
    name: Option<&'a str>,
}

impl Display for DisplayColumn<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let num = self.idx + 1;

        if let Some(name) = self.name {
            write!(f, "column #{} ({:?})", num, name)
        } else {
            write!(f, "column #{}", num)
        }
    }
}

#[cfg(feature = "offline")]
mod offline {
    use super::QueryData;
    use std::fs::File;

    use std::fmt::{self, Formatter};

    use serde::de::{Deserializer, MapAccess, Visitor};
    use sqlx::query::query;
    use std::path::Path;

    impl QueryData {
        /// Find and deserialize the data table for this query from a shared `sqlx-data.json`
        /// file. The expected structure is a JSON map keyed by the SHA-256 hash of queries in hex.
        pub fn from_data_file(path: &str, query: &str) -> crate::Result<QueryData> {
            serde_json::Deserializer::from_reader(
                File::open(path)
                    .map_err(|e| format!("failed to open path {:?}: {}", path, e).into())?,
            )
            .deserialize_map(DataFileVisitor {
                query,
                hash: hash_query(query),
            })
        }

        pub fn save_in(&self, dir: &str) -> crate::Result<()> {
            let path = Path::from(dir).join(format!("query-{}.json", hash_query(&self.query)));

            serde_json::to_writer(
                File::create(&path)
                    .map_err(|e| format!("failed to open path {:?}: {}", path, e).into())?,
                self,
            )
            .map_err(Into::into)
        }
    }

    fn hash_query(query: &str) -> String {
        // picked `sha2` because it's already in the dependency tree for both MySQL and Postgres
        use sha2::{Digest, Sha256};

        hex::encode(Sha256::digest(query.as_bytes()))
    }

    // lazily deserializes only the `QueryData` for the query we're looking for
    struct DataFileVisitor<'a> {
        query: &'a str,
        hash: String,
    }

    impl<'de> Visitor<'de> for DataFileVisitor {
        type Value = QueryData;

        fn expecting(&self, f: &mut Formatter) -> fmt::Result {
            write!(f, "expected map key {:?}", self.hash)
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, <A as MapAccess<'de>>::Error>
        where
            A: MapAccess<'de>,
        {
            // unfortunately we can't avoid this copy because deserializing from `io::Read`
            // doesn't support deserializing borrowed values
            while let Some(key) = map.next_key::<String>() {
                // lazily deserialize the query data only
                if key == self.hash {
                    let query_data: QueryData = map.next_value()?;

                    return if query_data.query == self.query {
                        Ok(query_data)
                    } else {
                        Err(serde::de::Error::custom(format_args!(
                            "hash collision for stored queries:\n{:?}\n{:?}",
                            self.query, query_data.query
                        )))
                    };
                }
            }

            Err(serde::de::Error::custom(format_args!(
                "hash collision for stored queries:\n{:?}\n{:?}",
                self.query, query_data.query
            )))
        }
    }
}
