import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// The remove confirmation. "Also Delete App Data" is off by default. The
// default button is Cancel and the dialog ignores input for its first half
// second. When the app is running and its data was to be deleted, the same
// dialog comes back (`showRunning`) and offers "Close and Remove": the app is
// closed on the worker, with no chance to save, so the text says so.
ConfirmDialog {
    id: dlg

    required property var jobs
    property string appId
    property string appName
    property string scope
    property string fullRef
    property bool shared: false
    property bool armed: false
    property bool running: false

    title: dlg.running ? qsTr("%1 Is Running").arg(dlg.appName) : qsTr("Remove %1?").arg(dlg.appName)
    text: dlg.running ? qsTr("To delete its data, the app has to be closed first. Anything unsaved in it is lost.") : qsTr("From the %1 installation. The app will be uninstalled. Runtimes it used stay until you remove unused ones.").arg(dlg.scope)
    acceptText: dlg.running ? qsTr("Close and Remove") : qsTr("Remove")
    rejectText: qsTr("Cancel")
    destructive: true
    defaultButton: "reject"
    focusReject: true
    closeOnAccept: false

    function show(id, name, scope, ref, shared) {
        dlg.appId = id;
        dlg.appName = name;
        dlg.scope = scope;
        dlg.fullRef = ref;
        dlg.shared = shared;
        deleteData.checked = false;
        dlg.running = false;
        dlg.armed = false;
        armTimer.restart();
        dlg.open();
    }

    // The removal stopped because the app runs (Jobs.removeBlocked).
    function showRunning(id, name, scope, ref) {
        dlg.appId = id;
        dlg.appName = name;
        dlg.scope = scope;
        dlg.fullRef = ref;
        dlg.shared = false;
        deleteData.checked = true;
        dlg.running = true;
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
        if (dlg.running) {
            dlg.jobs.closeAndRemove(dlg.appId, dlg.fullRef, dlg.scope);
        } else {
            dlg.jobs.remove(dlg.appId, dlg.fullRef, dlg.scope, deleteData.checked && !dlg.shared);
        }
    }

    TelamonCheckBox {
        id: deleteData
        Layout.fillWidth: true
        visible: !dlg.running
        text: qsTr("Also Delete App Data")
        checked: false
        enabled: !dlg.shared
    }
    Text {
        Layout.fillWidth: true
        visible: dlg.shared
        text: qsTr("The app is installed more than once, and its data folder is shared, so its data is kept.")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        visible: deleteData.checked
        text: qsTr("The app's settings and files for your user will be deleted. This can't be undone.")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.warning
        textFormat: Text.PlainText
    }
}
