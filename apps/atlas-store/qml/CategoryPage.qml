import QtQuick
import QtQuick.Layouts
import Atlas.Ui

// The apps of one category: a sort choice, two filters and a grid. The list
// is made on a worker (see AppListModel); changing a choice asks again and
// the newest answer wins.
Item {
    id: page

    required property var catalog
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

    Component.onCompleted: run()

    Connections {
        target: page.catalog
        function onRevisionChanged() {
            page.run();
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.leftMargin: AtlasStyle.spacingXXLarge
        anchors.rightMargin: AtlasStyle.spacingXXLarge
        spacing: AtlasStyle.spacingLarge

        Flow {
            Layout.fillWidth: true
            spacing: AtlasStyle.spacingXLarge
            visible: !blockedState.blocked

            AtlasSegmentedControl {
                id: sortControl
                model: [qsTr("Name"), qsTr("Recently Updated")]
                currentIndex: 0
                Accessible.name: qsTr("Sort by")
                onActivated: page.run()
            }
            AtlasSwitch {
                id: verified
                text: qsTr("Verified Only")
                onToggled: page.run()
            }
            AtlasSwitch {
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
            AtlasEmptyState {
                anchors.fill: parent
                visible: page.model.count === 0 && !page.model.busy
                symbol: Symbols.Storefront
                title: qsTr("No Apps")
                text: verified.checked || free.checked ? qsTr("No apps in this category match the filters.") : qsTr("This category has no apps.")
            }
            AtlasSpinner {
                anchors.centerIn: parent
                visible: page.model.count === 0 && page.model.busy
                running: visible
            }
        }
    }
}
