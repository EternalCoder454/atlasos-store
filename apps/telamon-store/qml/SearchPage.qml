import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// Search: a field and a grid of the apps that match, as you type. The query
// is looked up on a worker (see AppListModel), at most 500 results.
// A FocusScope with the field as its focused child: the navigation stack hands
// the focus to a page it pushes (and again when its transition ends), and with
// a plain Item that took the keyboard away from the field after the first
// letter typed on Home.
FocusScope {
    id: page

    required property var catalog
    required property var model

    // What the field holds; set by whoever opens the page.
    property string query

    property string title: qsTr("Search")
    // This page is a search (Main.qml reuses it for the next one).
    readonly property bool isSearch: true

    signal appRequested(string appId)
    signal openSources

    function setQuery(text) {
        field.text = text;
        page.query = text;
        run();
    }

    function run() {
        if (!catalog.ready || catalog.appCount === 0 || page.query.trim().length === 0) {
            model.clear();
            return;
        }
        model.search(page.query);
    }

    Component.onCompleted: {
        field.text = page.query;
        run();
        field.forceActiveFocus();
    }

    Connections {
        target: page.catalog
        function onRevisionChanged() {
            page.run();
        }
    }

    // Typing waits a moment so a fast typist starts one search, not one per key.
    Timer {
        id: debounce
        interval: 150
        onTriggered: {
            page.query = field.text;
            page.run();
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.leftMargin: TelamonStyle.spacingXXLarge
        anchors.rightMargin: TelamonStyle.spacingXXLarge
        spacing: TelamonStyle.spacingLarge

        TelamonTextField {
            id: field
            Layout.fillWidth: true
            placeholderText: qsTr("Search Apps")
            clearable: true
            focus: true
            Accessible.name: qsTr("Search Apps")
            onTextEdited: debounce.restart()
            // Escape empties the field and the results; the focus stays.
            Keys.onEscapePressed: event => {
                debounce.stop();
                field.text = "";
                page.query = "";
                page.run();
                event.accepted = true;
            }
            Keys.onDownPressed: grid.forceActiveFocus()
        }

        CatalogState {
            id: gate
            Layout.fillWidth: true
            Layout.fillHeight: true
            catalog: page.catalog
            onOpenSources: page.openSources()
        }

        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
            visible: !gate.blocked

            AppGrid {
                id: grid
                anchors.fill: parent
                model: page.model
                visible: page.model.count > 0
                onOpened: appId => page.appRequested(appId)
            }
            TelamonEmptyState {
                anchors.fill: parent
                visible: page.model.count === 0 && !page.model.busy
                symbol: Symbols.Search
                title: page.query.trim().length === 0 ? qsTr("Search for Apps") : qsTr("No Results")
                text: page.query.trim().length === 0 ? qsTr("Type a name, a keyword or a developer.") : qsTr("No apps match “%1”.").arg(page.query.trim())
            }
            TelamonSpinner {
                anchors.centerIn: parent
                visible: page.model.count === 0 && page.model.busy
                running: visible
            }
        }
    }
}
