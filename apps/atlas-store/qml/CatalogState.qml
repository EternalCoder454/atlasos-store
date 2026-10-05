import QtQuick
import Atlas.Ui

// What a page shows in place of apps while there are none to show: the
// catalogs are loading, there is no source, a catalog was never downloaded,
// or reading them failed. `blocked` says whether it applies; the page hides
// its content then and shows this. Every text here is plain: a source's name
// and the error lines come from outside.
Item {
    id: root

    // The Catalog QObject (src/catalog.rs).
    required property var catalog

    // True until at least one app is there to browse.
    readonly property bool blocked: !catalog.ready || catalog.appCount === 0

    // The user wants the Sources place (no source is enabled).
    signal openSources

    implicitWidth: state.implicitWidth
    implicitHeight: state.implicitHeight
    visible: blocked

    readonly property int kind: {
        if (!catalog.ready) {
            return 0;
        }
        if (catalog.sourceCount === 0 && catalog.errorText.length === 0) {
            return 1;
        }
        if (catalog.appCount === 0 && catalog.missingCount > 0 && catalog.errorText.length === 0) {
            return 2;
        }
        if (catalog.errorText.length > 0) {
            return 3;
        }
        return 4;
    }

    AtlasSpinner {
        anchors.centerIn: parent
        visible: root.kind === 0 || root.catalog.loading
        running: visible
        z: 1
    }

    AtlasEmptyState {
        id: state
        anchors.fill: parent
        visible: root.kind !== 0
        // A reload in progress shows the spinner; Try Again can't start another.
        enabled: !root.catalog.loading
        opacity: root.catalog.loading ? 0.4 : 1
        symbol: root.kind === 1 ? Symbols.Dns : root.kind === 2 ? Symbols.Download : root.kind === 3 ? Symbols.Error : Symbols.Storefront
        title: root.kind === 1 ? qsTr("No Sources") : root.kind === 2 ? qsTr("Catalog Not Downloaded") : root.kind === 3 ? qsTr("Could Not Read the Catalogs") : qsTr("No Apps")
        text: root.kind === 1 ? qsTr("Add or enable a source to find apps. Sources are managed in the Sources place.") : root.kind === 2 ? qsTr("The app catalog has not been downloaded yet. It is refreshed while the Store is open, and needs a network connection.") : root.kind === 3 ? root.catalog.errorText : qsTr("The enabled sources list no apps.")
        actionText: root.kind === 1 ? qsTr("Open Sources") : (root.kind === 2 || root.kind === 3) ? qsTr("Try Again") : ""
        actionSymbol: root.kind === 1 ? Symbols.Dns : Symbols.Refresh
        onTriggered: {
            if (root.kind === 1) {
                root.openSources();
            } else {
                root.catalog.reload();
            }
        }
    }
}
