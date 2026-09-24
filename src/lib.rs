#![forbid(unsafe_code)]

//! `MySQL` and `MariaDB` archive: the [`Dialect`] the archive capability's
//! `SqlArchive` keeps each retained item in an archive table with, as
//! `SqlArchive::<MySql>`.
//!
//! The store, the row, the SELECT and the receipt are the capability's
//! (`archive::sql`, ADR-0044); only the dialect is this crate's — backtick
//! identifiers, literals with the backslash escapes the server reads, the
//! bytes going in as the `X'…'` hexadecimal literal a BLOB stores as the
//! bytes and coming back spelled out — `CONCAT('0x', HEX(bytes))` —
//! because the text protocol hands a BLOB over raw and the row reader
//! would take it for text, `archived_at` as a `DATETIME` literal in UTC,
//! and the new row's id asked for afterwards with [`LAST_INSERT_ID`] on
//! the connection that inserted. The connection is the `MySQL` transport
//! technology's — the text protocol with the native password. The table is
//! the operator's to create; this is the shape it is written for:
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
//! The receipt is `mysql://<server>/<database>/<table>?id=<n>`.

use std::time::SystemTime;

use archive::ArchiveError;
use archive::sql::{Dialect, Row, Server, first_cell};
use mysql::{Client, Login, hex};

/// The statement that asks for the id the last insert on this connection
/// made, since the protocol's answer to the INSERT carries it and the
/// transport's [`mysql::QueryResult`] does not.
pub const LAST_INSERT_ID: &str = "SELECT LAST_INSERT_ID()";

/// What `MySQL` does its own way.
pub struct MySql;

impl Dialect for MySql {
    const SCHEME: &'static str = "mysql";
    const BYTES_EXPRESSION: &'static str = "CONCAT('0x', HEX(bytes))";
    type Connection = Client;

    fn quote_identifier(name: &str) -> String {
        mysql::quote_identifier(name)
    }

    fn quote_literal(text: &str) -> String {
        mysql::quote_literal(text)
    }

    fn bytes_literal(bytes: &[u8]) -> String {
        hex::hex_literal(bytes)
    }

    fn column_bytes(text: String) -> Vec<u8> {
        hex::column_bytes(text)
    }

    fn archived_at() -> String {
        archive::timestamp::datetime_utc(SystemTime::now())
    }

    fn connect(server: &Server) -> Result<Client, ArchiveError> {
        let login = Login::new(&*server.user, server.password.clone().unwrap_or_default());
        Client::connect(&server.address, &server.database, &login, server.timeout)
            .map_err(ArchiveError::caused_by)
    }

    fn select(client: &mut Client, sql: &str) -> Result<Vec<Row>, ArchiveError> {
        Ok(client.query(sql).map_err(ArchiveError::caused_by)?.rows)
    }

    /// The INSERT answers a count, so the id is asked for after it.
    fn insert(client: &mut Client, sql: &str) -> Result<Option<String>, ArchiveError> {
        client.execute(sql).map_err(ArchiveError::caused_by)?;
        Ok(first_cell(Self::select(client, LAST_INSERT_ID)?))
    }

    fn close(client: Client) -> Result<(), ArchiveError> {
        client.close().map_err(ArchiveError::caused_by)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archive::fixture::{item, secs};
    use archive::sql::{SqlArchive, insert_sql, item_from_row, select_sql};
    use archive::{ArchiveItem, ArchiveReceipt, ArchiveStore, metadata};
    use mysql::{Answer, Event, Session};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

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
            Some(metadata::encode(&held.metadata)),
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
                        } else if sql == LAST_INSERT_ID {
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

    fn quoted() -> ArchiveItem {
        ArchiveItem {
            data_type: "json".to_string(),
            identifier: "it's #1".to_string(),
            bytes: vec![0x7b, 0xff],
            metadata: vec![("source".to_string(), "playground".to_string())],
        }
    }

    #[test]
    fn the_insert_names_the_five_columns_and_the_select_spells_the_bytes() {
        let sql = insert_sql::<MySql>("audit.archive", &quoted(), "2026-09-09 12:00:00");
        assert!(sql.starts_with(
            "INSERT INTO `audit`.`archive` \
             (data_type, identifier, bytes, metadata, archived_at) VALUES ('json', 'it\\'s #1', \
             X'7bff', "
        ));
        assert!(sql.ends_with("'2026-09-09 12:00:00')"), "{sql}");
        assert_eq!(
            select_sql::<MySql>("Archive", 41),
            "SELECT data_type, identifier, CONCAT('0x', HEX(bytes)), metadata \
             FROM `Archive` WHERE id = 41"
        );
    }

    #[test]
    fn a_row_in_either_bytes_form_is_the_item_again() {
        let original = quoted();
        let hex = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("0x7BFF".to_string()),
            Some(metadata::encode(&original.metadata)),
        ];
        assert_eq!(item_from_row::<MySql>(&hex, "here").expect("row"), original);
        let text = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("plain".to_string()),
            Some(String::new()),
        ];
        let restored = item_from_row::<MySql>(&text, "here").expect("row");
        assert_eq!(restored.bytes, b"plain");
        assert!(restored.metadata.is_empty());
    }

    #[test]
    fn an_archived_item_is_one_insert_and_its_receipt_names_the_row() {
        let original = item("json#1");
        let (address, far_end) = far_end("secret", &original, 1);
        let store = SqlArchive::<MySql>::new(address.clone(), "orders", "xmip")
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
        assert_eq!(events[1], Event::Executed(LAST_INSERT_ID.to_string()));
    }

    #[test]
    fn the_row_restores_the_item_over_a_second_connection() {
        let original = item("json#2");
        let (address, far_end) = far_end("", &original, 2);
        let store = SqlArchive::<MySql>::new(address, "orders", "xmip").timing_out_after(secs(2));
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
        let store = SqlArchive::<MySql>::new(address, "orders", "xmip")
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
    }
}
