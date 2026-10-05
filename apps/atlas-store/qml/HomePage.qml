import QtQuick
import Atlas.Ui

// Home: search, editor's picks, popular, new and updated, and the categories.
// The skeleton only says what is coming.
AtlasPage {
    id: page

    required property var backend

    title: qsTr("Home")

    AtlasEmptyState {
        symbol: Symbols.Storefront
        title: qsTr("Atlas Store")
        text: qsTr("Apps from Flathub and your other sources will show here.")
    }
}
