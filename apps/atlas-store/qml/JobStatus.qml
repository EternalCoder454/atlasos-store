import QtQuick
import QtQuick.Layouts
import Atlas.Ui

// What the Flatpak worker (src/jobs.rs) is doing and what it last did: a
// progress line with Cancel while a job runs for `forApp` (any app when ""),
// then the error or result in plain words. Every text is plain: the lines
// can carry names from a remote.
ColumnLayout {
    id: root

    required property var jobs
    // Show the running job only when it is for this app ("" for any).
    property string forApp

    readonly property bool running: jobs.phase !== "idle" && (forApp.length === 0 || jobs.appId === forApp || jobs.appId.length === 0)

    // A message is shown on the page of the app it is about (or anywhere when
    // it is about none).
    readonly property bool messageHere: forApp.length === 0 || jobs.messageApp.length === 0 || jobs.messageApp === forApp
    readonly property bool hasMessage: messageHere && (jobs.errorText.length > 0 || jobs.resultText.length > 0)

    visible: running || hasMessage
    spacing: AtlasStyle.spacingSmall

    RowLayout {
        Layout.fillWidth: true
        visible: root.running
        spacing: AtlasStyle.spacingLarge

        ColumnLayout {
            Layout.fillWidth: true
            spacing: AtlasStyle.spacingSmall
            Text {
                Layout.fillWidth: true
                text: root.jobs.status.length > 0 ? root.jobs.status : qsTr("Working…")
                wrapMode: Text.Wrap
                font.family: AtlasStyle.fontFamily
                font.pointSize: AtlasStyle.fontSizeCaption
                color: root.jobs.notResponding ? AtlasStyle.warning : AtlasStyle.textMuted
                textFormat: Text.PlainText
            }
            AtlasProgressBar {
                Layout.fillWidth: true
                indeterminate: root.jobs.percent < 0
                value: Math.max(0, root.jobs.percent) / 100
            }
        }
        // Cancel stays enabled, also when the job is not responding.
        AtlasButton {
            text: qsTr("Cancel")
            enabled: root.jobs.phase === "planning" || root.jobs.phase === "installing" || root.jobs.phase === "removing"
            onClicked: root.jobs.cancel()
        }
    }

    RowLayout {
        Layout.fillWidth: true
        visible: !root.running && root.hasMessage
        spacing: AtlasStyle.spacingLarge
        Text {
            Layout.fillWidth: true
            text: root.jobs.errorText.length > 0 ? root.jobs.errorText : root.jobs.resultText
            wrapMode: Text.Wrap
            font.family: AtlasStyle.fontFamily
            font.pointSize: AtlasStyle.fontSizeBody
            color: root.jobs.errorText.length > 0 ? AtlasStyle.error : AtlasStyle.success
            textFormat: Text.PlainText
        }
        AtlasButton {
            text: qsTr("Dismiss")
            onClicked: root.jobs.clearMessages()
        }
    }
}
