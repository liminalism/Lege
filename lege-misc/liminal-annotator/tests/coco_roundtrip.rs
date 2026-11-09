//! Import → export with no edits preserves every page, annotation and field;
//! edits are reported in review/delta and never touch the imported layer.

use std::path::Path;

use liminal_annotator::annotation::commands::ReviewStatus;
use liminal_annotator::annotation::geometry::Box2;
use liminal_annotator::dataset::database::{self, PageFilter, PageSave};
use liminal_annotator::dataset::export_coco::{export, ExportOptions};
use liminal_annotator::dataset::import_coco::{import_coco, ImportOptions};
use serde_json::{json, Value};

const CATS: [i64; 4] = [1, 7, 8, 2];

fn write_coco(dir: &Path) -> std::path::PathBuf {
    let images: Vec<Value> = (0..5)
        .map(|i| {
            json!({
                "id": i + 1, "file_name": format!("images/dev/book{}/p{:04}.jpg", i % 2, i),
                "width": 850, "height": 1280, "page_key": format!("book{}_p{:04}", i % 2, i),
                "book_id": format!("book{}", i % 2), "split": "dev", "book_type": "white_scan",
                "page_index": i, "queue_reason": "multi_column,figure", "label_meta": {"line_height": 19.2, "gutters": [[400.0, 430.0]]}
            })
        })
        .collect();
    let mut anns = Vec::new();
    let mut id = 100;
    for i in 0..5 {
        #[allow(clippy::needless_range_loop)]
        for k in 0..4 {
            id += 1;
            let (x, y, w, h) = (60.5 + k as f64, 100.25 + 200.0 * k as f64, 700.125, 150.0);
            anns.push(json!({
                "id": id, "image_id": i + 1, "category_id": CATS[k], "bbox": [x, y, w, h],
                "area": w * h, "iscrowd": if k == 3 { 1 } else { 0 },
                "segmentation": [[x, y, x + w, y, x + w, y + h, x, y + h]],
                "score": 0.75, "source": "agree", "teachers": {"paddle": {"cls": "text", "score": 0.8}},
                "repairs": ["widen"], "flags": [], "granularity": null, "class_conflict": k == 1
            }));
        }
    }
    // Annotations deliberately *before* images: the importer must cope.
    let coco = json!({
        "info": {"description": "test set", "params_hash": "abc"},
        "licenses": [],
        "categories": liminal_annotator::annotation::labels::CLASSES.iter().map(|c| json!({
            "id": c.id + 1, "taxonomy_id": c.id, "name": c.name, "supercategory": c.supercategory, "pp23_export_id": c.pp23_export_id
        })).collect::<Vec<_>>(),
        "annotations": anns,
        "images": images,
    });
    let path = dir.join("annotations").join("dev_test.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&coco).unwrap()).unwrap();
    path
}

fn load(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn unedited_round_trip_is_lossless() {
    let dir = tempfile::tempdir().unwrap();
    let coco = write_coco(dir.path());
    let db = dir.path().join("p.db");
    let mut conn = database::open(&db).unwrap();
    let rep = import_coco(
        &mut conn,
        &coco,
        ImportOptions {
            set_name: "dev_test".into(),
            images_root: dir.path().into(),
        },
    )
    .unwrap();
    assert_eq!((rep.pages, rep.boxes, rep.ignore_regions), (5, 20, 5));
    // Importing again is idempotent: every page is skipped.
    let rep2 = import_coco(
        &mut conn,
        &coco,
        ImportOptions {
            set_name: "dev_test".into(),
            images_root: dir.path().into(),
        },
    )
    .unwrap();
    assert_eq!((rep2.pages, rep2.pages_skipped_existing), (0, 5));

    let out = dir.path().join("exports");
    let r = export(
        &conn,
        &ExportOptions {
            out_dir: out.clone(),
            name: Some("v1".into()),
            sizes: vec![640, 1024],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((r.pages, r.boxes), (5, 20));

    let orig = load(&coco);
    let exp = load(&out.join("v1/original/annotations.json"));
    assert_eq!(orig["categories"], exp["categories"]);
    let key_of = |d: &Value, id: &Value| {
        d["images"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| &i["id"] == id)
            .unwrap()["page_key"]
            .clone()
    };
    for (oi, ei) in orig["images"]
        .as_array()
        .unwrap()
        .iter()
        .zip(exp["images"].as_array().unwrap())
    {
        for (k, v) in oi.as_object().unwrap() {
            if k != "id" {
                assert_eq!(&ei[k], v, "image field {k}");
            }
        }
    }
    let oa = orig["annotations"].as_array().unwrap();
    let ea = exp["annotations"].as_array().unwrap();
    assert_eq!(oa.len(), ea.len());
    for (a, b) in oa.iter().zip(ea) {
        assert_eq!(
            key_of(&orig, &a["image_id"]),
            key_of(&exp, &b["image_id"]),
            "image association"
        );
        for k in [
            "category_id",
            "bbox",
            "area",
            "iscrowd",
            "score",
            "source",
            "teachers",
            "repairs",
            "flags",
            "granularity",
            "class_conflict",
            "segmentation",
        ] {
            assert_eq!(a[k], b[k], "annotation field {k}");
        }
        assert_eq!(b["annotator"]["source_id"], a["id"]);
        assert_eq!(b["annotator"]["change"], "unchanged");
    }

    // Stretched export: S×S images, boxes scaled per axis, area recomputed.
    let s = load(&out.join("v1/s640/annotations.json"));
    assert_eq!(s["images"][0]["width"], 640);
    assert_eq!(s["images"][0]["orig_width"], 850);
    let b0 = s["annotations"][0]["bbox"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect::<Vec<_>>();
    assert!((b0[0] - 60.5 * 640.0 / 850.0).abs() < 1e-6 && (b0[1] - 100.25 * 0.5).abs() < 1e-6);
    assert!((s["annotations"][0]["area"].as_f64().unwrap() - b0[2] * b0[3]).abs() < 1e-6);
    let m = load(&out.join("v1/manifest.json"));
    assert_eq!(m["pages"], 5);
    assert!(m["split_leaks"].as_array().unwrap().is_empty());

    // Never overwrite an export.
    assert!(export(
        &conn,
        &ExportOptions {
            out_dir: out.clone(),
            name: Some("v1".into()),
            ..Default::default()
        }
    )
    .is_err());
}

#[test]
fn edits_are_reported_and_the_imported_layer_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let coco = write_coco(dir.path());
    let db = dir.path().join("p.db");
    let mut conn = database::open(&db).unwrap();
    import_coco(
        &mut conn,
        &coco,
        ImportOptions {
            set_name: "s".into(),
            images_root: dir.path().into(),
        },
    )
    .unwrap();
    let key = "book0_p0000";
    let mut anns = database::annotations(&conn, key).unwrap();
    anns[0].bbox = Box2::new(10.0, 10.0, 500.0, 300.0); // modified
    anns[1].class_id = 0; // reclassified figure -> text
    anns.remove(2); // deleted caption
    anns.push(liminal_annotator::annotation::commands::Annotation {
        id: "h-new".into(),
        class_id: 7,
        bbox: Box2::new(100.0, 1200.0, 400.0, 1300.0),
        is_ignore: false,
        provenance_json: None,
    }); // added, extends past the page bottom → clamped on export
    database::save_page(
        &mut conn,
        &PageSave {
            page_key: key.into(),
            status: ReviewStatus::Reviewed,
            annotations: anns,
            revision: 1,
            kind: "test",
        },
    )
    .unwrap();
    let exclude = "book1_p0001";
    let a = database::annotations(&conn, exclude).unwrap();
    database::save_page(
        &mut conn,
        &PageSave {
            page_key: exclude.into(),
            status: ReviewStatus::Excluded,
            annotations: a,
            revision: 1,
            kind: "test",
        },
    )
    .unwrap();

    let imported = database::imported_annotations(&conn, key).unwrap();
    assert_eq!(imported.len(), 4, "imported layer is immutable");

    let out = dir.path().join("exports");
    let r = export(
        &conn,
        &ExportOptions {
            out_dir: out.clone(),
            name: Some("e".into()),
            sizes: vec![640],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.excluded_pages, 1);
    assert_eq!(r.pages, 4);
    assert_eq!(r.clamped_boxes, 1);
    let review = load(&out.join("e/review.json"));
    let page = review["pages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["page_key"] == key)
        .unwrap();
    assert_eq!(
        page["counts"],
        json!({"unchanged": 1, "modified": 2, "added": 1, "deleted": 1})
    );
    assert_eq!(review["excluded"], json!([exclude]));
    let delta = load(&out.join("e/delta.json"));
    assert_eq!(
        delta["reclassifications"],
        json!([{"from": "figure", "to": "text", "count": 1}])
    );
    assert_eq!(delta["by_class"]["caption"]["deleted"], 1);
    assert_eq!(delta["by_class"]["caption"]["added"], 1);
    assert_eq!(delta["by_class"]["text"]["geometry_modified"], 1);

    // Reviewed-only export.
    let r = export(
        &conn,
        &ExportOptions {
            out_dir: out.clone(),
            name: Some("r".into()),
            reviewed_only: true,
            filter: PageFilter::default(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.pages, 1);
}

#[test]
fn split_leaks_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let coco = write_coco(dir.path());
    let db = dir.path().join("p.db");
    let mut conn = database::open(&db).unwrap();
    import_coco(
        &mut conn,
        &coco,
        ImportOptions {
            set_name: "s".into(),
            images_root: dir.path().into(),
        },
    )
    .unwrap();
    conn.execute(
        "UPDATE pages SET split = 'train' WHERE page_key = 'book0_p0002'",
        [],
    )
    .unwrap();
    let out = dir.path().join("exports");
    let err = export(
        &conn,
        &ExportOptions {
            out_dir: out.clone(),
            name: Some("x".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.contains("book0"), "{err}");
    assert!(
        !out.join("x").exists() && !out.join(".x.partial").exists(),
        "failed export leaves nothing behind"
    );
    let r = export(
        &conn,
        &ExportOptions {
            out_dir: out,
            name: Some("y".into()),
            strict_splits: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.split_leaks.len(), 1);
}

/// The real regression set, when the dataset is present on this machine.
#[test]
fn real_regression_set_round_trips() {
    // The layout dataset lives outside this repository: $LAYOUT_DATA_DIR, or
    // the sibling checkout layout on the development machine.
    let data = std::env::var_os("LAYOUT_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "../../../google-colab-work/layout-detection/data".into());
    let coco = &data.join("annotations/regression.json");
    if !coco.exists() {
        eprintln!("skipped: {} not present", coco.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut conn = database::open(&dir.path().join("p.db")).unwrap();
    let rep = import_coco(
        &mut conn,
        coco,
        ImportOptions {
            set_name: "regression".into(),
            images_root: coco.parent().unwrap().parent().unwrap().into(),
        },
    )
    .unwrap();
    assert_eq!((rep.pages, rep.boxes, rep.ignore_regions), (88, 494, 12));
    let out = dir.path().join("x");
    export(
        &conn,
        &ExportOptions {
            out_dir: out.clone(),
            name: Some("rt".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let orig = load(coco);
    let exp = load(&out.join("rt/original/annotations.json"));
    let oa = orig["annotations"].as_array().unwrap();
    let ea = exp["annotations"].as_array().unwrap();
    assert_eq!(oa.len(), ea.len());
    for (a, b) in oa.iter().zip(ea) {
        assert_eq!(a["bbox"], b["bbox"]);
        assert_eq!(a["area"], b["area"]);
        assert_eq!(a["segmentation"], b["segmentation"]);
        assert_eq!(a["category_id"], b["category_id"]);
        assert_eq!(a["iscrowd"], b["iscrowd"]);
        assert_eq!(a["teachers"], b["teachers"]);
        assert_eq!(b["annotator"]["source_id"], a["id"]);
    }
    assert_eq!(
        orig["images"].as_array().unwrap().len(),
        exp["images"].as_array().unwrap().len()
    );
}
