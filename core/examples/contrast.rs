//! Every ratio a theme is built to, as numbers.
//!
//! ```sh
//! cargo run -q -p gitten-core --example contrast          # every registered theme
//! cargo run -q -p gitten-core --example contrast light    # one of them
//! ```
//!
//! `--json` (or `GITTEN_FORMAT=json`) prints one object to stdout instead of
//! the tables — the schema is `gitten.contrast/1`, documented in
//! `docs/agent-json.md`. An unknown theme in JSON mode is `{error, code,
//! hint}` on stderr with a nonzero exit; in human mode it stays a warning.
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

use gitten_app::env;
use gitten_core::host::Host;
use gitten_core::syntax::Kind;
use gitten_core::theme::{contrast, Surface, Theme};

/// One measured ratio, whatever the mode prints it as.
struct Check {
    label: String,
    ratio: f32,
    floor: f32,
}

/// The report's two audiences: human mode prints each row as it is measured,
/// JSON mode records it for the object at the end. One call path, so the
/// numbers cannot disagree with the tables.
struct Rep {
    json: bool,
    checks: Vec<Check>,
}

impl Rep {
    fn header(&self, name: &str) {
        if !self.json {
            println!("\n=== {name} ===");
        }
    }

    fn section(&self, title: &str) {
        if !self.json {
            println!("  {title}");
        }
    }

    fn push(&mut self, label: String, ratio: f32, floor: f32) {
        self.checks.push(Check {
            label,
            ratio,
            floor,
        });
    }

    fn row(&mut self, label: &str, got: f32, floor: f32) {
        if !self.json {
            let mark = if got < floor { "*" } else { " " };
            println!("  {label:<26} {got:6.2}{mark}");
        }
        self.push(label.to_string(), got, floor);
    }
}

