import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// Asked before Update All when some waiting updates ask for permissions the
// installed versions don't have: each such app with everything it asks for,
// what Update All downloads, and that every other update installs too.
// "Update Without These" leaves the listed apps as they are. The default
// button is Cancel and the dialog ignores input for its first half second.
// Every text is plain: the permissions came from a remote.
ConfirmDialog {
    id: dlg

    // The apps that ask (rows of the page's list with `review`) and what
    // Update All downloads in all ("" when not known).
    property var asking: []
    property string download
    property bool armed: false

    signal updateAll
    signal updateWithout

    title: dlg.asking.length === 1 ? qsTr("%1 Asks for New Permissions").arg(dlg.asking[0].name) : qsTr("%1 Apps Ask for New Permissions").arg(dlg.asking.length)
    acceptText: qsTr("Update All")
    alternativeText: qsTr("Update Without These")
    rejectText: qsTr("Cancel")
    defaultButton: "reject"
    focusReject: true
    closeOnAccept: false

    function show(apps, download) {
        dlg.asking = apps;
        dlg.download = download;
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
        dlg.updateAll();
    }
    onAlternative: {
        if (!dlg.armed) {
            return;
        }
        dlg.close();
        dlg.updateWithout();
    }

    Text {
        Layout.fillWidth: true
        text: dlg.asking.length === 1 ? qsTr("The new version asks for more than the one installed:") : qsTr("The new versions ask for more than the ones installed:")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }

    Repeater {
        model: dlg.asking
        ColumnLayout {
            id: app
            required property var modelData
            Layout.fillWidth: true
            spacing: TelamonStyle.spacingXSmall

            Text {
                Layout.fillWidth: true
                text: app.modelData.name
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                font.bold: true
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }
            Repeater {
                model: app.modelData.asksList
                Text {
                    required property string modelData
                    Layout.fillWidth: true
                    Layout.leftMargin: TelamonStyle.spacingLarge
                    text: "• " + modelData
                    wrapMode: Text.Wrap
                    font.family: TelamonStyle.fontFamily
                    font.pointSize: TelamonStyle.fontSizeCaption
                    color: TelamonStyle.warning
                    textFormat: Text.PlainText
                }
            }
        }
    }

    Text {
        Layout.fillWidth: true
        text: dlg.download.length > 0 ? qsTr("Update All installs these and every other update that is waiting (%1 to download).").arg(dlg.download) : qsTr("Update All installs these and every other update that is waiting.")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        text: qsTr("Update Without These installs the other updates and leaves these apps as they are.")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }
}
