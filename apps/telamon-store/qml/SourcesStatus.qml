import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// What the Sources worker (src/sources.rs) is doing and what it last did: a
// progress line with Cancel while a job runs, then the error or result in
// plain words. Every text is plain: the lines can carry names from a source.
ColumnLayout {
    id: root

    required property var sources

    readonly property bool running: sources.phase !== "idle"
    readonly property bool hasMessage: sources.errorText.length > 0 || sources.resultText.length > 0

    visible: running || hasMessage
    spacing: TelamonStyle.spacingSmall

    RowLayout {
        Layout.fillWidth: true
        visible: root.running
        spacing: TelamonStyle.spacingLarge

        ColumnLayout {
            Layout.fillWidth: true
            spacing: TelamonStyle.spacingSmall
            Text {
                Layout.fillWidth: true
                text: root.sources.status.length > 0 ? root.sources.status : qsTr("Working…")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            TelamonProgressBar {
                Layout.fillWidth: true
                indeterminate: root.sources.percent < 0
                value: Math.max(0, root.sources.percent) / 100
            }
        }
        TelamonButton {
            text: qsTr("Cancel")
            onClicked: root.sources.cancel()
        }
    }

    RowLayout {
        Layout.fillWidth: true
        visible: !root.running && root.hasMessage
        spacing: TelamonStyle.spacingLarge
        Text {
            Layout.fillWidth: true
            text: root.sources.errorText.length > 0 ? root.sources.errorText : root.sources.resultText
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: root.sources.errorText.length > 0 ? TelamonStyle.error : TelamonStyle.success
            textFormat: Text.PlainText
        }
        TelamonButton {
            text: qsTr("Dismiss")
            onClicked: root.sources.clearMessages()
        }
    }
}
