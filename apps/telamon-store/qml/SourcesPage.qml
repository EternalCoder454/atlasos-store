pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// The Sources place: the Flatpak remotes (src/sources.rs reads them on the
// worker). One grouped card with a row per source, the sources of single
// apps folded away, and Add Source in the header. The Add and Remove
// questions are dialogs of the window (Main.qml). Every text is plain.
TelamonPage {
    id: page

    required property var sources
    required property var jobs

    title: qsTr("Sources")
    subtitle: qsTr("Sources are where the Store finds apps. Flathub is on by default.")

    // Add Source was pressed.
    signal addRequested
    // Remove was pressed on the source (scope is "user" or "system").
    signal removeRequested(string scope, string name)

    readonly property var all: {
        void page.sources.revision;
        return JSON.parse(page.sources.sourcesJson);
    }
    readonly property var main: page.all.filter(s => !s.singleApp)
    readonly property var single: page.all.filter(s => s.singleApp)
    readonly property bool idle: page.sources.phase === "idle"

    headerTrailing: [
        TelamonButton {
            text: qsTr("Add Source")
            symbol: Symbols.Add
            variant: TelamonButton.Prominent
            enabled: page.idle
            onClicked: page.addRequested()
        }
    ]

    // Read the list when the page opens, and again when what is installed
    // changes (the rows say what is installed from each source).
    Component.onCompleted: page.sources.refresh()
    Connections {
        target: page.jobs
        function onInstalledRevisionChanged() {
            page.sources.refresh();
        }
    }

    SourcesStatus {
        Layout.fillWidth: true
        sources: page.sources
    }

    Text {
        Layout.fillWidth: true
        visible: page.sources.sourcesError.length > 0
        text: page.sources.sourcesError
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.error
        textFormat: Text.PlainText
    }

    TelamonSpinner {
        Layout.alignment: Qt.AlignHCenter
        visible: !page.sources.sourcesReady
        running: visible
    }

    TelamonEmptyState {
        Layout.fillWidth: true
        Layout.preferredHeight: 260
        visible: page.sources.sourcesReady && page.all.length === 0
        symbol: Symbols.Dns
        title: page.sources.sourcesError.length > 0 ? qsTr("Could Not Read the Sources") : qsTr("No Sources")
        text: page.sources.sourcesError.length > 0 ? qsTr("Try again in a moment.") : qsTr("Add a source to find apps.")
        actionText: page.sources.sourcesError.length > 0 ? qsTr("Try Again") : qsTr("Add Source")
        actionSymbol: page.sources.sourcesError.length > 0 ? Symbols.Refresh : Symbols.Add
        onTriggered: page.sources.sourcesError.length > 0 ? page.sources.refresh() : page.addRequested()
    }

    // The sources the catalog lists, as rows of one card.
    Section {
        Layout.fillWidth: true
        visible: page.main.length > 0

        Repeater {
            model: page.main
            SourceRow {
                required property var modelData
                source: modelData
                idle: page.idle
                onToggled: enabled => page.sources.setEnabled(modelData.scope, modelData.name, enabled)
                onRemoveRequested: page.removeRequested(modelData.scope, modelData.name)
            }
        }
    }

    // The sources an app's file added for that app alone (flatpak's "-origin"
    // sources) are not part of the catalog; they are folded away.
    TelamonExpandableSection {
        visible: page.single.length > 0
        title: qsTr("Show sources of single apps (%1)").arg(page.single.length)
        subtitle: qsTr("Added for one app each when it was installed from a file.")

        Section {
            Layout.fillWidth: true
            Repeater {
                model: page.single
                SourceRow {
                    required property var modelData
                    source: modelData
                    idle: page.idle
                    onToggled: enabled => page.sources.setEnabled(modelData.scope, modelData.name, enabled)
                    onRemoveRequested: page.removeRequested(modelData.scope, modelData.name)
                }
            }
        }
    }
}
