import QtQuick
import QtQuick.Layouts
import Telamon.Ui

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
                text: root.jobs.status.length > 0 ? root.jobs.status : qsTr("Working…")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: root.jobs.notResponding ? TelamonStyle.warning : TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            TelamonProgressBar {
                Layout.fillWidth: true
                indeterminate: root.jobs.percent < 0
                value: Math.max(0, root.jobs.percent) / 100
            }
        }
        // Cancel stays enabled, also when the job is not responding.
        TelamonButton {
            text: qsTr("Cancel")
            enabled: root.jobs.phase === "planning" || root.jobs.phase === "installing" || root.jobs.phase === "removing"
            onClicked: root.jobs.cancel()
        }
    }

    RowLayout {
        Layout.fillWidth: true
        visible: !root.running && root.hasMessage
        spacing: TelamonStyle.spacingLarge
        Text {
            Layout.fillWidth: true
            text: root.jobs.errorText.length > 0 ? root.jobs.errorText : root.jobs.resultText
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: root.jobs.errorText.length > 0 ? TelamonStyle.error : TelamonStyle.success
            textFormat: Text.PlainText
        }
        TelamonButton {
            text: qsTr("Dismiss")
            onClicked: root.jobs.clearMessages()
        }
    }
}
