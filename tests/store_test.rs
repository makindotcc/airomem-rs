use airomem::{JournalFlushPolicy, JsonSerializer, JsonStore, MergeTx, Store, StoreOptions, Tx};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, num::NonZeroUsize};
use tempfile::tempdir;

type UserId = usize;
type SessionsStore = JsonStore<Sessions, SessionsTx>;

#[derive(Serialize, Deserialize, Default)]
pub struct Sessions {
    tokens: HashMap<String, UserId>,
    operations: usize,
}

MergeTx!(pub SessionsTx<Sessions> = CreateSession | DeleteSession);

#[derive(Serialize, Deserialize)]
pub struct CreateSession {
    token: String,
    user_id: UserId,
}

impl Tx<Sessions> for CreateSession {
    fn execute(self, data: &mut Sessions) {
        data.operations += 1;
        data.tokens.insert(self.token, self.user_id);
    }
}

#[derive(Serialize, Deserialize)]
pub struct DeleteSession {
    token: String,
}

impl Tx<Sessions, Option<UserId>> for DeleteSession {
    fn execute(self, data: &mut Sessions) -> Option<UserId> {
        data.operations += 1;
        data.tokens.remove(&self.token)
    }
}

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
    assert_eq!(store.query().await.unwrap().tokens, expected_tokens);

    let deleted_uid = store
        .commit(DeleteSession {
            token: example_token,
        })
        .await
        .unwrap();
    assert_eq!(deleted_uid, Some(example_uid));
}

#[tokio::test]
async fn test_manual_flush() {
    let dir = tempdir().unwrap();
    let do_store_commit = |options: StoreOptions| {
        let dir = dir.path();
        async move {
            let mut store: SessionsStore = Store::open(JsonSerializer, options, dir).await.unwrap();
            store
                .commit(CreateSession {
                    token: "access_token".to_string(),
                    user_id: 1,
                })
                .await
                .unwrap();
        }
    };

    {
        do_store_commit(
            StoreOptions::default()
                .journal_flush_policy(JournalFlushPolicy::Manually)
                .flush_synchronously_on_drop(false),
        )
        .await;
        let store: SessionsStore = Store::open(JsonSerializer, StoreOptions::default(), dir.path())
            .await
            .unwrap();
        assert_eq!(
            store.query().await.unwrap().tokens.len(),
            0,
            "flushed journal log on drop, but it shouldn't"
        );
    }

    {
        do_store_commit(
            StoreOptions::default()
                .journal_flush_policy(JournalFlushPolicy::Manually)
                .flush_synchronously_on_drop(true),
        )
        .await;
        let store: SessionsStore = Store::open(JsonSerializer, StoreOptions::default(), dir.path())
            .await
            .unwrap();
        assert_eq!(
            store.query().await.unwrap().tokens.len(),
            1,
            "should flush journal log on drop"
        );
    }
}

#[tokio::test]
async fn test_journal_rebuild() {
    let dir = tempdir().unwrap();
    let options = StoreOptions::default().max_journal_entries(NonZeroUsize::new(10).unwrap());
    for i in 0..2 {
        let mut store: SessionsStore = Store::open(JsonSerializer, options.clone(), dir.path())
            .await
            .unwrap();
        store
            .commit(CreateSession {
                token: format!("token{i}"),
                user_id: i,
            })
            .await
            .unwrap();
    }
    let store: SessionsStore = Store::open(JsonSerializer, options, dir.into_path())
        .await
        .unwrap();
    let expected_tokens = {
        let mut it = HashMap::new();
        it.insert("token0".to_string(), 0);
        it.insert("token1".to_string(), 1);
        it
    };
    assert_eq!(store.query().await.unwrap().tokens, expected_tokens);
}

#[tokio::test]
async fn test_rebuild_with_snapshot() {
    let options = StoreOptions::default().max_journal_entries(NonZeroUsize::new(2).unwrap());
    for commits in 0..=5 {
        let dir = tempdir().unwrap();
        {
            let mut store: SessionsStore = Store::open(JsonSerializer, options.clone(), dir.path())
                .await
                .unwrap();
            for i in 0..commits {
                store
                    .commit(CreateSession {
                        token: format!("token{i}"),
                        user_id: i,
                    })
                    .await
                    .unwrap();
            }
        }
        let store: SessionsStore = Store::open(JsonSerializer, options.clone(), dir.path())
            .await
            .unwrap();
        let expected_tokens = {
            let mut it = HashMap::new();
            for i in 0..commits {
                it.insert(format!("token{i}"), i);
            }
            it
        };
        let state = store.query().await;
        assert_eq!(state.as_ref().unwrap().tokens, expected_tokens);
        assert_eq!(state.unwrap().operations, commits);
    }
}
