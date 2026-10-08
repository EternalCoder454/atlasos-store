pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// Home: a search field, Flathub's curated shelves (Popular Apps, New & Updated,
// Editor's Picks) and the categories. A shelf is shown only when it has apps
// the local catalog has; with no network and no cache Home is the search
// field and the categories. Typing in the field opens the search page.
TelamonPage {
    id: page

    required property var backend
    required property var catalog
    // Flathub's curated lists (src/featured.rs).
    required property var featured
    // [{ text, symbol }] in the order of Catalog.categoryKey.
    required property var categories
    // The Telamon apps connected to the Store (src/native.rs).
    required property var nativeApps

    title: qsTr("Home")

    signal searchRequested(string text)
    signal categoryRequested(int index)
    signal appRequested(string appId)
    signal openSources
    signal nativeListRequested

    // A shelf's apps from the JSON src/featured.rs publishes.
    function appsOf(json) {
        try {
            const apps = JSON.parse(json);
            return Array.isArray(apps) ? apps : [];
        } catch (e) {
            return [];
        }
    }

    // Shows the cached lists and lets the worker refresh the expired ones.
    // Nothing is asked for when there is no catalog to match against.
    function ask() {
        if (page.catalog.ready && page.catalog.appCount > 0) {
            page.featured.ensureHome();
        }
    }

    Component.onCompleted: page.ask()

    NativeAppsList {
        id: telamonApps
        json: page.nativeApps.appsJson
    }

    Connections {
        target: page.catalog
        function onRevisionChanged() {
            page.ask();
        }
    }

    TelamonTextField {
        id: field
        Layout.fillWidth: true
        placeholderText: qsTr("Search Apps")
        clearable: true
        Accessible.name: qsTr("Search Apps")
        onTextEdited: {
            if (text.length > 0) {
                const typed = text;
                // The search page takes over the text and the keyboard.
                text = "";
                page.searchRequested(typed);
            }
        }
        onAccepted: {
            if (text.trim().length > 0) {
                page.searchRequested(text);
                text = "";
            }
        }
    }

    // Sources that failed to load; the others still show. Plain text.
    Text {
        Layout.fillWidth: true
        visible: page.catalog.ready && page.catalog.appCount > 0 && page.catalog.errorText.length > 0
        text: page.catalog.errorText
        wrapMode: Text.Wrap
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeCaption
        color: TelamonStyle.warning
        textFormat: Text.PlainText
    }

    CatalogState {
        Layout.fillWidth: true
        Layout.preferredHeight: implicitHeight
        catalog: page.catalog
        onOpenSources: page.openSources()
    }

    // Apps made for Telamon OS that the Store installs for the user itself.
    AppShelf {
        Layout.fillWidth: true
        title: qsTr("Telamon Apps")
        apps: telamonApps.tiles
        onAppRequested: appId => page.appRequested(appId)
    }

    AppShelf {
        Layout.fillWidth: true
        title: qsTr("Popular Apps")
        apps: page.catalog.ready ? page.appsOf(page.featured.popularJson) : []
        onAppRequested: appId => page.appRequested(appId)
    }

    AppShelf {
        Layout.fillWidth: true
        title: qsTr("New & Updated")
        apps: page.catalog.ready ? page.appsOf(page.featured.newJson) : []
        onAppRequested: appId => page.appRequested(appId)
    }

    AppShelf {
        Layout.fillWidth: true
        title: qsTr("Editor's Picks")
        apps: page.catalog.ready ? page.appsOf(page.featured.picksJson) : []
        onAppRequested: appId => page.appRequested(appId)
    }

    Text {
        visible: (page.catalog.ready && page.catalog.appCount > 0) || telamonApps.tiles.length > 0
        text: qsTr("Categories")
        font.family: TelamonStyle.fontFamily
        font.pointSize: TelamonStyle.fontSizeHeading
        font.bold: true
        color: TelamonStyle.text
        textFormat: Text.PlainText
        Accessible.role: Accessible.Heading
    }

    GridLayout {
        Layout.fillWidth: true
        visible: (page.catalog.ready && page.catalog.appCount > 0) || telamonApps.tiles.length > 0
        columns: Math.max(1, Math.floor(width / (Kirigami.Units.gridUnit * 11)))
        columnSpacing: TelamonStyle.spacingLarge
        rowSpacing: TelamonStyle.spacingLarge

        Repeater {
            model: page.categories
            CategoryTile {
                required property var modelData
                required property int index
                Layout.fillWidth: true
                visible: page.catalog.ready && page.catalog.appCount > 0
                text: modelData.text
                symbol: modelData.symbol
                count: page.catalog.ready ? page.catalog.categoryCount(index) : -1
                onClicked: page.categoryRequested(index)
            }
        }

        // Telamon's own apps, as a place of their own.
        CategoryTile {
            Layout.fillWidth: true
            visible: telamonApps.tiles.length > 0
            text: qsTr("Telamon Apps")
            symbol: Symbols.Widgets
            count: telamonApps.tiles.length
            onClicked: page.nativeListRequested()
        }
    }
}
