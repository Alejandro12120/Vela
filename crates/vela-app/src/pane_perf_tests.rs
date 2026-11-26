use std::{fs, hint::black_box, time::Duration};

use super::*;

#[gpui::test]
#[ignore = "10k pane rows and a 160x50 terminal performance benchmark"]
fn benchmark_perf_suite(cx: &mut gpui::TestAppContext) {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let root = directory.path();
    for index in (0..10_000).rev() {
        fs::File::create(root.join(format!("File{index}"))).unwrap();
    }
    let mut listing = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        listing = tree::prepare(root, local::list_dir(root).unwrap());
        report("local_list_prepare_10000", start);
        assert_eq!(listing.len(), 10_000);
    }

    cx.update(|cx| {
        let list_focus = cx.focus_handle();
        let other_focus = cx.focus_handle();
        let pane = cx.new(|cx| {
            Pane::new(
                Source::Local,
                root.to_owned(),
                ExpansionMemory::default(),
                Rc::new(Cell::new(false)),
                list_focus,
                other_focus,
                cx,
            )
        });
        pane.update(cx, |pane, _| {
            pane.tree.finish(root.into(), Ok(listing));
            let dragged = [root.join("File0")];
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_268_800);
            for _ in 0..5 {
                pane.tree.select(8_000, Selection::Single);
                let start = Instant::now();
                for _ in 0..1_000 {
                    black_box(pane.tree.cursor_index());
                    pane.tree.step(true, Selection::Single);
                }
                report("tree_cursor_step_10000_x1000", start);
                assert_eq!(pane.tree.cursor_index(), Some(9_000));

                let start = Instant::now();
                for index in 5_000..5_500 {
                    black_box(drop_target::resolve(
                        pane.drop_rows().as_ref(),
                        root,
                        Some(index),
                        &dragged,
                        false,
                    ));
                }
                report("drop_resolve_10000_x500", start);

                let start = Instant::now();
                #[allow(clippy::useless_conversion)]
                let today = now.into();
                for row in &pane.tree.rows {
                    black_box(file_metadata::modified(row.entry().unwrap().mtime, today));
                }
                report("modified_10000", start);
            }
        });
    });
    crate::terminal::benchmark_prepaint(cx);
}

fn report(metric: &str, start: Instant) {
    eprintln!(
        "perf {metric} {:.6} ms",
        start.elapsed().as_secs_f64() * 1000.
    );
}