fn report(t: &Theme, rep: &mut Rep) {
    let d = &t.diff;
    let c = &t.chrome;
    let ctx = d.context_bg;
    rep.header(&t.name);

    rep.section("-- the rows, against context --");
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
        rep.row(name, contrast(bg, ctx), 0.0);
    }
    rep.section("-- and against what they are read beside --");
    rep.row(
        "added_word vs added",
        contrast(d.added_word_bg, d.added_bg),
        1.10,
    );
    rep.row(
        "removed_word vs removed",
        contrast(d.removed_word_bg, d.removed_bg),
        1.10,
    );
    // The only comparison that decides `absent_bg`: it appears beside a change
    // and nowhere else, so context is not what it has to differ from.
    rep.row("absent vs added", contrast(d.absent_bg, d.added_bg), 1.15);
    rep.row(
        "absent vs removed",
        contrast(d.absent_bg, d.removed_bg),
        1.15,
    );
    rep.row(
        "file vs hunk",
        contrast(d.file_bg, ctx) / contrast(d.hunk_bg, ctx),
        1.0,
    );

    rep.section("-- text, on the row it is drawn on --");
    rep.row("context_fg", contrast(d.context_fg, ctx), t.min_contrast);
    rep.row(
        "added_fg on added",
        contrast(d.added_fg, d.added_bg),
        t.min_contrast,
    );
    rep.row(
        "added_fg on moved",
        contrast(d.added_fg, d.moved_added_bg),
        t.min_contrast,
    );
    rep.row(
        "removed_fg on removed",
        contrast(d.removed_fg, d.removed_bg),
        t.min_contrast,
    );
    rep.row(
        "removed_fg on moved",
        contrast(d.removed_fg, d.moved_removed_bg),
        t.min_contrast,
    );
    rep.row(
        "file_fg on file",
        contrast(d.file_fg, d.file_bg),
        t.min_contrast,
    );
    rep.row(
        "adds_fg on file",
        contrast(d.adds_fg, d.file_bg),
        t.min_contrast,
    );
    rep.row(
        "dels_fg on file",
        contrast(d.dels_fg, d.file_bg),
        t.min_contrast,
    );
    rep.row(
        "hunk_fg on hunk",
        contrast(d.hunk_fg, d.hunk_bg),
        t.min_contrast,
    );

    rep.section("-- chrome: text inks on the strips they are drawn on --");
    for (name, fg) in [("fg", c.fg), ("accent", c.accent), ("error", c.error)] {
        for (bg_name, bg) in [
            ("bg", c.bg),
            ("title_bg", c.title_bg),
            ("status_bg", c.status_bg),
            ("selection_bg", c.selection_bg),
        ] {
            rep.row(
                &format!("{name} on {bg_name}"),
                contrast(fg, bg),
                t.min_contrast,
            );
        }
    }
    // `dim` is resolved per surface, for the reason the strips are surfaces:
    // raw, it clears the text floor on `bg` alone. What is drawn is the table,
    // so the table is what is measured — with the raw value beside it, `*`ed
    // the same way the gutter's unlifted row is.
    rep.row("dim, unlifted", contrast(c.dim, c.bg), t.min_contrast);
    for s in Surface::ALL {
        let got = contrast(t.dim_on(s), t.background(s));
        rep.row(&format!("dim on {s:?}"), got, t.min_contrast);
    }

    rep.section("-- quiet, resolved against the background it sits on --");
    for (name, bg) in [
        ("bg", c.bg),
        ("title_bg", c.title_bg),
        ("status_bg", c.status_bg),
    ] {
        rep.row(
            &format!("quiet on {name}"),
            contrast(t.quiet_on(bg), bg),
            t.min_furniture,
        );
    }
    for (name, bg) in [
        ("title_bg", c.title_bg),
        ("status_bg", c.status_bg),
        ("border", c.border),
        ("raised", c.raised),
        ("keycap", c.keycap),
        ("selection_bg", c.selection_bg),
    ] {
        rep.row(&format!("{name} vs bg"), contrast(bg, c.bg), 0.0);
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
    rep.row("selected_bg, worst row", worst, 1.05);

    rep.section("-- markdown --");
    // Below the floor raw, and marked for it: a bullet is furniture drawn on
    // whichever row the prose landed on, and the lifted value is what is drawn.
    rep.row(
        "marker, unlifted",
        contrast(t.markdown.marker, ctx),
        t.min_furniture,
    );
    for s in Surface::ALL {
        let got = contrast(t.marker_on(s), t.background(s));
        rep.row(&format!("marker on {s:?}"), got, t.min_furniture);
    }
    for (name, v) in [
        ("code_bar", t.markdown.code_bar),
        ("quote_bar", t.markdown.quote_bar),
        ("rule", t.markdown.rule),
    ] {
        rep.row(&format!("{name} vs context"), contrast(v, ctx), 0.0);
    }

    rep.section("-- graph --");
    for (i, lane) in t.lanes.iter().enumerate() {
        rep.row(&format!("lane {i}"), contrast(*lane, c.bg), t.min_furniture);
    }
    // `lane_overflow` is a stroke with no legibility floor by decision 0020 —
    // its dimness is the point — so it is measured for the record, not gated.
    rep.row(
        "lane_overflow, exempt (0020)",
        contrast(t.lane_overflow, c.bg),
        0.0,
    );

    rep.section("-- furniture, as written and then resolved per surface --");
    // Below the floor on purpose and marked `*` for it: what a theme *chooses*
    // is this one grey, and the eight below are what `rebuild` made of it.
    rep.row(
        "gutter_fg, unlifted",
        contrast(d.gutter_fg, ctx),
        t.min_furniture,
    );
    for s in Surface::ALL {
        let got = contrast(t.gutter_on(s), t.background(s));
        rep.row(&format!("gutter on {s:?}"), got, t.min_furniture);
    }

    rep.section("-- syntax, raw against context, then lifted where it had to be --");
    for kind in Kind::ALL {
        let raw = contrast(t.syntax(kind).fg, ctx);
        if !rep.json {
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
        rep.push(format!("{kind:?} raw"), raw, 0.0);
        for s in Surface::ALL {
            let on = contrast(t.syntax_on(kind, s).fg, t.background(s));
            let moved = t.syntax_on(kind, s).fg != t.syntax(kind).fg;
            rep.push(
                format!("{kind:?} on {s:?}{}", if moved { " (lifted)" } else { "" }),
                on,
                t.min_contrast,
            );
        }
    }
}

fn jstr(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn key(out: &mut String, first: &mut bool, k: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    jstr(out, k);
    out.push(':');
}

fn sfield(out: &mut String, first: &mut bool, k: &str, v: &str) {
    key(out, first, k);
    jstr(out, v);
}

fn nfield(out: &mut String, first: &mut bool, k: &str, v: impl std::fmt::Display) {
    key(out, first, k);
    out.push_str(&v.to_string());
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json = env::wants_json(&raw);
    let args = env::strip_json_arg(&raw);
    let host = Host::new();

    // One theme or every registered one. Unknown is a warning in human mode —
    // the historical behaviour — and a failure in JSON mode, where silence
    // would read as an empty answer.
    let names: Vec<String> = match args.first() {
        Some(name) => match host.themes.get(name) {
            Some(_) => vec![name.clone()],
            None => {
                let have = host.themes.names().join(", ");
                if json {
                    let mut out = String::from("{");
                    jstr(&mut out, "error");
                    out.push(':');
                    jstr(&mut out, &format!("no theme {name:?}"));
                    out.push(',');
                    jstr(&mut out, "code");
                    out.push(':');
                    jstr(&mut out, "usage");
                    out.push(',');
                    jstr(&mut out, "hint");
                    out.push(':');
                    jstr(&mut out, &format!("registered: {have}"));
                    out.push('}');
                    eprintln!("{out}");
                    std::process::exit(1);
                }
                eprintln!("contrast: no theme {name:?}; registered: {have}");
                return;
            }
        },
        None => host.themes.names().iter().map(|s| s.to_string()).collect(),
    };

    if !json {
        // The header belongs to the all-themes table: a filtered run never
        // printed it.
        if args.is_empty() {
            println!("surfaces, in the order the syntax table below prints them:");
            println!("  {}", Surface::ALL.map(|s| format!("{s:?}")).join("  "));
        }
        for name in &names {
            if let Some(t) = host.themes.get(name) {
                report(
                    t,
                    &mut Rep {
                        json,
                        checks: Vec::new(),
                    },
                );
            }
        }
        return;
    }

    let mut out = String::from("{");
    let mut first = true;
    sfield(&mut out, &mut first, "schema", "gitten.contrast/1");
    sfield(&mut out, &mut first, "tool", "contrast");
    nfield(&mut out, &mut first, "version", 1);
    key(&mut out, &mut first, "themes");
    out.push('[');
    let mut tfirst = true;
    for name in &names {
        let Some(t) = host.themes.get(name) else {
            continue;
        };
        let mut rep = Rep {
            json,
            checks: Vec::new(),
        };
        report(t, &mut rep);
        if !tfirst {
            out.push(',');
        }
        tfirst = false;
        out.push('{');
        let mut efirst = true;
        sfield(&mut out, &mut efirst, "name", &t.name);
        nfield(
            &mut out,
            &mut efirst,
            "minContrast",
            format!("{:.3}", t.min_contrast),
        );
        nfield(
            &mut out,
            &mut efirst,
            "minFurniture",
            format!("{:.3}", t.min_furniture),
        );
        key(&mut out, &mut efirst, "checks");
        out.push('[');
        let mut cfirst = true;
        for c in &rep.checks {
            if !cfirst {
                out.push(',');
            }
            cfirst = false;
            out.push('{');
            let mut ifirst = true;
            sfield(&mut out, &mut ifirst, "label", &c.label);
            nfield(&mut out, &mut ifirst, "ratio", format!("{:.4}", c.ratio));
            nfield(&mut out, &mut ifirst, "floor", format!("{:.3}", c.floor));
            key(&mut out, &mut ifirst, "pass");
            out.push_str(if c.ratio >= c.floor { "true" } else { "false" });
            out.push('}');
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    println!("{out}");
}
