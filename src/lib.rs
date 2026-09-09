#![forbid(unsafe_code)]

//! `MySQL` and `MariaDB` archive: an [`ArchiveStore`] that keeps each
//! retained item as one row of an archive table, and restores it by
//! selecting the row back.
//!
//! A xmip-core-archive **technology** (repository-model.md): it depends on
//! the archive capability for the [`ArchiveStore`] trait and its item,
//! receipt and error types, and on the `MySQL` transport technology for
//! the connection — the text protocol with the native password. One item
//! is one row with the four columns every archive technology carries —
//! `data_type`, `identifier`, `bytes`, `metadata` — and `archived_at`, when
//! it was handed over, in UTC. The table is the operator's to create; this
//! is the shape it is written for:
//!
//! ```sql
//! CREATE TABLE archive (
//!     id          BIGINT AUTO_INCREMENT PRIMARY KEY,
//!     data_type   TEXT NOT NULL,
//!     identifier  TEXT NOT NULL,
//!     bytes       LONGBLOB NOT NULL,
//!     metadata    TEXT NOT NULL,
//!     archived_at DATETIME NOT NULL
//! );
//! ```
//!
//! An archive never deletes (ADR-0040): this one inserts and selects, nothing
//! else. The receipt is `mysql://<server>/<database>/<table>?id=<n>`, the
//! id read with `SELECT LAST_INSERT_ID()` on the connection that inserted,
//! and restoring reads the table and the id from the receipt on the store's
//! own connection.

pub mod row;
pub mod timestamp;

use std::time::{Duration, SystemTime};

use archive::{ArchiveError, ArchiveItem, ArchiveReceipt, ArchiveStore};
use mysql::{Client, Login};

/// The table written to unless told otherwise.
pub const DEFAULT_TABLE: &str = "archive";

/// An archive that keeps items as rows of one table on one server.
pub struct MysqlArchive {
    server: String,
    database: String,
    user: String,
    password: Option<String>,
    table: String,
    timeout: Option<Duration>,
}

impl MysqlArchive {
    /// An archive writing to [`DEFAULT_TABLE`] in `database` at `server`,
    /// logging in as `user` with an empty password.
    #[must_use]
    pub fn new(
        server: impl Into<String>,
        database: impl Into<String>,
        user: impl Into<String>,
    ) -> Self {
        Self {
            server: server.into(),
            database: database.into(),
            user: user.into(),
            password: None,
            table: DEFAULT_TABLE.to_string(),
            timeout: None,
        }
    }

    /// The password the login scrambles.
    #[must_use]
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// The table to write to, `audit.archive` say.
    #[must_use]
    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table = table.into();
        self
    }

    /// Give up on a server that stops mid-packet.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn connect(&self) -> Result<Client, ArchiveError> {
        let login = Login::new(&*self.user, self.password.clone().unwrap_or_default());
        Client::connect(&self.server, &self.database, &login, self.timeout).map_err(error)
    }

    fn location(&self, id: &str) -> String {
        format!(
            "mysql://{}/{}/{}?id={id}",
            self.server, self.database, self.table
        )
    }
}

impl ArchiveStore for MysqlArchive {
    fn archive(&self, item: ArchiveItem) -> Result<ArchiveReceipt, ArchiveError> {
        let archived_at = timestamp::datetime_utc(SystemTime::now());
        let sql = row::insert_sql(&self.table, &item, &archived_at);
        let mut client = self.connect()?;
        client.execute(&sql).map_err(error)?;
        let result = client.query(row::LAST_INSERT_ID).map_err(error)?;
        client.close().map_err(error)?;
        let id = result
            .rows
            .first()
            .and_then(|first| first.first())
            .cloned()
            .flatten()
            .ok_or_else(|| ArchiveError {
                message: format!("the insert into {} returned no id", self.table),
            })?;
        Ok(ArchiveReceipt {
            location: self.location(&id),
            checksum: None,
        })
    }

    fn restore(&self, receipt: &ArchiveReceipt) -> Result<ArchiveItem, ArchiveError> {
        let (table, id) = parse_location(&receipt.location)?;
        let mut client = self.connect()?;
        let result = client.query(&row::select_sql(table, id)).map_err(error)?;
        client.close().map_err(error)?;
        let first = result.rows.first().ok_or_else(|| ArchiveError {
            message: format!("no row at {}", receipt.location),
        })?;
        row::item_from_row(first, &receipt.location)
    }
}

/// The table and id a receipt names:
/// `mysql://<server>/<database>/<table>?id=<n>`.
fn parse_location(location: &str) -> Result<(&str, u64), ArchiveError> {
    let malformed = || ArchiveError {
        message: format!("{location} is not mysql://server/database/table?id=n"),
    };
    let rest = location.strip_prefix("mysql://").ok_or_else(malformed)?;
    let (path, query) = rest.split_once('?').ok_or_else(malformed)?;
    let id = query
        .strip_prefix("id=")
        .and_then(|digits| digits.parse().ok())
        .ok_or_else(malformed)?;
    match path.splitn(3, '/').collect::<Vec<_>>().as_slice() {
        [_, _, table] if !table.is_empty() => Ok((table, id)),
        _ => Err(malformed()),
    }
}

