//! The same three font categories offered by the reference meter.

pub const OPTIONS: [(&str, &str); 3] = [
    ("system", "字体：系统无衬线"),
    ("serif", "字体：衬线"),
    ("mono", "字体：等宽"),
];

pub fn key(value: &str) -> &'static str {
    match value {
        "serif" => "serif",
        "mono" => "mono",
        _ => "system",
    }
}

pub fn native_family(value: &str) -> &'static str {
    match key(value) {
        "serif" => "Georgia",
        "mono" => "Consolas",
        _ => "Segoe UI",
    }
}
