pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// The apps of one category: the category's popular apps on a shelf, a sort
// choice, two filters and the grid of every app in it. The shelf and the
// choices are the grid's header, so they scroll away with the page. The list
// is made on a worker (see AppListModel); changing a choice asks again and the
// newest answer wins.
Item {
    id: page

    required property var catalog
    // Flathub's curated lists (src/featured.rs).
    required property var featured
    required property var model
    // The category's key from Catalog.categoryKey, "" for every app.
    required property string categoryKey
    property string title

    // The choices. Kept here, not in the header, so they survive it being
    // made again.
    property int sortIndex: 0
    // Only apps whose publisher Flathub verified.
    property bool verifiedOnly: false
    // Only apps with a free-software licence (the catalog's is_free_license).
    property bool freeOnly: false

    // "Popular in <Category>": the apps src/featured.rs published for this
    // category (the local catalog's own text). The shelf is for the whole
    // category, so it is hidden while a filter narrows the list below it, or
    // it would show apps the filter excludes.
    readonly property var shelfApps: {
        if (page.verifiedOnly || page.freeOnly || page.featured.categoryJson.length === 0) {
            return [];
        }
        try {
            const shelf = JSON.parse(page.featured.categoryJson);
            return shelf.key === page.categoryKey && Array.isArray(shelf.apps) ? shelf.apps : [];
        } catch (e) {
            return [];
        }
    }

    // The page shows its top (the shelf and the choices) until the user scrolls:
    // the shelf arrives after the page, and the grid would otherwise keep what
    // it showed and leave the new header above the view.
    property bool pinnedToTop: true

    signal appRequested(string appId)
    signal openSources

    function run() {
        if (!catalog.ready || catalog.appCount === 0) {
            model.clear();
            return;
        }
        model.browse(page.categoryKey, page.sortIndex === 1 ? "updated" : "name", page.verifiedOnly, page.freeOnly);
    }

    // Shows the cached list of this category's popular apps and lets the
    // worker refresh it when it has expired. Nothing is asked for when there
    // is no catalog to match against.
    function askShelf() {
        if (catalog.ready && catalog.appCount > 0) {
            featured.ensureCategory(page.categoryKey);
        }
    }

    // The model is shared: empty it first so the previous category's apps
    // never show while this one is being listed.
    Component.onCompleted: {
        model.clear();
        run();
        askShelf();
    }

    Connections {
        target: page.catalog
        function onRevisionChanged() {
            page.run();
            page.askShelf();
        }
    }

    CatalogState {
        id: blockedState
        anchors.fill: parent
        anchors.leftMargin: TelamonStyle.spacingXXLarge
        anchors.rightMargin: TelamonStyle.spacingXXLarge
        catalog: page.catalog
        onOpenSources: page.openSources()
    }

    AppGrid {
        id: grid
        anchors.fill: parent
        anchors.leftMargin: TelamonStyle.spacingXXLarge
        anchors.rightMargin: TelamonStyle.spacingXXLarge
        visible: !blockedState.blocked
        model: page.model
        onOpened: appId => page.appRequested(appId)
        onMovementStarted: page.pinnedToTop = false

        header: Item {
            id: head
            width: grid.width
            height: column.implicitHeight + TelamonStyle.spacingLarge
            onHeightChanged: {
                if (page.pinnedToTop) {
                    grid.positionViewAtBeginning();
                }
            }

            ColumnLayout {
                id: column
                width: parent.width
                spacing: TelamonStyle.spacingLarge

                AppShelf {
                    Layout.fillWidth: true
                    title: qsTr("Popular in %1").arg(page.title)
                    apps: page.shelfApps
                    onAppRequested: appId => page.appRequested(appId)
                }

                RowLayout {
                    Layout.fillWidth: true
                    spacing: TelamonStyle.spacingXLarge

                    TelamonSegmentedControl {
                        model: [qsTr("Name"), qsTr("Recently Updated")]
                        currentIndex: page.sortIndex
                        Accessible.name: qsTr("Sort by")
                        onActivated: index => {
                            page.sortIndex = index;
                            page.run();
                        }
                    }

                    // Filters: each one narrows the list when checked.
                    TelamonChipGroup {
                        Layout.fillWidth: true
                        Layout.alignment: Qt.AlignVCenter

                        TelamonChip {
                            id: verifiedChip
                            text: qsTr("Verified")
                            checkable: true
                            checked: page.verifiedOnly
                            Accessible.description: qsTr("Only apps whose publisher Flathub has verified.")
                            onToggled: {
                                page.verifiedOnly = checked;
                                page.run();
                            }
                            TelamonToolTip {
                                text: qsTr("Only apps whose publisher Flathub has verified.")
                                shown: verifiedChip.hovered || verifiedChip.visualFocus
                            }
                        }
                        TelamonChip {
                            id: freeChip
                            text: qsTr("Free Software")
                            checkable: true
                            checked: page.freeOnly
                            Accessible.description: qsTr("Only apps with a free-software license, such as the GPL, MIT or Apache. Apps with a proprietary or unknown license are hidden.")
                            onToggled: {
                                page.freeOnly = checked;
                                page.run();
                            }
                            TelamonToolTip {
                                text: qsTr("Only apps with a free-software license, such as the GPL, MIT or Apache. Apps with a proprietary or unknown license are hidden.")
                                shown: freeChip.hovered || freeChip.visualFocus
                            }
                        }
                    }
                }
            }
        }
    }

    // Under the header, over the empty grid.
    TelamonEmptyState {
        anchors.left: grid.left
        anchors.right: grid.right
        anchors.bottom: grid.bottom
        anchors.top: grid.top
        anchors.topMargin: grid.headerItem ? grid.headerItem.height : 0
        visible: grid.visible && page.model.count === 0 && !page.model.busy
        symbol: Symbols.Storefront
        title: qsTr("No Apps")
        text: page.verifiedOnly || page.freeOnly ? qsTr("No apps in this category match the filters.") : qsTr("This category has no apps.")
    }
    TelamonSpinner {
        anchors.horizontalCenter: grid.horizontalCenter
        y: grid.y + (grid.headerItem ? grid.headerItem.height : 0) + TelamonStyle.spacingXXLarge
        visible: grid.visible && page.model.count === 0 && page.model.busy
        running: visible
    }
}
