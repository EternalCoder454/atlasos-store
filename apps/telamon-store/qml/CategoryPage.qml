import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// The apps of one category: a sort choice, two filters and a grid. The list
// is made on a worker (see AppListModel); changing a choice asks again and
// the newest answer wins.
Item {
    id: page

    required property var catalog
    // Flathub's curated lists (src/featured.rs).
    required property var featured
    required property var model
    // The category's key from Catalog.categoryKey, "" for every app.
    required property string categoryKey
    property string title

    signal appRequested(string appId)
    signal openSources

    function run() {
        if (!catalog.ready || catalog.appCount === 0) {
            model.clear();
            return;
        }
        model.browse(page.categoryKey, sortControl.currentIndex === 1 ? "updated" : "name", verified.checked, free.checked);
    }

    // The model is shared: empty it first so the previous category's apps
    // never show while this one is being listed.
    Component.onCompleted: {
        model.clear();
        run();
    }

    Connections {
        target: page.catalog
        function onRevisionChanged() {
            page.run();
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.leftMargin: TelamonStyle.spacingXXLarge
        anchors.rightMargin: TelamonStyle.spacingXXLarge
        spacing: TelamonStyle.spacingLarge

        Flow {
            Layout.fillWidth: true
            spacing: TelamonStyle.spacingXLarge
            visible: !blockedState.blocked

            TelamonSegmentedControl {
                id: sortControl
                model: [qsTr("Name"), qsTr("Recently Updated")]
                currentIndex: 0
                Accessible.name: qsTr("Sort by")
                onActivated: page.run()
            }
            TelamonSwitch {
                id: verified
                text: qsTr("Verified Only")
                onToggled: page.run()
            }
            TelamonSwitch {
                id: free
                text: qsTr("Free Licenses Only")
                onToggled: page.run()
            }
        }

        CatalogState {
            id: blockedState
            Layout.fillWidth: true
            Layout.fillHeight: true
            catalog: page.catalog
            onOpenSources: page.openSources()
        }

        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
            visible: !blockedState.blocked

            AppGrid {
                anchors.fill: parent
                model: page.model
                visible: page.model.count > 0
                onOpened: appId => page.appRequested(appId)
            }
            TelamonEmptyState {
                anchors.fill: parent
                visible: page.model.count === 0 && !page.model.busy
                symbol: Symbols.Storefront
                title: qsTr("No Apps")
                text: verified.checked || free.checked ? qsTr("No apps in this category match the filters.") : qsTr("This category has no apps.")
            }
            TelamonSpinner {
                anchors.centerIn: parent
                visible: page.model.count === 0 && page.model.busy
                running: visible
            }
        }
    }
}
