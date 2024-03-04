# Airomem-rs

(Toy) persistence library for Rust inspired by [prevayler for java](https://prevayler.org/) and named after its wrapper [airomem](https://github.com/airomem/airomem).

> It is an implementation of the Prevalent System design pattern, in which business objects are kept live in memory and transactions are journaled for system recovery.

## Assumptions
- All data lives in memory guarded with ``std::sync::RwLock``, reads are fast and concurrent safe.
- Every command is saved to append-only journal file and immediately [fsynced](https://man7.org/linux/man-pages/man2/fsync.2.html).
By that, individual writes are slower, but they SHOULD survive crashes (e.g. power outage, software panic).
- I don't guarantee durability, it's created for toy projects or non-relevant data like http authorization tokens/cookies.

## Features

- [x] - saving executed commands to append only file
- [ ] - split journal log file if too big - while restoring data, all journal logs are loaded at once from disk to maximise throughput (and for simplicity reasons)
- [ ] - snapshots for faster recovery

## Resources

- [Jaroslaw Ratajski - DROP DATABASE - galactic story](https://www.youtube.com/watch?v=m_uIROLGrN4)
- https://github.com/killertux/prevayler-rs
