# Airomem-rs

Persistence library for Rust inspired by [prevayler for java](https://prevayler.org/) and named after its wrapper [airomem](https://github.com/airomem/airomem).

> It is an implementation of the Prevalent System design pattern, in which business objects are kept live in memory and transactions are journaled for system recovery.

## Assumptions
- All data is living in memory guarded with ``std::sync::RwLock``, so reads are fast and concurrent safe.
- Every command is saved to append-only journal file and immediately [fsynced](https://man7.org/linux/man-pages/man2/fsync.2.html).
By that, individual writes are slower, but they SHOULD survive crashes (e.g. power outage, software panic)

## Features

- [x] - saving executed commands to append only file
- [ ] - snapshots for faster recovery

## Resources

- [Jaroslaw Ratajski - DROP DATABASE - galactic story](https://www.youtube.com/watch?v=m_uIROLGrN4)
- https://github.com/killertux/prevayler-rs
