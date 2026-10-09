import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// An app's page: from the catalog when it has an entry, from the installed
// data when it only is installed. Install, Open and Remove are here; each of
// Install and Remove asks in a dialog of its own (Main.qml). Everything shown
// comes from the catalog or a remote, so every Text is plain.
TelamonPage {
    id: page

    required property var catalog
    required property var jobs
    required property string appId

    // JSON from Jobs.appInfo, asked again when the catalog or the installed
    // list changes.
    property var info: {
        void page.catalog.revision;
        void page.jobs.installedRevision;
        return JSON.parse(page.jobs.appInfo(page.appId));
    }
    readonly property bool found: info.found === true
    readonly property bool installed: info.installed !== undefined
    readonly property bool idle: page.jobs.phase === "idle"
    readonly property bool mine: page.jobs.appId === page.appId && !idle

    // This page is an app's page (Main.qml skips opening it twice).
    readonly property bool isApp: true
    // The navigation stack shows a page's title in a Label that decides for
    // itself whether it is markup (Qt's AutoText), and a name such as
    // "<img src=...>" would then load that image. Telamon.Ui is not ours to
    // change, so the title carries look-alike characters in place of the
    // three that can start markup.
    function headerTitle(value: string): string {
        return value.replace(/[<>&]/g, c => ({ "<": "\uFF1C", ">": "\uFF1E", "&": "\uFF06" })[c]);
    }

    title: found ? headerTitle(info.name) : qsTr("App")

    signal removeRequested(string appId, string name, string scope, string ref)

    // Open asks the window system for an activation token first (C++,
    // activation_token.cpp) and starts the app when it, or "" for none, comes
    // back: without a token Wayland can leave the app's window behind this one.
    property bool opening: false

    function openApp() {
        if (page.opening || !page.idle) {
            return;
        }
        page.opening = true;
        ActivationToken.request(page.Window.window, page.appId);
    }

    Connections {
        target: ActivationToken
        function onReady(appId, token) {
            if (!page.opening || appId !== page.appId) {
                return;
            }
            page.opening = false;
            page.jobs.open(page.appId, token);
        }
    }

    function dateText(seconds) {
        return seconds > 0 ? new Date(seconds * 1000).toLocaleDateString(Qt.locale(), Locale.LongFormat) : "";
    }

    TelamonSpinner {
        Layout.alignment: Qt.AlignHCenter
        visible: !page.found && !page.catalog.ready
        running: visible
    }
    TelamonEmptyState {
        Layout.fillWidth: true
        Layout.preferredHeight: Kirigami.Units.gridUnit * 14
        visible: !page.found && page.catalog.ready
        symbol: Symbols.Search
        title: qsTr("App Not Found")
        text: qsTr("No source the Store knows lists this app, and it is not installed.")
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
                visible: (page.info.developer ?? "").length > 0
                spacing: TelamonStyle.spacingLarge
                Text {
                    Layout.fillWidth: true
                    text: page.info.developer ?? ""
                    elide: Text.ElideRight
                    font.family: TelamonStyle.fontFamily
                    font.pointSize: TelamonStyle.fontSizeBody
                    color: TelamonStyle.textMuted
                    textFormat: Text.PlainText
                }
                TelamonBadge {
                    visible: page.info.verified === true
                    text: qsTr("Verified")
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
                    visible: !page.installed && page.info.canInstall === true
                    installState: page.mine && page.jobs.phase !== "idle" ? "installing" : "install"
                    progress: page.jobs.percent >= 0 ? page.jobs.percent / 100 : -1
                    enabled: page.idle || page.mine
                    onClicked: {
                        if (!page.mine) {
                            page.jobs.planInstall(page.appId);
                        }
                    }
                    onCancelRequested: page.jobs.cancel()
                }
                TelamonButton {
                    visible: page.installed
                    text: qsTr("Open")
                    prominent: true
                    enabled: page.idle && !page.opening
                    onClicked: page.openApp()
                }
                Repeater {
                    model: page.info.installs ?? []
                    TelamonButton {
                        required property var modelData
                        text: (page.info.installs ?? []).length > 1 ? qsTr("Remove from %1 Installation").arg(modelData.scope) : qsTr("Remove")
                        variant: TelamonButton.Destructive
                        enabled: page.idle
                        onClicked: page.removeRequested(page.appId, page.info.name, modelData.scope, modelData.ref)
                    }
                }
            }
        }
    }

    JobStatus {
        Layout.fillWidth: true
        visible: page.found && (running || hasMessage)
        jobs: page.jobs
        forApp: page.appId
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
                if ((i.version ?? "").length > 0) {
                    rows.push([qsTr("Version"), i.version]);
                }
                if ((i.released ?? 0) > 0) {
                    rows.push([qsTr("Released"), page.dateText(i.released)]);
                }
                if ((i.license ?? "").length > 0) {
                    rows.push([qsTr("License"), i.free ? qsTr("%1 (free software)").arg(i.license) : qsTr("%1 (proprietary)").arg(i.license)]);
                }
                if ((i.source ?? "").length > 0) {
                    rows.push([qsTr("Source"), i.source]);
                }
                for (const inst of (i.installs ?? [])) {
                    rows.push([qsTr("Installed"), qsTr("%1 installation, %2").arg(inst.scope).arg(inst.size)]);
                }
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

    Repeater {
        model: page.info.blocks ?? []
        Text {
            required property string modelData
            Layout.fillWidth: true
            text: modelData
            wrapMode: Text.Wrap
            font.family: TelamonStyle.fontFamily
            font.pointSize: TelamonStyle.fontSizeBody
            color: TelamonStyle.text
            textFormat: Text.PlainText
        }
    }

    // https links only (the core drops anything else); the portal opens them.
    Flow {
        Layout.fillWidth: true
        visible: (page.info.links ?? []).length > 0
        spacing: TelamonStyle.spacingLarge
        Repeater {
            model: page.info.links ?? []
            TelamonButton {
                required property var modelData
                text: modelData.label
                onClicked: TelamonPortal.openUrl(modelData.url)
                TelamonToolTip {
                    text: parent.modelData.host
                    shown: parent.hovered || parent.visualFocus
                }
            }
        }
    }
}
