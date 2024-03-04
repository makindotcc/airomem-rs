# Airomem-rs

(Toy) persistence library for Rust inspired by [prevayler for java](https://prevayler.org/) and named after its wrapper [airomem](https://github.com/airomem/airomem).

> It is an implementation of the Prevalent System design pattern, in which business objects are kept live in memory and transactions are journaled for system recovery.

## Assumptions

- All data lives in memory guarded with `std::sync::RwLock`, reads are fast and concurrent safe.
- Every command is saved to append-only journal file and immediately [fsynced](https://man7.org/linux/man-pages/man2/fsync.2.html).
  By that, individual writes are slower, but they SHOULD survive crashes (e.g. power outage, software panic).
- I don't guarantee durability, it's created for toy projects or non-relevant data like http authorization tokens/cookies. https://www.postgresql.org/docs/9.4/wal-reliability.html

## Features

- [x] - saving executed commands to append only file
- [ ] - split journal log file if too big - while restoring data, all journal logs are loaded at once from disk to maximise throughput (and for simplicity reasons)
- [ ] - snapshots for faster recovery

## Resources

- [Jaroslaw Ratajski - DROP DATABASE - galactic story](https://www.youtube.com/watch?v=m_uIROLGrN4)
- https://github.com/killertux/prevayler-rs

## Example

```rs
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
    let store: SessionsStore = Store::open(JsonSerializer, &dir).await.unwrap();
    store
        .commit(SessionsCommand::CreateSession {
            token: "access_token".to_string(),
            user_id: 1,
        })
        .await
        .unwrap();

    let mut expected_tokens = HashMap::new();
    expected_tokens.insert("access_token".to_string(), 1);
    assert_eq!(store.query().unwrap().tokens, expected_tokens);
}
```
