import QtQuick
import QtQuick.Layouts
import Atlas.Ui

// The installed apps (src/jobs.rs reads them on the worker): icon, name,
// installation and size, each with Remove, and Remove Unused with the list
// shown first. Every text is plain.
AtlasPage {
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
        AtlasButton {
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
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeCaption
        color: AtlasStyle.error
        textFormat: Text.PlainText
    }

    AtlasSpinner {
        Layout.alignment: Qt.AlignHCenter
        visible: !page.jobs.installedReady
        running: visible
    }

    AtlasEmptyState {
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

    Repeater {
        model: page.apps
        RowLayout {
            id: row
            required property var modelData
            Layout.fillWidth: true
            spacing: AtlasStyle.spacingLarge

            AppTile {
                Layout.fillWidth: true
                appId: row.modelData.appId
                name: row.modelData.name
                summary: qsTr("%1 installation · %2%3").arg(row.modelData.scope).arg(row.modelData.size).arg(row.modelData.version.length > 0 ? " · " + row.modelData.version : "")
                iconSource: row.modelData.iconSource
                onClicked: page.appRequested(row.modelData.appId)
            }
            AtlasButton {
                text: qsTr("Remove")
                variant: AtlasButton.Destructive
                enabled: page.idle
                onClicked: page.removeRequested(row.modelData.appId, row.modelData.name, row.modelData.scope, row.modelData.ref)
            }
        }
    }
}
