import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// The Remove confirmation for a source, asked after the worker found what is
// installed from it (Sources.checkRemove). Nothing installed from it: it says
// what happens and removes only on Remove (the default button is Cancel; the
// dialog ignores input for its first half second). Apps installed from it:
// it says which, and only offers OK. Every text is plain.
ConfirmDialog {
    id: dlg

    required property var sources
    property var info: ({})
    property bool armed: false

    readonly property bool blocked: dlg.info.blocked === true

    title: dlg.blocked ? qsTr("Source Still in Use") : qsTr("Remove %1?").arg(dlg.info.title ?? "")
    text: dlg.blocked ? (dlg.info.message ?? "") : (dlg.info.scope === "system" ? qsTr("The source will be taken off the list for everyone on this computer. It will ask for your password. Nothing is installed from it, so no apps change. You can add it again later.") : qsTr("The source will be taken off your list. Nothing is installed from it, so no apps change. You can add it again later."))
    acceptText: dlg.blocked ? qsTr("OK") : qsTr("Remove")
    rejectText: qsTr("Cancel")
    showReject: !dlg.blocked
    destructive: !dlg.blocked
    defaultButton: dlg.blocked ? "accept" : "reject"
    focusReject: !dlg.blocked
    closeOnAccept: false

    Timer {
        id: armTimer
        interval: 500
        onTriggered: dlg.armed = true
    }

    Connections {
        target: dlg.sources
        function onRemoveReady() {
            dlg.info = JSON.parse(dlg.sources.removeJson);
            dlg.armed = false;
            armTimer.restart();
            dlg.open();
        }
    }

    onAccepted: {
        if (!dlg.armed) {
            return;
        }
        dlg.close();
        if (!dlg.blocked) {
            dlg.sources.confirmRemove();
        }
    }
    onClosed: {
        armTimer.stop();
        if (!dlg.blocked) {
            // Only a confirmed removal has already taken it (confirmRemove).
            dlg.sources.cancelRemove();
        }
    }

    Text {
        Layout.fillWidth: true
        visible: !dlg.blocked && (dlg.info.note ?? "").length > 0
        text: dlg.info.note ?? ""
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.warning
        textFormat: Text.PlainText
    }
}
