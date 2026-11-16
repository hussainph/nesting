//! App icons in the theme's 16 pt stroke style, for the actions the theme catalog doesn't cover.

use rune::Icon;

pub static IMPORT: Icon = Icon {
    name: "nest-import",
    view: 16.0,
    body: r#"<path d="M9.5 1.5H4a1 1 0 0 0-1 1v11a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V5L9.5 1.5Z" stroke="currentColor" stroke-linejoin="round"/><path d="M8 7.5v4M6 9.5h4" stroke="currentColor" stroke-linecap="round"/>"#,
};

pub static PLAY: Icon = Icon {
    name: "nest-play",
    view: 16.0,
    body: r#"<path d="M5 3.5v9l7-4.5-7-4.5Z" stroke="currentColor" stroke-linejoin="round"/>"#,
};

pub static STOP: Icon = Icon {
    name: "nest-stop",
    view: 16.0,
    body: r#"<rect x="4" y="4" width="8" height="8" rx="1" stroke="currentColor"/>"#,
};

pub static EXPORT: Icon = Icon {
    name: "nest-export",
    view: 16.0,
    body: r#"<path d="M8 2v8M5 7l3 3 3-3" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/><path d="M2.5 10.5v2a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-2" stroke="currentColor" stroke-linecap="round"/>"#,
};

pub static FIT: Icon = Icon {
    name: "nest-fit",
    view: 16.0,
    body: r#"<path d="M2.5 6V3.5a1 1 0 0 1 1-1H6M10 2.5h2.5a1 1 0 0 1 1 1V6M13.5 10v2.5a1 1 0 0 1-1 1H10M6 13.5H3.5a1 1 0 0 1-1-1V10" stroke="currentColor" stroke-linecap="round"/>"#,
};

pub static SHEET: Icon = Icon {
    name: "nest-sheet",
    view: 16.0,
    body: r#"<path d="M2.5 3.5h11v9h-11z" stroke="currentColor" stroke-linejoin="round"/>"#,
};

pub static UNDO: Icon = Icon {
    name: "nest-undo",
    view: 16.0,
    body: r#"<path d="M5.5 3.5 2.5 6.5l3 3" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/><path d="M3 6.5h6.5a3.5 3.5 0 0 1 0 7H7" stroke="currentColor" stroke-linecap="round"/>"#,
};

pub static REDO: Icon = Icon {
    name: "nest-redo",
    view: 16.0,
    body: r#"<path d="m10.5 3.5 3 3-3 3" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/><path d="M13 6.5H6.5a3.5 3.5 0 0 0 0 7H9" stroke="currentColor" stroke-linecap="round"/>"#,
};
