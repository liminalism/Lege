use serde::Serialize;

use super::text::TextProvenance;

#[derive(Debug, Clone, Serialize)]
pub struct TablesPageData {
    pub unit: &'static str,
    pub tables: Vec<TableView>,
    pub provenance: TextProvenance,
}

/// A table found on a page: rows of cells, top to bottom. Every row has
/// `columns` cells; one with nothing in it is an empty string.
#[derive(Debug, Clone, Serialize)]
pub struct TableView {
    #[serde(serialize_with = "super::rounded_box")]
    pub bbox: [f64; 4],
    pub rows: usize,
    pub columns: usize,
    pub cells: Vec<Vec<String>>,
}
