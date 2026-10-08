pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import QtQuick.Dialogs
import org.kde.kirigami as Kirigami
import Telamon.Ui

// Add Source, in two steps. Step 1: a link to a .flatpakrepo, or a file from
// this computer; the Store reads it (src/sources.rs) and nothing is added.
// Step 2 is the confirmation, before anything is added: the warning, the
// source's name, address and key fingerprint (or a prominent warning that it
// is not signed, which needs its own tick), and who the source is for. The
// default button is Cancel; Add Source stays off for the first half second of
// the confirmation, so a stray key press cannot add anything. Every text is
// plain: the title, address and key come from a file or a server.
TelamonDialog {
    id: dlg

    required property var sources

    // 1 (where from) or 2 (the confirmation).
    property int step: 1
    // The confirmation: Sources.previewJson.
    property var preview: ({})
    // The half second after the confirmation appeared has passed.
    property bool armed: false
    // The scope chosen: "user" or "system".
    property string scope: "user"

    readonly property bool busy: dlg.sources.phase !== "idle"
    readonly property var chosen: dlg.scope === "system" ? (dlg.preview.system ?? ({})) : (dlg.preview.user ?? ({}))
    readonly property bool canAdd: dlg.step === 2 && dlg.armed && !dlg.busy && dlg.chosen.state === "free" && (!dlg.preview.unsigned || acknowledge.checked)

    title: dlg.step === 1 ? qsTr("Add Source") : qsTr("Add %1?").arg(dlg.preview.title ?? "")
    preferredWidth: Kirigami.Units.gridUnit * 34

    // Opens at step 1.
    function show() {
        dlg.reset();
        dlg.open();
    }

    // Opens with a source file from a launch (a .flatpakrepo the user opened).
    function showFile(path) {
        dlg.reset();
        dlg.open();
        dlg.sources.prepareAddFromFile(path);
    }

    function reset() {
        dlg.step = 1;
        dlg.preview = {};
        dlg.armed = false;
        dlg.scope = "user";
        urlField.text = "";
        acknowledge.checked = false;
        dlg.sources.cancelAdd();
    }

    function lookUp() {
        if (urlField.text.trim().length > 0 && !dlg.busy) {
            dlg.sources.prepareAddFromUrl(urlField.text.trim());
        }
    }

    Timer {
        id: armTimer
        interval: 500
        onTriggered: dlg.armed = true
    }

    Connections {
        target: dlg.sources
        function onPreviewReady() {
            if (!dlg.visible) {
                return;
            }
            dlg.preview = JSON.parse(dlg.sources.previewJson);
            dlg.scope = dlg.preview.user.state === "free" ? "user" : (dlg.preview.system.state === "free" ? "system" : "user");
            acknowledge.checked = false;
            dlg.armed = false;
            dlg.step = 2;
            armTimer.restart();
        }
        function onAdded() {
            dlg.close();
        }
    }

    // Closed in any way: what was prepared is dropped, and a download or an
    // add that still runs is stopped.
    onClosed: {
        armTimer.stop();
        dlg.sources.cancelAdd();
    }

    footerContent: [
        TelamonButton {
            text: qsTr("Cancel")
            variant: TelamonButton.Prominent
            onClicked: dlg.close()
        },
        TelamonButton {
            visible: dlg.step === 1
            text: qsTr("Continue")
            enabled: urlField.text.trim().length > 0 && !dlg.busy
            onClicked: dlg.lookUp()
        },
        TelamonButton {
            visible: dlg.step === 2
            text: qsTr("Add Source")
            busy: dlg.busy
            enabled: dlg.canAdd
            onClicked: dlg.sources.confirmAdd(dlg.scope, acknowledge.checked)
        }
    ]

    // ---- step 1 ----

    ColumnLayout {
        Layout.fillWidth: true
        visible: dlg.step === 1
        spacing: TelamonStyle.spacingLarge

        Text {
            Layout.fillWidth: true
            text: qsTr("Paste the link to a source file (.flatpakrepo), or choose one from this computer. You will see what it is before anything is added.")
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: TelamonStyle.text
            textFormat: Text.PlainText
        }
        TelamonTextField {
            id: urlField
            Layout.fillWidth: true
            placeholderText: qsTr("https://example.org/apps.flatpakrepo")
            clearable: true
            enabled: !dlg.busy
            Accessible.name: qsTr("Link to a source file")
            onAccepted: dlg.lookUp()
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: TelamonStyle.spacingLarge
            Text {
                text: qsTr("or")
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            TelamonButton {
                text: qsTr("Choose File…")
                symbol: Symbols.FileOpen
                enabled: !dlg.busy
                onClicked: fileDialog.open()
            }
            Item {
                Layout.fillWidth: true
            }
        }
        RowLayout {
            Layout.fillWidth: true
            visible: dlg.busy
            spacing: TelamonStyle.spacingLarge
            TelamonSpinner {
                running: dlg.busy
                Layout.preferredWidth: 20
                Layout.preferredHeight: 20
            }
            Text {
                Layout.fillWidth: true
                text: dlg.sources.status.length > 0 ? dlg.sources.status : qsTr("Working…")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
        }
    }

    FileDialog {
        id: fileDialog
        title: qsTr("Choose a Source File")
        nameFilters: [qsTr("Flatpak sources (*.flatpakrepo)"), qsTr("All files (*)")]
        onAccepted: dlg.sources.prepareAddFromFile(fileDialog.selectedFile.toString())
    }

    // ---- step 2 ----

    ColumnLayout {
        Layout.fillWidth: true
        visible: dlg.step === 2
        spacing: TelamonStyle.spacingLarge

        Text {
            Layout.fillWidth: true
            text: qsTr("Only add sources you trust. Apps from a source run with your user's access to your files and can't be checked by the Store.")
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            font.bold: true
            color: TelamonStyle.warning
            textFormat: Text.PlainText
        }

        // What the file says, one labelled line each. Selectable, so the
        // address and the fingerprint can be compared with another place.
        GridLayout {
            Layout.fillWidth: true
            columns: 2
            columnSpacing: TelamonStyle.spacingLarge
            rowSpacing: TelamonStyle.spacingSmall

            Text {
                text: qsTr("Name")
                Layout.alignment: Qt.AlignTop
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            TextEdit {
                Layout.fillWidth: true
                text: dlg.preview.title ?? ""
                readOnly: true
                selectByMouse: true
                wrapMode: TextEdit.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                font.bold: true
                color: TelamonStyle.text
                textFormat: TextEdit.PlainText
                Accessible.name: qsTr("Name")
                Accessible.readOnly: true
            }

            Text {
                text: qsTr("Address")
                Layout.alignment: Qt.AlignTop
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            TextEdit {
                Layout.fillWidth: true
                text: dlg.preview.url ?? ""
                readOnly: true
                selectByMouse: true
                wrapMode: TextEdit.WrapAnywhere
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                color: TelamonStyle.text
                textFormat: TextEdit.PlainText
                Accessible.name: qsTr("Address")
                Accessible.readOnly: true
            }

            Text {
                visible: (dlg.preview.comment ?? "").length > 0
                text: qsTr("About")
                Layout.alignment: Qt.AlignTop
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            Text {
                visible: (dlg.preview.comment ?? "").length > 0
                Layout.fillWidth: true
                text: dlg.preview.comment ?? ""
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }

            Text {
                visible: !dlg.preview.unsigned
                text: qsTr("Key")
                Layout.alignment: Qt.AlignTop
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            TextEdit {
                visible: !dlg.preview.unsigned
                Layout.fillWidth: true
                text: dlg.preview.fingerprint ?? ""
                readOnly: true
                selectByMouse: true
                wrapMode: TextEdit.Wrap
                font.family: "monospace"
                font.pointSize: TelamonStyle.fontSizeBody
                color: TelamonStyle.text
                textFormat: TextEdit.PlainText
                Accessible.name: qsTr("Key fingerprint")
                Accessible.readOnly: true
            }
        }

        // Not signed: a warning of its own, and a tick that has to be set.
        ColumnLayout {
            Layout.fillWidth: true
            visible: dlg.preview.unsigned === true
            spacing: TelamonStyle.spacingSmall
            Text {
                Layout.fillWidth: true
                text: qsTr("This source is not signed.")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                font.bold: true
                color: TelamonStyle.error
                textFormat: Text.PlainText
            }
            Text {
                Layout.fillWidth: true
                text: qsTr("It has no key, so the Store cannot tell that its apps come from who they say, or that nobody changed them on the way. Anyone who can reach that server could give you something else.")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }
            TelamonCheckBox {
                id: acknowledge
                Layout.fillWidth: true
                text: qsTr("I understand and still want to add it")
            }
        }

        // Who the source is for.
        ColumnLayout {
            Layout.fillWidth: true
            spacing: TelamonStyle.spacingSmall
            Text {
                text: qsTr("Who is it for?")
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                font.bold: true
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }
            TelamonRadioButton {
                id: forMe
                Layout.fillWidth: true
                text: qsTr("For you only")
                checked: dlg.scope === "user"
                enabled: (dlg.preview.user ?? ({})).state === "free"
                onClicked: dlg.scope = "user"
                Accessible.description: qsTr("No password needed")
            }
            Text {
                Layout.fillWidth: true
                Layout.leftMargin: TelamonStyle.spacingLarge * 2
                text: (dlg.preview.user ?? ({})).state === "free" ? qsTr("Only your account sees it. No password needed.") : ((dlg.preview.user ?? ({})).text ?? "")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: (dlg.preview.user ?? ({})).state === "free" ? TelamonStyle.textMuted : TelamonStyle.warning
                textFormat: Text.PlainText
            }
            TelamonRadioButton {
                id: forAll
                Layout.fillWidth: true
                text: qsTr("For everyone on this computer")
                checked: dlg.scope === "system"
                enabled: (dlg.preview.system ?? ({})).state === "free"
                onClicked: dlg.scope = "system"
                Accessible.description: qsTr("Asks for your password")
            }
            Text {
                Layout.fillWidth: true
                Layout.leftMargin: TelamonStyle.spacingLarge * 2
                text: (dlg.preview.system ?? ({})).state === "free" ? qsTr("Every account can use it. Asks for your password.") : ((dlg.preview.system ?? ({})).text ?? "")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: (dlg.preview.system ?? ({})).state === "free" ? TelamonStyle.textMuted : TelamonStyle.warning
                textFormat: Text.PlainText
            }
            Text {
                Layout.fillWidth: true
                visible: dlg.chosen.state === "free"
                text: qsTr("It will be added as \"%1\".").arg(dlg.chosen.name ?? "")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
        }

        RowLayout {
            Layout.fillWidth: true
            visible: dlg.busy
            spacing: TelamonStyle.spacingLarge
            TelamonSpinner {
                running: dlg.busy
                Layout.preferredWidth: 20
                Layout.preferredHeight: 20
            }
            Text {
                Layout.fillWidth: true
                text: dlg.sources.status.length > 0 ? dlg.sources.status : qsTr("Working…")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
        }
    }

    // Errors of either step, in plain words.
    Text {
        Layout.fillWidth: true
        visible: dlg.sources.addError.length > 0
        text: dlg.sources.addError
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        color: TelamonStyle.error
        textFormat: Text.PlainText
        Accessible.role: Accessible.StaticText
    }
}
