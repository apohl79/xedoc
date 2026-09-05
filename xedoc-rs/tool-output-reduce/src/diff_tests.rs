use super::diff;

#[test]
fn strips_metadata_and_keeps_every_hunk_header() {
    let mut lines = vec![
        "diff --git a/a b/a",
        "index 123..456 100644",
        "--- a/a",
        "+++ b/a",
        "@@ -1,20 +1,20 @@",
    ];
    lines.extend(std::iter::repeat_n(" unchanged", 12));
    lines.extend(["-old", "+new", "@@ -40,2 +40,2 @@", "-before", "+after"]);
    let input = lines.join("\n");
    let reduced = diff::reduce(&input).expect("large-context diff should reduce");
    assert!(reduced.contains("diff --git a/a b/a"));
    assert!(!reduced.contains("index 123..456"));
    assert!(reduced.contains("@@ -1,20 +1,20 @@"));
    assert!(reduced.contains("@@ -40,2 +40,2 @@"));
}
