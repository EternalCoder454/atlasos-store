import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// The confirmation before a Telamon app is installed or updated, in two
// forms. From the list of Telamon apps (`show`): what will be installed, from
// which repository, how big, that it is for this user only. From a file the
// user opened (`showLocal`, src/native.rs looked inside it first): the same,
// and, first, that Telamon's list does not vouch for it. Neither says the app
// is safe: it runs as the user, as any program does. Cancel is the default;
// the dialog ignores input for its first half second. Every text is plain.
ConfirmDialog {
    id: dlg

    required property var nativeApps
    property var app: ({})
    property var detail: ({})
    property bool local: false
    property bool armed: false
    property bool answered: false

    readonly property string name: dlg.local ? (dlg.detail.name ?? "") : (dlg.app.name ?? "")
    readonly property bool update: dlg.local ? dlg.detail.replaces === true : dlg.app.state === "update"

    width: Math.min(parent ? Math.max(0, parent.width - Kirigami.Units.gridUnit * 2) : 0, Kirigami.Units.gridUnit * 30)
    title: dlg.update ? qsTr("Update %1?").arg(dlg.name) : qsTr("Install %1?").arg(dlg.name)
    acceptText: dlg.update ? qsTr("Update") : qsTr("Install")
    rejectText: qsTr("Cancel")
    defaultButton: "reject"
    focusReject: true
    destructive: dlg.local
    closeOnAccept: false

    function show(app) {
        dlg.local = false;
        dlg.app = app;
        dlg.arm();
    }

    function showLocal() {
        dlg.local = true;
        dlg.detail = JSON.parse(dlg.nativeApps.detailJson);
        dlg.arm();
    }

    function arm() {
        dlg.armed = false;
        dlg.answered = false;
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
        dlg.answered = true;
        dlg.close();
        if (dlg.local) {
            dlg.nativeApps.confirmLocal();
        } else {
            dlg.nativeApps.install(dlg.app.id, dlg.app.availableVersion);
        }
    }
    // Closed any other way: a file that was looked at is dropped.
    onClosed: {
        if (dlg.local && !dlg.answered) {
            dlg.nativeApps.cancelLocal();
        }
    }

    // A file's own warning comes first.
    Rectangle {
        Layout.fillWidth: true
        visible: dlg.local
        implicitHeight: warning.implicitHeight + TelamonStyle.spacingLarge * 2
        radius: TelamonStyle.radius
        color: Qt.alpha(TelamonStyle.error, 0.10)
        border.width: 1
        border.color: Qt.alpha(TelamonStyle.error, 0.7)
        Text {
            id: warning
            anchors.fill: parent
            anchors.margins: TelamonStyle.spacingLarge
            text: qsTr("This file did not come from Telamon's list of apps, and Telamon has not checked it. Install it only if you made it or trust whoever did.")
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            font.bold: true
            color: TelamonStyle.error
            textFormat: Text.PlainText
        }
    }

    Text {
        Layout.fillWidth: true
        text: dlg.name
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        font.bold: true
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        text: {
            if (dlg.local) {
                const old = dlg.detail.installedVersion ?? "";
                return (dlg.detail.replaces === true && old.length > 0 ? qsTr("Version %1 replaces %2").arg(dlg.detail.version ?? "").arg(old) : qsTr("Version %1").arg(dlg.detail.version ?? "")) + " · " + (dlg.detail.size ?? "");
            }
            const from = dlg.app.installedVersion ?? "";
            const v = from.length > 0 ? qsTr("Version %1, you have %2").arg(dlg.app.availableVersion ?? "").arg(from) : qsTr("Version %1").arg(dlg.app.availableVersion ?? "");
            return (dlg.app.size ?? "").length > 0 ? qsTr("%1 · %2 to download").arg(v).arg(dlg.app.size) : v;
        }
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        visible: text.length > 0
        text: dlg.local ? (dlg.detail.summary ?? "") : (dlg.app.summary ?? "")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }

    Text {
        text: qsTr("What Happens")
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        font.bold: true
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        text: {
            const where = qsTr("It is installed for you only, in your own folder (.local/share/telamon-apps), with a menu entry. Nothing else on this computer changes.");
            if (dlg.local) {
                return qsTr("The file %1 is unpacked and checked against its own list of files. It has no signature.").arg(dlg.detail.fileName ?? "") + " " + where;
            }
            return qsTr("The Store downloads it from github.com/%1 and checks it against the checksum published with the release before installing. The checksum comes from the same place as the file, so it catches damage, not a hijacked project.").arg(dlg.app.repo ?? "") + " " + where;
        }
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }

    Text {
        text: qsTr("What It Can Access")
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        font.bold: true
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        text: qsTr("This app isn't sandboxed. Like any program you run, it can read and change your files, use the network and see what you can see.")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }

    Text {
        Layout.fillWidth: true
        visible: dlg.local && (dlg.detail.problem ?? "").length > 0
        text: dlg.detail.problem ?? ""
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        font.bold: true
        color: TelamonStyle.warning
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        visible: dlg.local
        text: qsTr("SHA-256: %1").arg(dlg.detail.sha256 ?? "")
        wrapMode: Text.WrapAnywhere
        font.family: "monospace"
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }
}
