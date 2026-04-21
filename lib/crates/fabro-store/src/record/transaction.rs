use slatedb::{Db, WriteBatch};

use super::repository::key_for_id;
use super::{Codec, Record};
use crate::Result;

pub(crate) async fn transaction<T, F>(db: &Db, f: F) -> Result<T>
where
    F: FnOnce(&mut Tx) -> Result<T>,
{
    let mut tx = Tx::new();
    let value = f(&mut tx)?;
    if tx.has_ops {
        db.write(tx.batch).await?;
    }
    Ok(value)
}

pub(crate) struct Tx {
    batch:   WriteBatch,
    /// SlateDB rejects empty `WriteBatch` commits; skip the write entirely
    /// when the closure produced no operations.
    has_ops: bool,
}

impl Tx {
    fn new() -> Self {
        Self {
            batch:   WriteBatch::new(),
            has_ops: false,
        }
    }

    pub(crate) fn put<R: Record>(&mut self, record: &R) -> Result<&mut Self> {
        let id = record.id();
        self.put_at(&id, record)
    }

    pub(crate) fn put_at<R: Record>(&mut self, id: &R::Id, record: &R) -> Result<&mut Self> {
        self.batch
            .put(key_for_id::<R>(id)?, R::Codec::encode(record)?);
        self.has_ops = true;
        Ok(self)
    }

    #[allow(
        dead_code,
        reason = "Shared transaction surface; current production callers only use put paths"
    )]
    pub(crate) fn delete<R: Record>(&mut self, id: &R::Id) -> Result<&mut Self> {
        self.batch.delete(key_for_id::<R>(id)?);
        self.has_ops = true;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use object_store::memory::InMemory;
    use serde::{Deserialize, Serialize};

    use super::{Record, Tx, transaction};
    use crate::record::{Codec, JsonCodec, Repository};
    use crate::{Error, Result};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TxRecord {
        id:       String,
        payload:  String,
        poisoned: bool,
    }

    impl Record for TxRecord {
        type Id = String;
        type Codec = JsonCodec;

        const PREFIX: &'static str = "test/transaction";

        fn id(&self) -> Self::Id {
            self.id.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct FailingRecord {
        id:       String,
        poisoned: bool,
    }

    struct FailingCodec;

    impl Codec<FailingRecord> for FailingCodec {
        fn encode(value: &FailingRecord) -> Result<Vec<u8>> {
            if value.poisoned {
                return Err(Error::Other(
                    "poisoned record refused to encode".to_string(),
                ));
            }
            serde_json::to_vec(value).map_err(Into::into)
        }

        fn decode(bytes: &[u8]) -> Result<FailingRecord> {
            serde_json::from_slice(bytes).map_err(Into::into)
        }
    }

    impl Record for FailingRecord {
        type Id = String;
        type Codec = FailingCodec;

        const PREFIX: &'static str = "test/failing-transaction";

        fn id(&self) -> Self::Id {
            self.id.clone()
        }
    }

    async fn db() -> Arc<slatedb::Db> {
        Arc::new(
            slatedb::Db::open("transaction-tests", Arc::new(InMemory::new()))
                .await
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn closure_error_short_circuits_without_writing() {
        let db = db().await;
        let repo = Repository::<TxRecord>::new(Arc::clone(&db));
        let record = TxRecord {
            id:       "record-1".to_string(),
            payload:  "hello".to_string(),
            poisoned: false,
        };

        let error = transaction::<(), _>(&db, |tx| {
            tx.put(&record)?;
            Err(Error::Other("stop before commit".to_string()))
        })
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "stop before commit");
        assert!(repo.get(&record.id()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn encode_failure_discards_the_entire_batch() {
        let db = db().await;
        let repo = Repository::<FailingRecord>::new(Arc::clone(&db));
        let good = FailingRecord {
            id:       "good".to_string(),
            poisoned: false,
        };
        let bad = FailingRecord {
            id:       "bad".to_string(),
            poisoned: true,
        };

        let error = transaction::<(), _>(&db, |tx| {
            tx.put(&good)?;
            tx.put(&bad)?;
            Ok(())
        })
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "poisoned record refused to encode");
        assert!(repo.get(&good.id()).await.unwrap().is_none());
        assert!(repo.get(&bad.id()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn empty_transaction_returns_without_writing() {
        let db = db().await;
        let repo = Repository::<TxRecord>::new(Arc::clone(&db));

        let value = transaction(&db, |_tx: &mut Tx| Ok::<_, Error>("ok"))
            .await
            .unwrap();

        assert_eq!(value, "ok");
        assert!(repo.get(&"missing".to_string()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_operations_are_committed() {
        let db = db().await;
        let repo = Repository::<TxRecord>::new(Arc::clone(&db));
        let record = TxRecord {
            id:       "delete-me".to_string(),
            payload:  "hello".to_string(),
            poisoned: false,
        };
        repo.put(&record).await.unwrap();

        transaction::<(), _>(&db, |tx| {
            tx.delete::<TxRecord>(&record.id())?;
            Ok(())
        })
        .await
        .unwrap();

        assert!(repo.get(&record.id()).await.unwrap().is_none());
    }
}
