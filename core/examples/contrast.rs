//! Every ratio a theme is built to, as numbers.
//!
//! ```sh
//! cargo run -q -p gitten-core --example contrast          # every registered theme
//! cargo run -q -p gitten-core --example contrast light    # one of them
//! ```
//!
//! `paint` is for looking at a palette and this is for checking one, and the two
//! answer different questions: a colour can look right and still be 1.6:1 on a
//! background it lands on, which is how the furniture bug in
//! `docs/decisions/0020` survived being looked at for weeks.
//!
//! The tests assert the floors — nothing may be illegible — but a floor is not a
//! *hierarchy*, and the hierarchy is what makes a second theme feel like the
//! first one. A file header has to out-read a hunk header, an absent cell has to
//! read against the row opposite it, and the furniture has to stay quieter than
//! the text it labels. This prints all of it for every theme at once, which is
//! how the shipped light and slate palettes were built: take the dark theme's
//! column as the target and match it hue by hue.
//!
//! Anything below its floor is marked `*`, which for a syntax class is not a
//! failure — it is `readable` doing its job, and the lifted value is what is
//! drawn. The `*` says which colours the theme is *not* really choosing.

use gitten_core::host::Host;
use gitten_core::syntax::Kind;
use gitten_core::theme::{contrast, Surface, Theme};

fn row(label: &str, got: f32, floor: f32) {
    let mark = if got < floor { "*" } else { " " };
    println!("  {label:<26} {got:6.2}{mark}");
}

fn report(t: &Theme) {
    let d = &t.diff;
    let c = &t.chrome;
    let ctx = d.context_bg;
    println!("\n=== {} ===", t.name);

    println!("  -- the rows, against context --");
    for (name, bg) in [
        ("file_bg", d.file_bg),
        ("hunk_bg", d.hunk_bg),
        ("added_bg", d.added_bg),
        ("removed_bg", d.removed_bg),
        ("added_word_bg", d.added_word_bg),
        ("removed_word_bg", d.removed_word_bg),
        ("moved_removed_bg", d.moved_removed_bg),
        ("moved_added_bg", d.moved_added_bg),
        ("absent_bg", d.absent_bg),
    ] {
        row(name, contrast(bg, ctx), 0.0);
    }
    println!("  -- and against what they are read beside --");
    row(
        "added_word vs added",
        contrast(d.added_word_bg, d.added_bg),
        1.10,
    );
    row(
        "removed_word vs removed",
        contrast(d.removed_word_bg, d.removed_bg),
        1.10,
    );
    // The only comparison that decides `absent_bg`: it appears beside a change
    // and nowhere else, so context is not what it has to differ from.
    row("absent vs added", contrast(d.absent_bg, d.added_bg), 1.15);
    row(
        "absent vs removed",
        contrast(d.absent_bg, d.removed_bg),
        1.15,
    );
    row(
        "file vs hunk",
        contrast(d.file_bg, ctx) / contrast(d.hunk_bg, ctx),
        1.0,
    );

    println!("  -- text, on the row it is drawn on --");
    row("context_fg", contrast(d.context_fg, ctx), t.min_contrast);
    row(
        "added_fg on added",
        contrast(d.added_fg, d.added_bg),
        t.min_contrast,
    );
    row(
        "added_fg on moved",
        contrast(d.added_fg, d.moved_added_bg),
        t.min_contrast,
    );
    row(
        "removed_fg on removed",
        contrast(d.removed_fg, d.removed_bg),
        t.min_contrast,
    );
    row(
        "removed_fg on moved",
        contrast(d.removed_fg, d.moved_removed_bg),
        t.min_contrast,
    );
    row(
        "file_fg on file",
        contrast(d.file_fg, d.file_bg),
        t.min_contrast,
    );
    row(
        "adds_fg on file",
        contrast(d.adds_fg, d.file_bg),
        t.min_contrast,
    );
    row(
        "dels_fg on file",
        contrast(d.dels_fg, d.file_bg),
        t.min_contrast,
    );
    row(
        "hunk_fg on hunk",
        contrast(d.hunk_fg, d.hunk_bg),
        t.min_contrast,
    );

    println!("  -- chrome --");
    for (name, fg) in [
        ("fg", c.fg),
        ("dim", c.dim),
        ("faint", c.faint),
        ("accent", c.accent),
        ("error", c.error),
    ] {
        row(&format!("{name} on bg"), contrast(fg, c.bg), 0.0);
    }
    for (name, bg) in [
        ("title_bg", c.title_bg),
        ("status_bg", c.status_bg),
        ("border", c.border),
        ("selection_bg", c.selection_bg),
    ] {
        row(&format!("{name} vs bg"), contrast(bg, c.bg), 0.0);
    }
    // A drag selection lands on all six diff backgrounds, so the one it differs
    // from least is the only number worth printing.
    let worst = [
        d.context_bg,
        d.added_bg,
        d.removed_bg,
        d.added_word_bg,
        d.removed_word_bg,
        d.moved_added_bg,
    ]
    .into_iter()
    .map(|bg| contrast(c.selected_bg, bg))
    .fold(f32::MAX, f32::min);
    row("selected_bg, worst row", worst, 1.05);

    println!("  -- furniture, as written and then resolved per surface --");
    // Below the floor on purpose and marked `*` for it: what a theme *chooses*
    // is this one grey, and the eight below are what `rebuild` made of it.
    row(
        "gutter_fg, unlifted",
        contrast(d.gutter_fg, ctx),
        t.min_furniture,
    );
    for s in Surface::ALL {
        let got = contrast(t.gutter_on(s), t.background(s));
        row(&format!("gutter on {s:?}"), got, t.min_furniture);
    }

    println!("  -- syntax, raw against context, then lifted where it had to be --");
    for kind in Kind::ALL {
        let raw = contrast(t.syntax(kind).fg, ctx);
        let lifted: Vec<String> = Surface::ALL
            .iter()
            .map(|s| {
                let on = contrast(t.syntax_on(kind, *s).fg, t.background(*s));
                let moved = t.syntax_on(kind, *s).fg != t.syntax(kind).fg;
                format!("{on:5.1}{}", if moved { "*" } else { " " })
            })
            .collect();
        println!(
            "  {:<12} {raw:6.2}   {}",
            format!("{kind:?}"),
            lifted.join(" ")
        );
    }
}

fn main() {
    let host = Host::new();
    match std::env::args().nth(1) {
        Some(name) => match host.themes.get(&name) {
            Some(t) => report(t),
            None => eprintln!(
                "contrast: no theme {name:?}; registered: {}",
                host.themes.names().join(", ")
            ),
        },
        None => {
            println!("surfaces, in the order the syntax table below prints them:");
            println!("  {}", Surface::ALL.map(|s| format!("{s:?}")).join("  "));
            for name in host.themes.names() {
                if let Some(t) = host.themes.get(name) {
                    report(t);
                }
            }
        }
    }
}
