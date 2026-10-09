//! The Store's QML shows text from catalogs, remotes, files and launches, so
//! it must never turn that text into markup, open links by itself, build code
//! from data or load an image from anywhere but a local file.
//! `scripts/check-qml-plaintext.sh` holds the rules (and CI can run it
//! alone); these tests run it on the real QML and on files that break each
//! rule. They are skipped, with a note, where `bash` or `python3` is missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Runs the script on `target`; `None` when it cannot run here.
fn run(target: &Path) -> Option<(i32, String)> {
    if !have("bash") || !have("python3") {
        eprintln!("skipped: bash and python3 are needed to run the QML check");
        return None;
    }
    let out = Command::new("bash")
        .arg(repo_root().join("scripts/check-qml-plaintext.sh"))
        .arg(target)
        .output()
        .expect("bash runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Some((out.status.code().unwrap_or(-1), text))
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("qml-plaintext-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_stores_qml_is_plain_text_only() {
    let Some((code, text)) = run(&repo_root().join("apps/telamon-store/qml")) else {
        return;
    };
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("clean"), "{text}");
}

#[test]
fn every_rule_catches_what_it_is_for() {
    let dir = scratch("rules");
    let cases: &[(&str, &str, &str)] = &[
        (
            "a Text without a text format",
            "Item { Text { text: model.name } }",
            "without textFormat",
        ),
        (
            "a Label that says AutoText",
            "Item { QQC2.Label { text: x; textFormat: Text.AutoText } }",
            "AutoText",
        ),
        (
            "rich text",
            "Item { Text { text: x; textFormat: Text.RichText } }",
            "RichText",
        ),
        (
            "styled text in a TextEdit",
            "Item { TextEdit { textFormat: TextEdit.RichText } }",
            "RichText",
        ),
        (
            "a link opened from QML",
            "Item { MouseArea { onClicked: Qt.openUrlExternally(url) } }",
            "openUrlExternally",
        ),
        (
            "code built from a string",
            "Item { Component.onCompleted: Qt.createQmlObject(code, this) }",
            "createQmlObject",
        ),
        (
            "eval",
            "Item { Component.onCompleted: eval(code) }",
            "eval(",
        ),
        (
            "a network request",
            "Item { Component.onCompleted: { const r = new XMLHttpRequest(); } }",
            "XMLHttpRequest",
        ),
        (
            "an image from the web",
            "Item { Image { source: \"https://example.org/a.png\" } }",
            "Image source",
        ),
        (
            "an image from data",
            "Item { Image { source: page.info.remoteIcon } }",
            "Image source",
        ),
        (
            "a loader from data",
            "Item { Loader { source: page.url } }",
            "Loader source",
        ),
        (
            "a Heading without a text format",
            "Item { Kirigami.Heading { text: model.name } }",
            "Heading without textFormat",
        ),
        (
            "Controls' own ToolTip",
            "Item { QQC2.ToolTip { text: model.name } }",
            "TelamonToolTip",
        ),
        (
            "a page title from the catalog, as the header shows it",
            "TelamonPage { title: found ? info.name : qsTr(\"App\") }",
            "headerTitle",
        ),
    ];
    for (i, (what, body, expect)) in cases.iter().enumerate() {
        let file = dir.join(format!("case{i}.qml"));
        fs::write(&file, format!("import QtQuick\n{body}\n")).unwrap();
        let Some((code, text)) = run(&file) else {
            return;
        };
        assert_eq!(code, 1, "{what}: {text}");
        assert!(text.contains(expect), "{what}: {text}");
    }
}

#[test]
fn what_is_allowed_passes() {
    let dir = scratch("allowed");
    let file = dir.join("ok.qml");
    fs::write(
        &file,
        r#"import QtQuick
Item {
    // Text { textFormat: Text.RichText } in a comment is nothing
    Text { text: "a"; textFormat: Text.PlainText }
    QQC2.Label {
        text: model.name
        textFormat: Text.PlainText
    }
    TextEdit { readOnly: true; textFormat: TextEdit.PlainText }
    Image { source: info.iconSource ?? "" }
    Image { source: modelData.iconSource }
    Image { source: "file:///usr/share/icons/a.png" }
    Image { source: "qrc:/a.png" }
    Loader { source: "Other.qml" }
    TelamonPage { title: found ? headerTitle(info.name) : qsTr("App") }
    TelamonPage { title: qsTr("Installed") }
}
"#,
    )
    .unwrap();
    let Some((code, text)) = run(&file) else {
        return;
    };
    assert_eq!(code, 0, "{text}");
}
