import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// A Telamon app's page (src/native.rs): what it is, where it comes from, and
// Install, Update, Open and Uninstall. Install and Update ask in the Store's
// own dialog first (Main.qml), as does Uninstall. Every text is a cleaned
// manifest field or ours, shown as plain text.
TelamonPage {
    id: page

    required property var nativeApps
    required property string appId

    signal installRequested(var app)
    signal uninstallRequested(string appId, string name)

    // This page is an app's page (Main.qml skips opening it twice).
    readonly property bool isApp: true

    NativeAppsList {
        id: list
        json: page.nativeApps.appsJson
    }

    readonly property var info: list.all.find(a => a.id === page.appId) ?? ({})
    readonly property bool found: info.id !== undefined
    readonly property bool installed: info.installed === true
    readonly property bool idle: page.nativeApps.phase === "idle"
    // Open and Uninstall need no network: a running check does not stop them.
    readonly property bool free: page.idle || page.nativeApps.phase === "checking"
    readonly property bool mine: page.nativeApps.busyId === page.appId && !idle
    readonly property bool messageHere: page.nativeApps.busyId.length === 0 || page.nativeApps.busyId === page.appId

    title: found ? info.name : qsTr("App")

    // Open asks the window system for an activation token first, then starts
    // the app: without one Wayland can leave its window behind this one.
    property bool opening: false

    function openApp() {
        if (page.opening || !page.free) {
            return;
        }
        page.opening = true;
        ActivationToken.request(page.Window.window, "native:" + page.appId);
    }

    Connections {
        target: ActivationToken
        function onReady(key, token) {
            if (!page.opening || key !== "native:" + page.appId) {
                return;
            }
            page.opening = false;
            page.nativeApps.open(page.appId, token);
        }
    }

    TelamonSpinner {
        Layout.alignment: Qt.AlignHCenter
        visible: !page.found && !page.nativeApps.ready
        running: visible
    }
    TelamonEmptyState {
        Layout.fillWidth: true
        Layout.preferredHeight: Kirigami.Units.gridUnit * 14
        visible: !page.found && page.nativeApps.ready
        symbol: Symbols.Search
        title: qsTr("App Not Found")
        text: qsTr("This app is not in Telamon's list of apps, and it is not installed.")
    }

    RowLayout {
        Layout.fillWidth: true
        visible: page.found
        spacing: TelamonStyle.spacingXLarge

        Rectangle {
            Layout.preferredWidth: Kirigami.Units.gridUnit * 5
            Layout.preferredHeight: Kirigami.Units.gridUnit * 5
            Layout.alignment: Qt.AlignTop
            radius: Math.round(width * 0.225)
            color: appIcon.status === Image.Ready ? "transparent" : Qt.alpha(TelamonStyle.accent, 0.14)
            Accessible.ignored: true
            Text {
                anchors.centerIn: parent
                visible: appIcon.status !== Image.Ready
                text: (page.info.name ?? "").length > 0 ? page.info.name.charAt(0).toUpperCase() : ""
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeTitle
                font.bold: true
                color: TelamonStyle.accent
                textFormat: Text.PlainText
            }
            Image {
                id: appIcon
                anchors.fill: parent
                source: page.info.iconSource ?? ""
                asynchronous: true
                fillMode: Image.PreserveAspectFit
                sourceSize: Qt.size(parent.width * Screen.devicePixelRatio, parent.height * Screen.devicePixelRatio)
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignTop
            spacing: TelamonStyle.spacingSmall

            Text {
                Layout.fillWidth: true
                text: page.info.name ?? ""
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeTitle
                font.bold: true
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }
            RowLayout {
                Layout.fillWidth: true
                spacing: TelamonStyle.spacingLarge
                Text {
                    Layout.fillWidth: true
                    text: page.info.local === true ? qsTr("Installed from a file") : qsTr("Telamon app")
                    elide: Text.ElideRight
                    font.family: TelamonStyle.fontFamily
                    font.pointSize: TelamonStyle.fontSizeBody
                    color: TelamonStyle.textMuted
                    textFormat: Text.PlainText
                }
                TelamonBadge {
                    visible: page.info.state === "update"
                    text: qsTr("Update available")
                    type: "success"
                }
            }
            Text {
                Layout.fillWidth: true
                visible: text.length > 0
                text: page.info.summary ?? ""
                wrapMode: Text.Wrap
                font.family: TelamonStyle.fontFamily
                font.pointSize: TelamonStyle.fontSizeBody
                color: TelamonStyle.text
                textFormat: Text.PlainText
            }

            RowLayout {
                Layout.topMargin: TelamonStyle.spacingLarge
                spacing: TelamonStyle.spacingLarge

                TelamonInstallButton {
                    visible: !page.installed && page.info.state === "available"
                    installState: page.mine ? "installing" : "install"
                    progress: page.nativeApps.percent >= 0 ? page.nativeApps.percent / 100 : -1
                    enabled: page.idle || page.mine
                    onClicked: {
                        if (!page.mine) {
                            page.installRequested(page.info);
                        }
                    }
                }
                TelamonButton {
                    visible: page.installed && page.info.present !== false
                    text: qsTr("Open")
                    variant: page.info.state === "update" ? TelamonButton.Default : TelamonButton.Prominent
                    enabled: page.free && !page.opening
                    onClicked: page.openApp()
                }
                TelamonButton {
                    visible: page.info.state === "update"
                    text: qsTr("Update")
                    variant: TelamonButton.Prominent
                    enabled: page.idle
                    onClicked: page.installRequested(page.info)
                }
                TelamonButton {
                    visible: page.installed
                    text: qsTr("Uninstall")
                    variant: TelamonButton.Destructive
                    enabled: page.free
                    onClicked: page.uninstallRequested(page.appId, page.info.name)
                }
            }
        }
    }

    // A release that cannot run here, said plainly.
    Text {
        Layout.fillWidth: true
        visible: page.found && (page.info.reason ?? "").length > 0
        text: page.info.state === "incompatible" ? qsTr("This app can't be installed here. %1").arg(page.info.reason) : qsTr("The newest version can't be installed here. %1").arg(page.info.reason ?? "")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        color: TelamonStyle.warning
        textFormat: Text.PlainText
    }
    Text {
        Layout.fillWidth: true
        visible: page.found && page.installed && page.info.present === false
        text: qsTr("The app's files are missing. Uninstall it and install it again.")
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeBody
        color: TelamonStyle.error
        textFormat: Text.PlainText
    }

    // What the running job does, and what the last one did.
    ColumnLayout {
        Layout.fillWidth: true
        visible: page.found && page.mine
        spacing: TelamonStyle.spacingSmall
        Text {
            Layout.fillWidth: true
            text: page.nativeApps.status.length > 0 ? page.nativeApps.status : qsTr("Working…")
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeCaption
            color: TelamonStyle.textMuted
            textFormat: Text.PlainText
        }
        TelamonProgressBar {
            Layout.fillWidth: true
            indeterminate: page.nativeApps.percent < 0
            value: Math.max(0, page.nativeApps.percent) / 100
        }
    }
    RowLayout {
        Layout.fillWidth: true
        visible: page.found && page.idle && page.messageHere && (page.nativeApps.errorText.length > 0 || page.nativeApps.resultText.length > 0)
        spacing: TelamonStyle.spacingLarge
        Text {
            Layout.fillWidth: true
            text: page.nativeApps.errorText.length > 0 ? page.nativeApps.errorText : page.nativeApps.resultText
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: page.nativeApps.errorText.length > 0 ? TelamonStyle.error : TelamonStyle.success
            textFormat: Text.PlainText
        }
        TelamonButton {
            text: qsTr("Dismiss")
            onClicked: page.nativeApps.clearMessages()
        }
    }

    GridLayout {
        Layout.fillWidth: true
        visible: page.found
        columns: 2
        columnSpacing: TelamonStyle.spacingXLarge
        rowSpacing: TelamonStyle.spacingSmall

        Repeater {
            model: {
                const rows = [];
                const i = page.info;
                if ((i.installedVersion ?? "").length > 0) {
                    rows.push([qsTr("Installed"), (i.installedSize ?? "").length > 0 ? qsTr("Version %1, %2").arg(i.installedVersion).arg(i.installedSize) : qsTr("Version %1").arg(i.installedVersion)]);
                }
                if ((i.availableVersion ?? "").length > 0 && i.availableVersion !== i.installedVersion) {
                    rows.push([qsTr("Newest"), (i.size ?? "").length > 0 ? qsTr("Version %1, %2 to download").arg(i.availableVersion).arg(i.size) : qsTr("Version %1").arg(i.availableVersion)]);
                }
                if ((i.license ?? "").length > 0) {
                    rows.push([qsTr("License"), i.license]);
                }
                if ((i.repo ?? "").length > 0) {
                    rows.push([qsTr("Source"), "github.com/" + i.repo]);
                }
                rows.push([qsTr("Installs to"), qsTr("Your own folder only (.local/share/telamon-apps)")]);
                return rows;
            }
            delegate: Item {
                id: cell
                required property var modelData
                Layout.columnSpan: 2
                Layout.fillWidth: true
                implicitHeight: cellRow.implicitHeight
                RowLayout {
                    id: cellRow
                    width: parent.width
                    spacing: TelamonStyle.spacingXLarge
                    Text {
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 7
                        Layout.alignment: Qt.AlignTop
                        text: cell.modelData[0]
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        color: TelamonStyle.textMuted
                        textFormat: Text.PlainText
                    }
                    Text {
                        Layout.fillWidth: true
                        text: cell.modelData[1]
                        wrapMode: Text.Wrap
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeBody
                        color: TelamonStyle.text
                        textFormat: Text.PlainText
                    }
                }
            }
        }
    }

    // The project's page, https only (the manifest reader refused anything
    // else); the portal opens it.
    Flow {
        Layout.fillWidth: true
        visible: page.found && (page.info.homepage ?? "").length > 0
        spacing: TelamonStyle.spacingLarge
        TelamonButton {
            text: qsTr("Project Page")
            onClicked: TelamonPortal.openUrl(page.info.homepage)
        }
    }
}
