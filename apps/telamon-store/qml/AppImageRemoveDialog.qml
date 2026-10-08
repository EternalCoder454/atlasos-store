import QtQuick
import Telamon.Ui

// The confirmation before an installed AppImage is removed: its copy in the
// Applications folder, its icon and its menu entry. Cancel is the default
// and the dialog ignores input for its first half second.
ConfirmDialog {
    id: dlg

    required property var appImages
    property string appId
    property string appName
    property bool armed: false

    title: qsTr("Remove %1?").arg(dlg.appName)
    text: qsTr("The app's copy in your Applications folder, its icon and its menu entry are deleted. Its settings and the files it made are kept. The file you downloaded is not touched.")
    acceptText: qsTr("Remove")
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
        dlg.appImages.uninstall(dlg.appId);
    }
}
