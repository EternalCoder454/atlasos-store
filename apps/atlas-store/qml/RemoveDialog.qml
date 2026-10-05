import QtQuick
import QtQuick.Layouts
import Atlas.Ui

// The remove confirmation. "Also Delete App Data" is off by default. The
// default button is Cancel and the dialog ignores input for its first half
// second.
ConfirmDialog {
    id: dlg

    required property var jobs
    property string appId
    property string appName
    property string scope
    property string fullRef
    property bool shared: false
    property bool armed: false

    title: qsTr("Remove %1?").arg(dlg.appName)
    text: qsTr("From the %1 installation. The app will be uninstalled. Runtimes it used stay until you remove unused ones.").arg(dlg.scope)
    acceptText: qsTr("Remove")
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
        dlg.jobs.remove(dlg.appId, dlg.fullRef, deleteData.checked && !dlg.shared);
    }

    AtlasCheckBox {
        id: deleteData
        Layout.fillWidth: true
        text: qsTr("Also Delete App Data")
        checked: false
        enabled: !dlg.shared
    }
    Text {
        Layout.fillWidth: true
        visible: dlg.shared
        text: qsTr("The app is installed more than once, and its data folder is shared, so its data is kept.")
        wrapMode: Text.Wrap
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeCaption
        color: AtlasStyle.textMuted
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        visible: deleteData.checked
        text: qsTr("The app's settings and files for your user will be deleted. This can't be undone.")
        wrapMode: Text.Wrap
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeCaption
        color: AtlasStyle.warning
        textFormat: Text.PlainText
    }
}
