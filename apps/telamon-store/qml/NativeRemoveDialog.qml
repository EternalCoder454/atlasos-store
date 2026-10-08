import QtQuick
import Telamon.Ui

// The confirmation before a Telamon app is uninstalled. Cancel is the default
// and the dialog ignores input for its first half second.
ConfirmDialog {
    id: dlg

    required property var nativeApps
    property string appId
    property string appName
    property bool armed: false

    title: qsTr("Uninstall %1?").arg(dlg.appName)
    text: qsTr("The app and its menu entry are deleted. Its settings and the files it made are kept.")
    acceptText: qsTr("Uninstall")
    rejectText: qsTr("Cancel")
    destructive: true
    defaultButton: "reject"
    focusReject: true
    closeOnAccept: false

    function show(id, name) {
        dlg.appId = id;
        dlg.appName = name;
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
        dlg.nativeApps.uninstall(dlg.appId);
    }
}