fn error(cause: impl std::fmt::Display) -> ArchiveError {
    ArchiveError {
        message: cause.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mysql::hex;
    use mysql::{Answer, Event, Session};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn item(id: &str) -> ArchiveItem {
        ArchiveItem {
            data_type: "json".to_string(),
            identifier: id.to_string(),
            bytes: b"{\"kept\":true}".to_vec(),
            metadata: vec![("source".to_string(), "playground".to_string())],
        }
    }

    /// A far end that serves `connections` clients in turn, demanding
    /// `password` for user `xmip`: any INSERT is completed, the id question
    /// is answered with 41, any other SELECT with the canned row for `held`,
    /// and every statement is reported back.
    fn far_end(
        password: &'static str,
        held: &ArchiveItem,
        connections: usize,
    ) -> (String, JoinHandle<Vec<Event>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address").to_string();
        let expected = Login::new("xmip", password);
        let row = [
            Some(held.data_type.clone()),
            Some(held.identifier.clone()),
            Some(hex::hex_literal(&held.bytes)),
            Some(row::encode_metadata(&held.metadata)),
        ];
        let handle = std::thread::spawn(move || {
            let mut events = Vec::new();
            for _ in 0..connections {
                let Ok(session) = Session::accept(&listener, &expected, Some(secs(2))) else {
                    continue;
                };
                let canned = [
                    row[0].as_deref(),
                    row[1].as_deref(),
                    row[2].as_deref(),
                    row[3].as_deref(),
                ];
                let rows: [&[Option<&str>]; 1] = [&canned];
                let mut session = session
                    .with_table(&["data_type", "identifier", "bytes", "metadata"], &rows)
                    .answering(|sql| {
                        if sql.starts_with("INSERT") {
                            Some(Answer::Complete(1))
                        } else if sql == row::LAST_INSERT_ID {
                            Some(Answer::Rows {
                                columns: vec!["LAST_INSERT_ID()".to_string()],
                                rows: vec![vec![Some("41".to_string())]],
                            })
                        } else {
                            None
                        }
                    });
                while let Some(event) = session.next_event().expect("event") {
                    events.push(event);
                }
            }
            events
        });
        (address, handle)
    }

    #[test]
    fn an_archived_item_is_one_insert_and_its_receipt_names_the_row() {
        let original = item("json#1");
        let (address, far_end) = far_end("secret", &original, 1);
        let store = MysqlArchive::new(address.clone(), "orders", "xmip")
            .with_password("secret")
            .with_table("audit.archive")
            .timing_out_after(secs(2));
        let receipt = store.archive(original.clone()).expect("archive");
        assert_eq!(
            receipt.location,
            format!("mysql://{address}/orders/audit.archive?id=41")
        );
        assert_eq!(receipt.checksum, None);
        let events = far_end.join().expect("thread");
        assert_eq!(events.len(), 2, "the insert, the id, then the client quit");
        let Event::Executed(sql) = &events[0] else {
            panic!("an INSERT is answered by the closure: {:?}", events[0]);
        };
        assert!(sql.starts_with("INSERT INTO `audit`.`archive` (data_type, identifier, "));
        assert!(sql.contains("VALUES ('json', 'json#1', X'7b226b657074223a747275657d', "));
        assert!(sql.contains("'source\u{1f}playground', '20"));
        let stamp = &sql[sql.len() - 22..];
        assert!(stamp.starts_with("'20") && stamp.ends_with("')"), "{sql}");
        assert_eq!(&stamp[11..12], " ", "a DATETIME literal, not RFC 3339");
        assert_eq!(events[1], Event::Executed(row::LAST_INSERT_ID.to_string()));
    }

    #[test]
    fn the_row_restores_the_item_over_a_second_connection() {
        let original = item("json#2");
        let (address, far_end) = far_end("", &original, 2);
        let store = MysqlArchive::new(address, "orders", "xmip").timing_out_after(secs(2));
        let receipt = store.archive(original.clone()).expect("archive");
        let restored = store.restore(&receipt).expect("restore");
        assert_eq!(restored, original, "the row read back is the item");
        let events = far_end.join().expect("thread");
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[2],
            Event::Selected(
                "SELECT data_type, identifier, CONCAT('0x', HEX(bytes)), metadata \
                 FROM `archive` WHERE id = 41"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_wrong_password_and_a_wrong_receipt_are_refused() {
        let (address, far_end) = far_end("secret", &item("json#3"), 1);
        let store = MysqlArchive::new(address, "orders", "xmip")
            .with_password("wrong")
            .timing_out_after(secs(2));
        let refused = store.archive(item("json#3")).expect_err("wrong password");
        assert!(refused.message.contains("1045"), "{refused}");
        far_end.join().expect("thread");
        for location in [
            "s3://bucket/key",
            "mysql://host/orders/archive",
            "mysql://host/orders/archive?id=x",
            "mysql://host/orders?id=1",
        ] {
            let receipt = ArchiveReceipt {
                location: location.to_string(),
                checksum: None,
            };
            let failure = store.restore(&receipt).expect_err(location);
            assert!(failure.message.contains("is not mysql://"), "{failure}");
        }
        assert_eq!(
            parse_location("mysql://h:3306/db/audit.archive?id=7").expect("parsed"),
            ("audit.archive", 7)
        );
    }
}
