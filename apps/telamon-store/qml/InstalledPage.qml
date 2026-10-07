pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// The installed apps (src/jobs.rs reads them on the worker) as rows of one
// grouped card: icon, name, installation, size and version, each row with
// its own Remove, and Remove Unused with the list shown first. Every text is
// plain.
TelamonPage {
    id: page

    required property var jobs

    title: qsTr("Installed")

    signal appRequested(string appId)
    signal removeRequested(string appId, string name, string scope, string ref)

    readonly property var apps: {
        void page.jobs.installedRevision;
        return JSON.parse(page.jobs.installedJson);
    }
    readonly property bool idle: page.jobs.phase === "idle"

    headerTrailing: [
        TelamonButton {
            text: qsTr("Remove Unused")
            enabled: page.idle && page.jobs.installedReady
            onClicked: page.jobs.checkUnused()
        }
    ]

    JobStatus {
        Layout.fillWidth: true
        jobs: page.jobs
    }

    Text {
        Layout.fillWidth: true
        visible: page.jobs.installedError.length > 0
        text: page.jobs.installedError
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.error
        textFormat: Text.PlainText
    }

    TelamonSpinner {
        Layout.alignment: Qt.AlignHCenter
        visible: !page.jobs.installedReady
        running: visible
    }

    TelamonEmptyState {
        Layout.fillWidth: true
        Layout.preferredHeight: 260
        visible: page.jobs.installedReady && page.apps.length === 0
        symbol: Symbols.Apps
        title: page.jobs.installedError.length > 0 ? qsTr("Could Not Read Installed Apps") : qsTr("No Apps Installed")
        text: page.jobs.installedError.length > 0 ? qsTr("Try again in a moment.") : qsTr("Apps you install show up here.")
        actionText: page.jobs.installedError.length > 0 ? qsTr("Try Again") : ""
        actionSymbol: Symbols.Refresh
        onTriggered: page.jobs.refresh()
    }

    // One grouped card, a row per app: icon, name and details, and the
    // app's Remove inside its row at the trailing end. Clicking the row (or
    // Return on it) opens the app's page; Tab moves on to its Remove.
    Section {
        Layout.fillWidth: true
        visible: page.apps.length > 0

        Repeater {
            model: page.apps
            SectionRow {
                id: row
                required property var modelData
                readonly property real iconSide: Math.round(Kirigami.Units.gridUnit * 2.8)

                title: row.modelData.name
                subtitle: qsTr("%1 installation · %2%3").arg(row.modelData.scope).arg(row.modelData.size).arg(row.modelData.version.length > 0 ? " · " + row.modelData.version : "")
                clickable: true
                onClicked: page.appRequested(row.modelData.appId)

                leading: Rectangle {
                    width: row.iconSide
                    height: row.iconSide
                    radius: Math.round(row.iconSide * 0.225)
                    color: icon.status === Image.Ready ? "transparent" : Qt.alpha(TelamonStyle.accent, 0.14)
                    Accessible.ignored: true
                    Image {
                        id: icon
                        anchors.fill: parent
                        source: row.modelData.iconSource
                        asynchronous: true
                        cache: true
                        fillMode: Image.PreserveAspectFit
                        sourceSize: Qt.size(row.iconSide * Screen.devicePixelRatio, row.iconSide * Screen.devicePixelRatio)
                    }
                }

                // The name bold and both lines elided, as on the app tiles;
                // the row's title and subtitle above are what a screen
                // reader reads.
                content: ColumnLayout {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignVCenter
                    spacing: TelamonStyle.spacingXSmall

                    Text {
                        Layout.fillWidth: true
                        text: row.title
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeBody
                        font.bold: true
                        color: TelamonStyle.text
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        Accessible.ignored: true
                    }
                    Text {
                        Layout.fillWidth: true
                        text: row.subtitle
                        font.family: TelamonStyle.fontFamily
                        font.pointSize: TelamonStyle.fontSizeCaption
                        color: TelamonStyle.textMuted
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        Accessible.ignored: true
                    }
                }

                TelamonButton {
                    text: qsTr("Remove")
                    //: A Remove button in a row of the Installed list; %1 is the app's name
                    Accessible.name: qsTr("Remove %1").arg(row.modelData.name)
                    variant: TelamonButton.Destructive
                    enabled: page.idle
                    onClicked: page.removeRequested(row.modelData.appId, row.modelData.name, row.modelData.scope, row.modelData.ref)
                }
            }
        }
    }
}
