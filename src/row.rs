//! One item as one row: the INSERT that stores it, the SELECT that brings
//! it back, and the mapping between the item's fields and the columns.
//!
//! The columns are the four every archive technology carries — `data_type`,
//! `identifier`, `bytes`, `metadata` — plus `archived_at`, so a row reads the
//! same to an operator as a Parquet file or a `PostgreSQL` row does. The
//! bytes go in as the `X'…'` hexadecimal literal a BLOB stores as the
//! bytes, and come back spelled out — `CONCAT('0x', HEX(bytes))` — because
//! the text protocol hands a BLOB over raw and the row reader would take it
//! for text. A text column that kept a hex form verbatim answers the bytes
//! too.

use archive::{ArchiveError, ArchiveItem, metadata};
use mysql::{hex, quote_identifier, quote_literal};

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
        table_name(table),
        quote_literal(&item.data_type),
        quote_literal(&item.identifier),
        hex::hex_literal(&item.bytes),
        quote_literal(&metadata::encode(&item.metadata)),
        quote_literal(archived_at)
    )
}

/// The statement that brings row `id` of `table` back, the four columns in
/// the order [`item_from_row`] reads them, the bytes spelled out in hex.
#[must_use]
pub fn select_sql(table: &str, id: u64) -> String {
    format!(
        "SELECT data_type, identifier, CONCAT('0x', HEX(bytes)), metadata FROM {} \
         WHERE id = {id}",
        table_name(table)
    )
}

/// One row, the four columns in order, back into an item.
///
/// # Errors
/// Where a column is missing or NULL.
pub fn item_from_row(row: &[Option<String>], location: &str) -> Result<ArchiveItem, ArchiveError> {
    let column = |index: usize, name: &str| {
        row.get(index)
            .cloned()
            .flatten()
            .ok_or_else(|| ArchiveError {
                message: format!("column {name} is missing or NULL in {location}"),
            })
    };
    Ok(ArchiveItem {
        data_type: column(0, "data_type")?,
        identifier: column(1, "identifier")?,
        bytes: hex::column_bytes(column(2, "bytes")?),
        metadata: metadata::decode(&column(3, "metadata")?),
    })
}

/// A table name quoted segment by segment, so `audit.archive` stays a table
/// in a schema and `Archive` keeps its case.
fn table_name(table: &str) -> String {
    table
        .split('.')
        .map(quote_identifier)
        .collect::<Vec<_>>()
        .join(".")
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
            select_sql("Archive", 41),
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
        assert_eq!(item_from_row(&hex, "here").expect("row"), original);
        let text = vec![
            Some("json".to_string()),
            Some("it's #1".to_string()),
            Some("plain".to_string()),
            Some(String::new()),
        ];
        let restored = item_from_row(&text, "here").expect("row");
        assert_eq!(restored.bytes, b"plain");
        assert!(restored.metadata.is_empty());
        let short = [Some("json".to_string())];
        let failure = item_from_row(&short, "here").expect_err("missing");
        assert!(failure.message.contains("identifier"));
        let null = [None, Some("x".to_string())];
        assert!(item_from_row(&null, "here").is_err());
    }
}
