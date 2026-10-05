import QtQuick
import QtQuick.Layouts
import Atlas.Ui

// The install confirmation: what will be installed (the app's ref, its
// source, the sizes, each runtime it brings and where that comes from) and
// the permissions it asks for, each with its risk. Nothing is installed
// before Install is pressed. The default button is Cancel and the dialog
// ignores input for its first half second. Every text is plain.
ConfirmDialog {
    id: dlg

    required property var jobs
    property var plan: ({})
    property bool armed: false
    property bool answered: false

    title: qsTr("Install %1?").arg(dlg.plan.name ?? "")
    acceptText: qsTr("Install")
    rejectText: qsTr("Cancel")
    defaultButton: "reject"
    focusReject: true
    closeOnAccept: false

    function show() {
        dlg.plan = JSON.parse(dlg.jobs.planJson);
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
        dlg.jobs.confirmInstall();
    }
    // Closed any other way: the plan is dropped.
    onClosed: {
        if (!dlg.answered) {
            dlg.jobs.cancel();
        }
    }

    Text {
        Layout.fillWidth: true
        text: qsTr("From %1 (%2 installation)\n%3").arg(dlg.plan.remote ?? "").arg(dlg.plan.scope ?? "").arg(dlg.plan.ref ?? "")
        wrapMode: Text.Wrap
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeCaption
        color: AtlasStyle.textMuted
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        visible: dlg.plan.signed === false
        text: qsTr("This source does not sign its apps, so they can't be checked.")
        wrapMode: Text.Wrap
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeCaption
        color: AtlasStyle.warning
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        text: qsTr("Download: %1\nSpace needed: %2").arg(dlg.plan.download ?? "").arg(dlg.plan.installed ?? "")
        wrapMode: Text.Wrap
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeBody
        color: AtlasStyle.text
        textFormat: Text.PlainText
    }

    Text {
        visible: (dlg.plan.runtimes ?? []).length > 0
        text: qsTr("Also Installs")
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeBody
        font.bold: true
        color: AtlasStyle.text
        textFormat: Text.PlainText
    }
    Repeater {
        model: dlg.plan.runtimes ?? []
        Text {
            required property var modelData
            Layout.fillWidth: true
            text: qsTr("%1\nfrom %2 · %3 download%4").arg(modelData.name).arg(modelData.remote).arg(modelData.download).arg(modelData.signed ? "" : qsTr(" · unsigned source"))
            wrapMode: Text.Wrap
            font.family: AtlasStyle.fontFamily
            font.pointSize: AtlasStyle.fontSizeCaption
            color: modelData.signed ? AtlasStyle.textMuted : AtlasStyle.warning
            textFormat: Text.PlainText
        }
    }

    Text {
        text: qsTr("Permissions")
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeBody
        font.bold: true
        color: AtlasStyle.text
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        visible: (dlg.plan.permissions ?? []).length === 0
        text: qsTr("This app asks for no extra permissions.")
        wrapMode: Text.Wrap
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeCaption
        color: AtlasStyle.textMuted
        textFormat: Text.PlainText
    }
    Repeater {
        model: dlg.plan.permissions ?? []
        RowLayout {
            id: row
            required property var modelData
            Layout.fillWidth: true
            spacing: AtlasStyle.spacingLarge
            // The badge is its own item: no text can paint over it.
            Item {
                Layout.alignment: Qt.AlignTop
                Layout.preferredWidth: badge.implicitWidth
                Layout.preferredHeight: badge.implicitHeight
                AtlasBadge {
                    id: badge
                    text: row.modelData.risk === "high" ? qsTr("High") : row.modelData.risk === "medium" ? qsTr("Medium") : qsTr("Low")
                    type: row.modelData.risk === "high" ? "error" : row.modelData.risk === "medium" ? "warning" : "neutral"
                }
            }
            Text {
                Layout.fillWidth: true
                text: row.modelData.text
                wrapMode: Text.Wrap
                clip: true
                font.family: AtlasStyle.fontFamily
                font.pointSize: AtlasStyle.fontSizeCaption
                color: AtlasStyle.text
                textFormat: Text.PlainText
            }
        }
    }
}
