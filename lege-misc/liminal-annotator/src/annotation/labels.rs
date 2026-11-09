//! The 14-class book-layout taxonomy (`data/annotations/categories.json`)
//! and its display grouping. Class id = taxonomy id = COCO `category_id - 1`.
//!
//! Hotkey order lives in [`crate::settings`], independent of these ids, so
//! text and figure can sit on keys 1 and 2 without touching the training
//! taxonomy.

pub const NUM_CLASSES: usize = 14;

/// Review grouping. Display/evaluation only; annotations keep full classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    TextLike,
    Furniture,
    Figure,
    Table,
}

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Group::TextLike => "text-like",
            Group::Furniture => "furniture",
            Group::Figure => "figure",
            Group::Table => "table",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClassInfo {
    pub id: u8,
    pub name: &'static str,
    /// Short label for the toolbar.
    pub short: &'static str,
    pub supercategory: &'static str,
    pub group: Group,
    pub pp23_export_id: u8,
    /// Default outline colour, 0xRRGGBB.
    pub color: u32,
}

pub const CLASSES: [ClassInfo; NUM_CLASSES] = [
    ClassInfo {
        id: 0,
        name: "text",
        short: "Text",
        supercategory: "textlike",
        group: Group::TextLike,
        pp23_export_id: 2,
        color: 0x3d8bff,
    },
    ClassInfo {
        id: 1,
        name: "heading",
        short: "Heading",
        supercategory: "textlike",
        group: Group::TextLike,
        pp23_export_id: 0,
        color: 0xff4fa3,
    },
    ClassInfo {
        id: 2,
        name: "running_header",
        short: "Run. head",
        supercategory: "furniture",
        group: Group::Furniture,
        pp23_export_id: 13,
        color: 0x9b7bff,
    },
    ClassInfo {
        id: 3,
        name: "running_footer",
        short: "Run. foot",
        supercategory: "furniture",
        group: Group::Furniture,
        pp23_export_id: 15,
        color: 0x7c6ad6,
    },
    ClassInfo {
        id: 4,
        name: "page_number",
        short: "Page no.",
        supercategory: "furniture",
        group: Group::Furniture,
        pp23_export_id: 3,
        color: 0xc78bff,
    },
    ClassInfo {
        id: 5,
        name: "footnote",
        short: "Footnote",
        supercategory: "textlike",
        group: Group::TextLike,
        pp23_export_id: 12,
        color: 0x00c2b8,
    },
    ClassInfo {
        id: 6,
        name: "figure",
        short: "Figure",
        supercategory: "figure",
        group: Group::Figure,
        pp23_export_id: 1,
        color: 0xff8a1f,
    },
    ClassInfo {
        id: 7,
        name: "caption",
        short: "Caption",
        supercategory: "textlike",
        group: Group::TextLike,
        pp23_export_id: 6,
        color: 0x2fcf4f,
    },
    ClassInfo {
        id: 8,
        name: "table",
        short: "Table",
        supercategory: "table",
        group: Group::Table,
        pp23_export_id: 8,
        color: 0xffd400,
    },
    ClassInfo {
        id: 9,
        name: "marginalia",
        short: "Marginalia",
        supercategory: "textlike",
        group: Group::TextLike,
        pp23_export_id: 22,
        color: 0x8fd14f,
    },
    ClassInfo {
        id: 10,
        name: "toc",
        short: "TOC",
        supercategory: "textlike",
        group: Group::TextLike,
        pp23_export_id: 5,
        color: 0x4fd1ff,
    },
    ClassInfo {
        id: 11,
        name: "reference",
        short: "Reference",
        supercategory: "textlike",
        group: Group::TextLike,
        pp23_export_id: 10,
        color: 0x5fa0c0,
    },
    ClassInfo {
        id: 12,
        name: "formula",
        short: "Formula",
        supercategory: "textlike",
        group: Group::TextLike,
        pp23_export_id: 7,
        color: 0xe0e060,
    },
    ClassInfo {
        id: 13,
        name: "ornament",
        short: "Ornament",
        supercategory: "figure",
        group: Group::Figure,
        pp23_export_id: 20,
        color: 0xd9692b,
    },
];

pub fn class(id: u8) -> &'static ClassInfo {
    &CLASSES[(id as usize).min(NUM_CLASSES - 1)]
}

pub fn class_by_name(name: &str) -> Option<u8> {
    CLASSES.iter().find(|c| c.name == name).map(|c| c.id)
}

/// The colour ignore regions are drawn in, whatever their class.
pub const IGNORE_COLOR: u32 = 0x9aa0a6;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_positions() {
        for (i, c) in CLASSES.iter().enumerate() {
            assert_eq!(c.id as usize, i);
        }
        assert_eq!(class_by_name("caption"), Some(7));
    }
}
