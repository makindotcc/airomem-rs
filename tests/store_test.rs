use airomem::{JsonSerializer, JsonStore, Store, StoreOptions};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tempfile::tempdir;

type SessionsStore = JsonStore<Sessions>;

#[derive(Serialize, Deserialize, Default, Debug)]
struct Sessions {
    tokens: HashMap<String, usize>,
}

impl airomem::State for Sessions {
    type Command = SessionsCommand;

    fn execute(&mut self, command: SessionsCommand) {
        match command {
            SessionsCommand::CreateSession { token, user_id } => {
                self.tokens.insert(token, user_id);
            }
            SessionsCommand::DeleteSession { token } => {
                self.tokens.remove(&token);
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
enum SessionsCommand {
    CreateSession { token: String, user_id: usize },
    DeleteSession { token: String },
}

#[tokio::test]
async fn test_mem_commit() {
    let dir = tempdir().unwrap();
    let store: SessionsStore =
        Store::open(JsonSerializer, StoreOptions::default(), dir.into_path())
            .await
            .unwrap();
    store
        .commit(SessionsCommand::CreateSession {
            token: "access_token".to_string(),
            user_id: 1,
        })
        .await
        .unwrap();

    let mut expected_tokens = HashMap::new();
    expected_tokens.insert("access_token".to_string(), 1);
    assert_eq!(store.query().await.unwrap().tokens, expected_tokens);
}

#[tokio::test]
async fn test_journal_rebuild() {
    let dir = tempdir().unwrap();
    for i in 0..2 {
        let store: SessionsStore = Store::open(JsonSerializer, StoreOptions::default(), dir.path())
            .await
            .unwrap();
        store
            .commit(SessionsCommand::CreateSession {
                token: format!("token{i}"),
                user_id: i,
            })
            .await
            .unwrap();
    }
    let store: SessionsStore =
        Store::open(JsonSerializer, StoreOptions::default(), dir.into_path())
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
