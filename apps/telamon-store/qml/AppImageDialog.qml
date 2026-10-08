import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// The confirmation before an AppImage is installed (src/appimages.rs looked
// inside the file without running it). It names the app, says what an
// AppImage can access and, always, that it "isn't sandboxed and isn't checked
// by Telamon", then lists what was found out about the file: red when it is a
// reason to stop and think (not signed, a wrong signature, no known source,
// a plain http download, a file that can't be looked into). When the app is on
// Flathub, getting that version is the main choice and installing the
// AppImage anyway is the other. Otherwise the default button is Cancel. The
// dialog ignores input for its first half second. Every text is plain: all
// of it came out of a file.
ConfirmDialog {
    id: dlg

    required property var appImages
    property var detail: ({})
    property bool armed: false
    property bool answered: false

    readonly property bool onFlathub: dlg.detail.flathub !== null && dlg.detail.flathub !== undefined
    readonly property bool strong: dlg.detail.strong === true

    // The page of the Flathub version was asked for.
    signal flathubRequested(string appId)

    width: Math.min(parent ? Math.max(0, parent.width - Kirigami.Units.gridUnit * 2) : 0, Kirigami.Units.gridUnit * 31)
    title: qsTr("Install %1?").arg(dlg.detail.name ?? "")
    acceptText: dlg.onFlathub ? qsTr("Get Flathub Version") : qsTr("Install")
    alternativeText: dlg.onFlathub ? qsTr("Install AppImage Anyway") : ""
    rejectText: qsTr("Cancel")
    defaultButton: dlg.onFlathub ? "accept" : "reject"
    focusReject: !dlg.onFlathub
    destructive: !dlg.onFlathub && dlg.strong
    closeOnAccept: false

    function show() {
        dlg.detail = JSON.parse(dlg.appImages.detailJson);
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
        if (dlg.onFlathub) {
            dlg.flathubRequested(dlg.detail.flathub.appId);
            dlg.appImages.cancel();
        } else {
            dlg.appImages.confirm();
        }
    }
    onAlternative: {
        if (!dlg.armed) {
            return;
        }
        dlg.answered = true;
        dlg.close();
        dlg.appImages.confirm();
    }
    // Closed any other way: the file is dropped.
    onClosed: {
        if (!dlg.answered) {
            dlg.appImages.cancel();
        }
    }

    RowLayout {
        Layout.fillWidth: true
        spacing: TelamonStyle.spacingLarge

        Rectangle {
            readonly property real side: Math.round(Kirigami.Units.gridUnit * 3.4)
            Layout.preferredWidth: side
            Layout.preferredHeight: side
            Layout.alignment: Qt.AlignTop
            radius: Math.round(side * 0.225)
            color: icon.status === Image.Ready ? "transparent" : Qt.alpha(TelamonStyle.accent, 0.14)
            Accessible.ignored: true
            Text {
                anchors.centerIn: parent
                visible: icon.status !== Image.Ready
                text: (dlg.detail.name ?? "").length > 0 ? dlg.detail.name.charAt(0).toUpperCase() : ""
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeHeading
                font.bold: true
                color: TelamonStyle.accent
                textFormat: Text.PlainText
            }
            Image {
                id: icon
                anchors.fill: parent
                source: dlg.detail.iconSource ?? ""
                asynchronous: true
                fillMode: Image.PreserveAspectFit
                sourceSize: Qt.size(parent.side * Screen.devicePixelRatio, parent.side * Screen.devicePixelRatio)
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            spacing: TelamonStyle.spacingXSmall
            Text {
                Layout.fillWidth: true
                text: dlg.detail.name ?? ""
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                font.bold: true
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }
            Text {
                Layout.fillWidth: true
                text: (dlg.detail.version ?? "").length > 0 ? qsTr("Version %1").arg(dlg.detail.version) : qsTr("Version unknown")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            Text {
                Layout.fillWidth: true
                text: (dlg.detail.publisher ?? "").length > 0 ? qsTr("By %1").arg(dlg.detail.publisher) : qsTr("Publisher unknown")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
            Text {
                Layout.fillWidth: true
                text: qsTr("Size: %1").arg(dlg.detail.size ?? "")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.textMuted
                textFormat: Text.PlainText
            }
        }
    }

    Text {
        Layout.fillWidth: true
        visible: (dlg.detail.summary ?? "").length > 0
        text: dlg.detail.summary ?? ""
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }

    // On Flathub: the better choice, said first.
    Rectangle {
        Layout.fillWidth: true
        visible: dlg.onFlathub
        implicitHeight: flathubText.implicitHeight + TelamonStyle.spacingLarge * 2
        radius: TelamonStyle.radius
        color: Qt.alpha(TelamonStyle.accent, 0.12)
        border.width: 1
        border.color: Qt.alpha(TelamonStyle.accent, 0.5)
        ColumnLayout {
            id: flathubText
            anchors.fill: parent
            anchors.margins: TelamonStyle.spacingLarge
            spacing: TelamonStyle.spacingXSmall
            Text {
                Layout.fillWidth: true
                text: qsTr("%1 is on Flathub").arg(dlg.onFlathub ? dlg.detail.flathub.name : "")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                font.bold: true
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }
            Text {
                Layout.fillWidth: true
                text: qsTr("Get the Flathub version instead - safer, sandboxed, updates automatically.")
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeCaption
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }
        }
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
        text: dlg.detail.access ?? ""
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }

    Text {
        text: qsTr("About This File")
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        font.bold: true
        color: TelamonStyle.text
        textFormat: Text.PlainText
    }
    Rectangle {
        Layout.fillWidth: true
        implicitHeight: findings.implicitHeight + TelamonStyle.spacingLarge * 2
        radius: TelamonStyle.radius
        color: dlg.strong ? Qt.alpha(TelamonStyle.error, 0.10) : Qt.alpha(TelamonStyle.warning, 0.08)
        border.width: 1
        border.color: dlg.strong ? Qt.alpha(TelamonStyle.error, 0.7) : Qt.alpha(TelamonStyle.warning, 0.4)
        ColumnLayout {
            id: findings
            anchors.fill: parent
            anchors.margins: TelamonStyle.spacingLarge
            spacing: TelamonStyle.spacing
            Repeater {
                model: dlg.detail.lines ?? []
                RowLayout {
                    id: finding
                    required property var modelData
                    Layout.fillWidth: true
                    spacing: TelamonStyle.spacingLarge
                    // The badges are one width, so the texts line up.
                    Item {
                        Layout.alignment: Qt.AlignTop
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 4.4
                        Layout.preferredHeight: badge.implicitHeight
                        TelamonBadge {
                            id: badge
                            text: finding.modelData.severity === "danger" ? qsTr("Warning") : finding.modelData.severity === "caution" ? qsTr("Caution") : qsTr("Info")
                            type: finding.modelData.severity === "danger" ? "error" : finding.modelData.severity === "caution" ? "warning" : "neutral"
                        }
                    }
                    Text {
                        Layout.fillWidth: true
                        text: finding.modelData.text
                        wrapMode: Text.Wrap
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        font.bold: finding.modelData.severity === "danger"
                        color: finding.modelData.severity === "danger" ? TelamonStyle.error : TelamonStyle.text
                        textFormat: Text.PlainText
                    }
                }
            }
        }
    }

    Text {
        Layout.fillWidth: true
        text: dlg.detail.replaces === true ? qsTr("Replaces the copy you installed before (%1) in the Applications folder.").arg((dlg.detail.installedVersion ?? "").length > 0 ? dlg.detail.installedVersion : qsTr("version unknown")) : qsTr("A copy is saved in your Applications folder as %1. The file you downloaded stays where it is.").arg(dlg.detail.installsAs ?? "")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.textMuted
        textFormat: Text.PlainText
    }
}
