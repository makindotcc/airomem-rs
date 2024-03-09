# Airomem-rs

[Release at crates.io](https://crates.io/crates/airomem) \
(Toy) persistence library for Rust inspired by [prevayler for java](https://prevayler.org/) and named after its wrapper [airomem](https://github.com/airomem/airomem).

> It is an implementation of the Prevalent System design pattern, in which business objects are kept live in memory and transactions are journaled for system recovery.

## Assumptions

- All data lives in memory guarded with `tokio::sync::RwLock`, reads are fast and concurrent safe.
- By default every transaction is saved to append-only journal file and immediately [fsynced](https://man7.org/linux/man-pages/man2/fsync.2.html).
  By that, individual writes are slow, but they SHOULD survive crashes (e.g. power outage, software panic). \
  However, you can set periodic sync or manual. See `JournalFlushPolicy` for more info.
  Recommended for data that may be lost (e.g. cache, http session storage).
- I don't guarantee durability, it's created for toy projects or non-relevant data like http authorization tokens/cookies. https://www.postgresql.org/docs/9.4/wal-reliability.html

## Features

- [x] - saving executed transactions to append only file
- [x] - split journal log file if too big - while restoring data, all journal logs are loaded at once from disk to maximise throughput (and for simplicity reasons)
- [x] - snapshots for faster recovery
- [ ] - stable api

## Resources

- [Jaroslaw Ratajski - DROP DATABASE - galactic story](https://www.youtube.com/watch?v=m_uIROLGrN4)
- https://github.com/killertux/prevayler-rs

## Example

```rust
type UserId = usize;
type SessionsStore = JsonStore<Sessions, SessionsTx>;

#[derive(Serialize, Deserialize, Default)]
struct Sessions {
    tokens: HashMap<String, UserId>,
    operations: usize,
}

// ``SessionsTx`` - your desired name for root Tx name. It implements enum ``SessionsTx`` under the hood.
// ``Sessions`` - data struct name, used in closure
NestedTx!(SessionsTx<Sessions> {
    // ``-> ()`` is return type unit (aka no value)
    CreateSession (token: String, user_id: UserId) -> (): |data: &mut Sessions, tx: CreateSession| {
        data.operations += 1;
        data.tokens.insert(tx.token, tx.user_id);
    },
    // ``DeleteSession`` - your desired name for sub-tx struct implementation.
    // ``token: String, user_id: UserId`` - variables for ``DeleteSession`` struct
    // ``Option<UserId>`` - return type from closure, used to return data from store.commit(DeleteSession { ... })...
    DeleteSession (token: String) -> Option<UserId>: |data: &mut Sessions, tx: DeleteSession| {
        data.operations += 1;
        data.tokens.remove(&tx.token)
    },
});

#[tokio::test]
async fn test_mem_commit() {
    let dir = tempdir().unwrap();
    let mut store: SessionsStore =
        Store::open(JsonSerializer, StoreOptions::default(), dir.into_path())
            .await
            .unwrap();
    let example_token = "access_token".to_string();
    let example_uid = 1;
    store
        .commit(CreateSession {
            token: example_token.clone(),
            user_id: example_uid,
        })
        .await
        .unwrap();

    let mut expected_tokens = HashMap::new();
    expected_tokens.insert(example_token.clone(), example_uid);
    assert_eq!(store.query().await.tokens, expected_tokens);

    let deleted_uid = store
        .commit(DeleteSession {
            token: example_token,
        })
        .await
        .unwrap();
    assert_eq!(deleted_uid, Some(example_uid));
}

// No macro ``NestedTx!`` equivalent:
mod no_nested_tx_macro {
    #[derive(serde::Serialize, serde::Deserialize)]
    pub enum SessionsTx {
        CreateSession(CreateSession),
        DeleteSession(DeleteSession),
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    pub struct CreateSession {
        token: String,
        user_id: UserId,
    }

    impl Into<SessionsTx> for CreateSession {
        fn into(self) -> SessionsTx {
            SessionsTx::CreateSession(self)
        }
    }

    impl From<SessionsTx> for CreateSession {
        fn from(value: SessionsTx) -> Self {
            match value {
                SessionsTx::CreateSession(inner) => inner,
                _ => unreachable!(),
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    pub struct DeleteSession {
        token: String,
    }

    // macro implementing `Into` and `From` like we have done it above manually for CreateSession
    airomem::EnumBorrowOwned!(SessionsTx, DeleteSession);

    impl Tx<Sessions, ()> for SessionsTx {
        fn execute(self, data: &mut Sessions) -> () {
            match self {
                SessionsTx::CreateSession(it) => {
                    it.execute(data);
                }
                SessionsTx::DeleteSession(it) => {
                    it.execute(data);
                }
            }
        }
    }

    impl Tx<Sessions, usize> for DeleteSession {
        fn execute(self, data: &mut Sessions) -> usize {
            data.operations += 1;
            data.tokens.remove(&self.token)
        }
    }

    // impl Tx for CreateSession (...)
}
```
