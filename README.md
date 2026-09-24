# xmip-core-archive-mysql

MySQL and MariaDB archive target: one item is one row of an archive table. A technology of [xmip-core-archive](https://github.com/IlleNilsson/xmip-core-archive).

The store is the capability's `archive::sql::SqlArchive`; this crate is its
dialect, `MySql` — how the server quotes, how the bytes go in and come back,
how a new row's id is asked for — and the connection through
[xmip-core-transport-mysql](https://github.com/IlleNilsson/xmip-core-transport-mysql).
An archive here is `SqlArchive::<MySql>::new(server, database, user)`.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
