//! One item as one row, the `MySQL` part: the INSERT that stores it, the
//! statement that asks for its id afterwards, and the dialect the shared
//! row code is given — backtick identifiers, the bytes going in as the
//! `X'…'` hexadecimal literal a BLOB stores as the bytes, and coming back
//! spelled out — `CONCAT('0x', HEX(bytes))` — because the text protocol
//! hands a BLOB over raw and the row reader would take it for text. A text
//! column that kept a hex form verbatim answers the bytes too. The
//! columns, the SELECT and the row read back are the capability's
//! `archive::row` (ADR-0044).

use archive::row::Dialect;
use archive::{ArchiveItem, metadata};
use mysql::{hex, quote_identifier, quote_literal};

/// What `MySQL` does its own way, handed to the shared row code.
pub const DIALECT: Dialect = Dialect {
    quote_identifier,
    bytes_expression: "CONCAT('0x', HEX(bytes))",
    column_bytes: hex::column_bytes,
};

/// The statement that asks for the id the last insert on this connection
/// made, since the protocol's answer to the INSERT carries it and the
/// transport's [`mysql::QueryResult`] does not.
pub const LAST_INSERT_ID: &str = "SELECT LAST_INSERT_ID()";

/// The statement that stores `item` in `table` at `archived_at`; the id is
/// asked for afterwards with [`LAST_INSERT_ID`].
#[must_use]
pub fn insert_sql(table: &str, item: &ArchiveItem, archived_at: &str) -> String {
    format!(
        "INSERT INTO {} (data_type, identifier, bytes, metadata, archived_at) \
         VALUES ({}, {}, {}, {}, {})",
        DIALECT.table_name(table),
        quote_literal(&item.data_type),
        quote_literal(&item.identifier),
        hex::hex_literal(&item.bytes),
        quote_literal(&metadata::encode(&item.metadata)),
        quote_literal(archived_at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> ArchiveItem {
        ArchiveItem {
            data_type: "json".to_string(),
            identifier: "it's #1".to_string(),
            bytes: vec![0x7b, 0xff],
            metadata: vec![("source".to_string(), "playground".to_string())],
        }
    }

    #[test]
    fn the_insert_names_the_five_columns_and_the_select_spells_the_bytes() {
        let sql = insert_sql("audit.archive", &item(), "2026-09-09 12:00:00");
        assert!(sql.starts_with(
            "INSERT INTO `audit`.`archive` \
             (data_type, identifier, bytes, metadata, archived_at) VALUES ('json', 'it\\'s #1', \
             X'7bff', "
        ));
        assert!(sql.ends_with("'2026-09-09 12:00:00')"), "{sql}");
        assert_eq!(
            DIALECT.select_sql("Archive", 41),
            "SELECT data_type, identifier, CONCAT('0x', HEX(bytes)), metadata \
             FROM `Archive` WHERE id = 41"
        );
    }

    #[test]
    fn a_row_in_either_bytes_form_is_the_item_again() {
        let original = item();
        let hex = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("0x7BFF".to_string()),
            Some(metadata::encode(&original.metadata)),
        ];
        assert_eq!(DIALECT.item_from_row(&hex, "here").expect("row"), original);
        let text = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("plain".to_string()),
            Some(String::new()),
        ];
        let restored = DIALECT.item_from_row(&text, "here").expect("row");
        assert_eq!(restored.bytes, b"plain");
        assert!(restored.metadata.is_empty());
    }
}
