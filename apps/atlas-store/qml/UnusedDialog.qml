import QtQuick
import QtQuick.Layouts
import Atlas.Ui

// The list of runtimes nothing uses any more, shown before they are removed.
// The default button is Cancel and the dialog ignores input for its first
// half second.
ConfirmDialog {
    id: dlg

    required property var jobs
    property var items: []
    property bool armed: false

    title: qsTr("Remove Unused Runtimes?")
    text: qsTr("No installed app needs these any more.")
    acceptText: qsTr("Remove Unused")
    rejectText: qsTr("Not Now")
    destructive: true
    defaultButton: "reject"
    focusReject: true
    closeOnAccept: false

    function show() {
        dlg.items = JSON.parse(dlg.jobs.unusedJson);
        dlg.armed = false;
        armTimer.restart();
        dlg.open();
    }

    Timer {
        id: armTimer
        interval: 500
        onTriggered: dlg.armed = true
    }

    onAccepted: {
        if (!dlg.armed) {
            return;
        }
        dlg.close();
        dlg.jobs.removeUnused();
    }

    Repeater {
        model: dlg.items
        Text {
            required property var modelData
            Layout.fillWidth: true
            text: qsTr("%1 (%2) · %3 · %4").arg(modelData.name).arg(modelData.branch).arg(modelData.scope).arg(modelData.size)
            wrapMode: Text.Wrap
            font.family: AtlasStyle.fontFamily
            font.pointSize: AtlasStyle.fontSizeCaption
            color: AtlasStyle.text
            textFormat: Text.PlainText
        }
    }
}
