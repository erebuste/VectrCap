use image::{Rgba, RgbaImage};
use std::sync::Arc;
use vectrcap::{
    capture::geometry::{PhysicalPoint, PhysicalRect},
    editor::{
        Annotation, EditorCommand, EditorDocument, Style,
        effects::SmartEraser,
        eraser::erase_annotation_hits,
        render_export,
        shapes::{constrained_segment, constrained_shape},
    },
};
fn point(x: i32, y: i32) -> PhysicalPoint {
    PhysicalPoint { x, y }
}
fn rect(x: i32, y: i32, w: u32, h: u32) -> PhysicalRect {
    PhysicalRect::new(x, y, w, h).unwrap()
}
fn doc() -> EditorDocument {
    EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(80, 60, Rgba([255; 4])))).unwrap()
}
fn insert(d: &mut EditorDocument, a: Annotation) {
    d.apply(EditorCommand::Insert(a)).unwrap();
}
#[test]
fn rectangle_ellipse_outline_and_fill() {
    for ellipse in [false, true] {
        for fill in [None, Some([0, 200, 0, 255])] {
            let mut d = doc();
            let style = Style::new([200, 0, 0, 255], 2.0, fill).unwrap();
            insert(
                &mut d,
                if ellipse {
                    Annotation::ellipse(rect(10, 10, 40, 30), style).unwrap()
                } else {
                    Annotation::rectangle(rect(10, 10, 40, 30), style).unwrap()
                },
            );
            let out = render_export(&d).unwrap();
            assert_eq!(out.get_pixel(30, 25).0, fill.unwrap_or([255; 4]));
            assert!(out.get_pixel(30, 10)[0] > 180 && out.get_pixel(30, 10)[1] < 100);
            assert_eq!(out.get_pixel(0, 0).0, [255; 4]);
        }
    }
}
#[test]
fn shift_constrains_shapes_and_line_angles() {
    assert_eq!(
        constrained_shape(point(20, 20), point(40, 30), true).unwrap(),
        rect(20, 20, 20, 20)
    );
    assert_eq!(
        constrained_shape(point(20, 20), point(0, 10), true).unwrap(),
        rect(0, 0, 20, 20)
    );
    let end = constrained_segment(point(10, 10), point(40, 17), true).unwrap();
    assert_eq!(end.y, 10);
    let diagonal = constrained_segment(point(0, 0), point(19, 17), true).unwrap();
    assert_eq!(diagonal.x, diagonal.y);
    let mut d = doc();
    insert(
        &mut d,
        Annotation::line(point(5, 10), end, [0, 0, 0, 255], 2.0).unwrap(),
    );
    insert(
        &mut d,
        Annotation::arrow(point(5, 35), point(60, 35), [0, 0, 200, 255], 3.0).unwrap(),
    );
    let out = render_export(&d).unwrap();
    assert_eq!(out.get_pixel(20, 10).0, [0, 0, 0, 255]);
    assert_eq!(out.get_pixel(50, 35).0, [0, 0, 200, 255]);
}
#[test]
fn step_numbers_survive_delete_and_undo() {
    let mut d = doc();
    assert_eq!(d.next_step(), 1);
    for x in [10, 30, 50] {
        d.apply(EditorCommand::Step {
            center: point(x, 30),
            radius: 8.0,
            color: [200, 0, 0, 255],
        })
        .unwrap();
    }
    let first = d.annotations()[0].id();
    let second = d.annotations()[1].id();
    d.apply(EditorCommand::Erase(vec![second])).unwrap();
    let numbers = |d: &EditorDocument| {
        d.annotations()
            .iter()
            .filter_map(|a| {
                if let Annotation::Step { number, .. } = a {
                    Some(*number)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(numbers(&d), vec![1, 3]);
    assert_eq!(d.next_step(), 4);
    d.undo().unwrap();
    assert_eq!(numbers(&d), vec![1, 2, 3]);
    assert_eq!(d.annotations()[0].id(), first);
    d.undo().unwrap();
    assert_eq!(d.next_step(), 3);
    d.redo().unwrap();
    assert_eq!(d.next_step(), 4);
    assert_ne!(render_export(&d).unwrap(), *d.source().as_ref());
    assert_eq!(doc().next_step(), 1);
}
#[test]
fn highlight_blends_at_35_percent() {
    let mut d = doc();
    insert(
        &mut d,
        Annotation::highlight(rect(10, 10, 20, 20), [255, 255, 0], 0.35).unwrap(),
    );
    let out = render_export(&d).unwrap();
    let p = out.get_pixel(15, 15).0;
    assert_eq!(&p[..2], &[255, 255]);
    assert!((i32::from(p[2]) - 166).abs() <= 1);
    assert_eq!(p[3], 255);
    assert_eq!(out.get_pixel(0, 0).0, [255; 4]);
}
#[test]
fn spotlight_dims_only_outside_focus_union() {
    let mut d = doc();
    insert(
        &mut d,
        Annotation::spotlight(rect(10, 10, 20, 20), 0.6).unwrap(),
    );
    insert(
        &mut d,
        Annotation::spotlight(rect(20, 20, 20, 20), 0.6).unwrap(),
    );
    let out = render_export(&d).unwrap();
    for p in [(15, 15), (25, 25), (35, 35)] {
        assert_eq!(out.get_pixel(p.0, p.1).0, [255; 4]);
    }
    assert_eq!(out.get_pixel(0, 0).0, [102, 102, 102, 255]);
    // A normal annotation remains above spotlight, irrespective of insertion order.
    let mut d = doc();
    insert(
        &mut d,
        Annotation::rectangle(
            rect(0, 0, 5, 5),
            Style::new([255, 0, 0, 255], 1.0, Some([255, 0, 0, 255])).unwrap(),
        )
        .unwrap(),
    );
    insert(
        &mut d,
        Annotation::spotlight(rect(20, 20, 10, 10), 0.6).unwrap(),
    );
    assert_eq!(
        render_export(&d).unwrap().get_pixel(2, 2).0,
        [255, 0, 0, 255]
    );
}
#[test]
fn annotation_eraser_changes_objects_not_source() {
    let mut d = doc();
    let source = d.source().clone();
    let crop = d.crop();
    let mark = Annotation::line(point(5, 20), point(60, 20), [0, 0, 0, 255], 2.0).unwrap();
    let id = mark.id();
    insert(&mut d, mark);
    // A sparse eraser drag crosses the line between events.
    let hits = erase_annotation_hits(&d, &[point(30, 0), point(30, 40)], 1.0).unwrap();
    assert_eq!(hits, vec![id]);
    d.apply(EditorCommand::Erase(hits)).unwrap();
    assert!(d.annotations().is_empty());
    assert_eq!(d.crop(), crop);
    assert!(Arc::ptr_eq(d.source(), &source));
    assert_eq!(render_export(&d).unwrap(), *source);
}
#[test]
fn smart_eraser_samples_source_and_paints_opaque() {
    let mut d = EditorDocument::from_image(Arc::new(RgbaImage::from_pixel(
        80,
        60,
        Rgba([10, 80, 120, 0]),
    )))
    .unwrap();
    insert(
        &mut d,
        Annotation::redaction(rect(0, 0, 10, 10), [255, 0, 0]).unwrap(),
    );
    let eraser = SmartEraser::sample(&d, point(2, 2)).unwrap();
    insert(
        &mut d,
        eraser
            .stroke(vec![point(20, 30), point(60, 30)].into(), 8.0)
            .unwrap(),
    );
    assert_eq!(
        render_export(&d).unwrap().get_pixel(40, 30).0,
        [10, 80, 120, 255]
    );
    assert!(SmartEraser::sample(&d, point(-1, 0)).is_err());
}
#[test]
fn eraser_gesture_undo_restores_all_hits() {
    let mut d = doc();
    for x in [10, 30, 50] {
        insert(
            &mut d,
            Annotation::rectangle(
                rect(x, 10, 10, 10),
                Style::new([0, 0, 0, 255], 2.0, Some([0, 0, 0, 255])).unwrap(),
            )
            .unwrap(),
        );
    }
    let before = render_export(&d).unwrap();
    let ids: Vec<_> = d.annotations().iter().map(Annotation::id).collect();
    let hits =
        erase_annotation_hits(&d, &[point(5, 15), point(70, 15), point(5, 15)], 2.0).unwrap();
    assert_eq!(hits, ids);
    d.apply(EditorCommand::Erase(hits)).unwrap();
    assert!(d.annotations().is_empty());
    d.undo().unwrap();
    assert_eq!(render_export(&d).unwrap(), before);
    assert_eq!(
        d.annotations()
            .iter()
            .map(Annotation::id)
            .collect::<Vec<_>>(),
        ids
    );
}
#[test]
fn final_redaction_survives_other_effects() {
    let mut d = doc();
    insert(
        &mut d,
        Annotation::redaction(rect(10, 10, 20, 20), [7, 8, 9]).unwrap(),
    );
    insert(
        &mut d,
        Annotation::highlight(rect(0, 0, 50, 50), [255, 255, 0], 0.35).unwrap(),
    );
    insert(
        &mut d,
        Annotation::spotlight(rect(50, 30, 20, 20), 0.6).unwrap(),
    );
    let eraser = SmartEraser::sample(&d, point(0, 0)).unwrap();
    insert(
        &mut d,
        eraser
            .stroke(vec![point(0, 20), point(70, 20)].into(), 40.0)
            .unwrap(),
    );
    let out = render_export(&d).unwrap();
    for y in 10..30 {
        for x in 10..30 {
            assert_eq!(out.get_pixel(x, y).0, [7, 8, 9, 255]);
        }
    }
}
#[test]
fn rejects_excessive_annotation_input() {
    let mut d = doc();
    let before = d.revision();
    assert!(Annotation::highlight(rect(0, 0, 10, 10), [255, 255, 0], 0.35).is_ok());
    assert!(Annotation::spotlight(rect(0, 0, 10, 10), 0.6).is_ok());
    for value in [f32::NAN, f32::INFINITY, -1.0, 0.0, 2048.0] {
        assert!(Style::new([0; 4], value, None).is_err());
    }
    for opacity in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert!(Annotation::highlight(rect(0, 0, 10, 10), [255, 255, 0], opacity).is_err());
        assert!(Annotation::spotlight(rect(0, 0, 10, 10), opacity).is_err());
    }
    assert!(Annotation::text(point(0, 0), "a".repeat(8193).into(), 16.0, [0; 4]).is_err());
    assert!(Annotation::freehand(vec![point(0, 0); 20001].into(), [0; 4], 1.0).is_err());
    assert_eq!(d.revision(), before);
    for _ in 0..10 {
        insert(
            &mut d,
            Annotation::freehand(vec![point(0, 0); 20000].into(), [0; 4], 1.0).unwrap(),
        );
    }
    let revision = d.revision();
    assert!(
        d.apply(EditorCommand::Insert(
            Annotation::freehand(vec![point(0, 0)].into(), [0; 4], 1.0).unwrap()
        ))
        .is_err()
    );
    assert_eq!(d.revision(), revision);
    assert_eq!(d.annotations().len(), 10);
    let mut d = doc();
    for _ in 0..2000 {
        insert(
            &mut d,
            Annotation::redaction(rect(0, 0, 1, 1), [0; 3]).unwrap(),
        );
    }
    let revision = d.revision();
    assert!(
        d.apply(EditorCommand::Insert(
            Annotation::redaction(rect(0, 0, 1, 1), [0; 3]).unwrap()
        ))
        .is_err()
    );
    assert_eq!(d.revision(), revision);
    assert_eq!(d.annotations().len(), 2000);
}

#[test]
fn overflowing_tool_geometry_is_rejected_transactionally() {
    assert!(
        Annotation::line(
            point(i32::MIN, i32::MIN),
            point(i32::MAX, i32::MAX),
            [0; 4],
            1.0
        )
        .is_err()
    );
    let mut d = doc();
    let revision = d.revision();
    assert!(
        d.apply(EditorCommand::Step {
            center: point(i32::MAX, 0),
            radius: 20.0,
            color: [0; 4]
        })
        .is_err()
    );
    assert_eq!(d.revision(), revision);
    assert_eq!(d.next_step(), 1);
    assert!(d.annotations().is_empty());
}
#[test]
fn eraser_hits_every_object_kind_and_preserves_hollow_interiors() {
    let mut d = doc();
    insert(
        &mut d,
        Annotation::ellipse(
            rect(10, 10, 40, 30),
            Style::new([0, 0, 0, 255], 2.0, None).unwrap(),
        )
        .unwrap(),
    );
    assert!(
        erase_annotation_hits(&d, &[point(30, 25)], 1.0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        erase_annotation_hits(&d, &[point(30, 10)], 1.0)
            .unwrap()
            .len(),
        1
    );
    let style = Style::new([0, 0, 0, 255], 2.0, None).unwrap();
    let sampled = SmartEraser::sample(&d, point(0, 0)).unwrap();
    for a in [
        Annotation::rectangle(rect(10, 10, 40, 30), style).unwrap(),
        Annotation::arrow(point(10, 10), point(50, 40), [0, 0, 0, 255], 2.0).unwrap(),
        Annotation::freehand(
            vec![point(10, 10), point(50, 40)].into(),
            [0, 0, 0, 255],
            2.0,
        )
        .unwrap(),
        Annotation::text(point(10, 10), "Test".into(), 20.0, [0, 0, 0, 255]).unwrap(),
        Annotation::highlight(rect(10, 10, 40, 30), [255, 255, 0], 0.35).unwrap(),
        Annotation::spotlight(rect(10, 10, 40, 30), 0.6).unwrap(),
        Annotation::redaction(rect(10, 10, 40, 30), [0; 3]).unwrap(),
        sampled
            .stroke(vec![point(10, 10), point(50, 40)].into(), 2.0)
            .unwrap(),
    ] {
        insert(&mut d, a);
    }
    d.apply(EditorCommand::Step {
        center: point(30, 25),
        radius: 8.0,
        color: [200, 0, 0, 255],
    })
    .unwrap();
    let hits = erase_annotation_hits(&d, &[point(30, 0), point(30, 50)], 2.0).unwrap();
    assert_eq!(hits.len(), d.annotations().len());
    for radius in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(erase_annotation_hits(&d, &[point(10, 10)], radius).is_err());
    }
    assert!(erase_annotation_hits(&d, &[], 1.0).is_err());
    assert!(erase_annotation_hits(&d, &vec![point(0, 0); 20001], 1.0).is_err());
}

#[test]
fn commands_and_history_keep_shared_geometry_and_stable_identity() {
    let mut d = doc();
    let points: Arc<[PhysicalPoint]> = vec![point(2, 2), point(20, 20)].into();
    let annotation = Annotation::freehand(points.clone(), [0, 0, 0, 255], 2.0).unwrap();
    let id = annotation.id();
    insert(&mut d, annotation);
    let source = d.source().clone();
    for i in 0..70 {
        d.apply(EditorCommand::Crop(Some(rect(i % 10, 0, 60, 50))))
            .unwrap();
    }
    let mut count = 0;
    while d.undo().unwrap() {
        count += 1;
    }
    assert_eq!(count, 64);
    assert!(Arc::ptr_eq(d.source(), &source));
    assert!(
        matches!(&d.annotations()[0],Annotation::Freehand{points:stored,..} if Arc::ptr_eq(stored,&points))
    );
    let mut changed = d.annotations()[0].clone();
    if let Annotation::Freehand { color, .. } = &mut changed {
        *color = [200, 0, 0, 255];
    }
    d.apply(EditorCommand::Modify(changed)).unwrap();
    assert_eq!(d.annotations()[0].id(), id);
    assert!(!d.redo().unwrap());
    let revision = d.revision();
    assert!(
        d.apply(EditorCommand::Erase(vec![uuid::Uuid::new_v4()]))
            .is_err()
    );
    assert_eq!(d.revision(), revision);
    assert_eq!(d.annotations()[0].id(), id);
}
#[test]
fn excessive_eraser_work_is_bounded_without_partial_deletion() {
    let mut d = doc();
    insert(
        &mut d,
        Annotation::freehand(vec![point(0, 0); 20000].into(), [0, 0, 0, 255], 1.0).unwrap(),
    );
    let revision = d.revision();
    let ids: Vec<_> = d.annotations().iter().map(Annotation::id).collect();
    assert!(erase_annotation_hits(&d, &vec![point(70, 50); 20000], 1.0).is_err());
    assert_eq!(d.revision(), revision);
    assert_eq!(
        d.annotations()
            .iter()
            .map(Annotation::id)
            .collect::<Vec<_>>(),
        ids
    );
}
