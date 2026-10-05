import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Atlas.Ui

// Home: a search field and the categories. Popular, new and updated apps come
// with the Flathub API item. Typing in the field opens the search page.
AtlasPage {
    id: page

    required property var backend
    required property var catalog
    // [{ text, symbol }] in the order of Catalog.categoryKey.
    required property var categories

    title: qsTr("Home")

    signal searchRequested(string text)
    signal categoryRequested(int index)
    signal openSources

    AtlasTextField {
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
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeCaption
        color: AtlasStyle.warning
        textFormat: Text.PlainText
    }

    CatalogState {
        Layout.fillWidth: true
        Layout.preferredHeight: implicitHeight
        catalog: page.catalog
        onOpenSources: page.openSources()
    }

    Text {
        visible: page.catalog.ready && page.catalog.appCount > 0
        text: qsTr("Categories")
        font.family: AtlasStyle.fontFamily
        font.pointSize: AtlasStyle.fontSizeHeading
        font.bold: true
        color: AtlasStyle.text
        textFormat: Text.PlainText
        Accessible.role: Accessible.Heading
    }

    GridLayout {
        Layout.fillWidth: true
        visible: page.catalog.ready && page.catalog.appCount > 0
        columns: Math.max(1, Math.floor(width / (Kirigami.Units.gridUnit * 11)))
        columnSpacing: AtlasStyle.spacingLarge
        rowSpacing: AtlasStyle.spacingLarge

        Repeater {
            model: page.categories
            CategoryTile {
                required property var modelData
                required property int index
                Layout.fillWidth: true
                text: modelData.text
                symbol: modelData.symbol
                count: page.catalog.ready ? page.catalog.categoryCount(index) : -1
                onClicked: page.categoryRequested(index)
            }
        }
    }
}
