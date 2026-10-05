import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Atlas.Ui

// The Store's window: a sidebar of places, and a navigation stack per place
// for the pages opened from it (an app's page, a category).
AtlasWindow {
    id: root

    // The Rust backend (src/backend.rs); main.cpp sets it.
    required property var backend

    // The place shown: "home", "installed", "updates" or "sources".
    property string place: "home"

    title: AtlasApp.name
    width: Kirigami.Units.gridUnit * 64
    height: Kirigami.Units.gridUnit * 42
    minimumWidth: Kirigami.Units.gridUnit * 24
    minimumHeight: Kirigami.Units.gridUnit * 20
    stateKey: "main"
    visible: true
    LayoutMirroring.enabled: Qt.application.layoutDirection === Qt.RightToLeft
    LayoutMirroring.childrenInherit: true

    readonly property var places: [
        { key: "home", text: qsTr("Home"), symbol: Symbols.Storefront },
        { key: "installed", text: qsTr("Installed"), symbol: Symbols.Apps },
        { key: "updates", text: qsTr("Updates"), symbol: Symbols.Update },
        { key: "sources", text: qsTr("Sources"), symbol: Symbols.Dns }
    ]

    function openPlace(key) {
        root.place = key;
        stack.popToRoot();
    }

    // What a launch asked for. Pages for apps, searches, files and links
    // come with the F phase; until then the request is shown, not lost, on
    // top of the place the window is on.
    readonly property var requestHeadings: ({
            app: qsTr("App Page"),
            search: qsTr("Search"),
            ref: qsTr("App from a File"),
            repo: qsTr("Source from a File"),
            bundle: qsTr("App Bundle"),
            rpm: qsTr("RPM Package"),
            refUrl: qsTr("App from a Link")
        })

    // Shows what a launch asked for over the current place. A launch emits
    // its refusals and requests one after another; they are gathered and
    // shown as one page once it is done, and that page replaces the one an
    // earlier launch left, so launches from other processes can't pile pages
    // up or hide each other's refusals.
    property var pendingMessages: []

    function showMessage(item) {
        if (pendingMessages.length === 0) {
            Qt.callLater(root.flushMessages);
        }
        pendingMessages.push(item);
    }

    function flushMessages() {
        const items = pendingMessages;
        pendingMessages = [];
        if (items.length === 0) {
            return;
        }
        const refused = items.find(i => i.refused === true);
        const properties = items.length === 1 ? items[0] : {
            title: refused ? qsTr("Could Not Open") : qsTr("Not Yet Available"),
            symbol: refused ? Symbols.Error : Symbols.Construction,
            heading: refused ? refused.heading : qsTr("Not built yet"),
            text: items.map(i => i.heading + ": " + i.text).join("\n")
        };
        if (stack.currentItem && stack.currentItem.message === true) {
            stack.pop();
        }
        stack.push(placeholder, {
            message: true,
            title: properties.title,
            symbol: properties.symbol ?? Symbols.Construction,
            heading: properties.heading,
            text: properties.text
        });
    }

    Connections {
        target: root.backend
        function onRequested(kind, value) {
            if (kind === "page") {
                root.openPlace(value);
                return;
            }
            root.showMessage({
                title: qsTr("Not Yet Available"),
                heading: root.requestHeadings[kind] ?? kind,
                text: value
            });
        }
        function onRefused(text) {
            root.showMessage({
                refused: true,
                title: qsTr("Could Not Open"),
                symbol: Symbols.Error,
                heading: qsTr("The Store can't open this"),
                text: text
            });
        }
    }

    RowLayout {
        anchors.fill: parent
        spacing: 0

        Item {
            Layout.fillHeight: true
            Layout.preferredWidth: root.sidebarCollapsed ? Kirigami.Units.gridUnit * 3.6 : Kirigami.Units.gridUnit * 12.5

            // Scrolls by itself when the window is too short for every place.
            AtlasSidebar {
                id: sidebar
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.bottom: footer.top
                anchors.rightMargin: 1
                compact: root.sidebarCollapsed
                padding: Kirigami.Units.largeSpacing
                spacing: 2

                Repeater {
                    model: root.places
                    SidebarItem {
                        required property var modelData
                        Layout.fillWidth: true
                        text: modelData.text
                        symbol: modelData.symbol
                        compact: sidebar.compact
                        selected: root.place === modelData.key
                        onClicked: root.openPlace(modelData.key)
                    }
                }
            }

            // About stays at the bottom, as in the other Atlas apps.
            Rectangle {
                id: footer
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.rightMargin: 1
                anchors.bottom: parent.bottom
                height: footerColumn.implicitHeight + Kirigami.Units.largeSpacing * 2
                color: sidebar.baseColor

                ColumnLayout {
                    id: footerColumn
                    anchors.fill: parent
                    anchors.margins: Kirigami.Units.largeSpacing
                    spacing: 2

                    SidebarItem {
                        Layout.fillWidth: true
                        text: qsTr("About Store")
                        symbol: Symbols.Info
                        compact: sidebar.compact
                        selected: root.place === "about"
                        onClicked: root.openPlace("about")
                    }
                }
            }

            Rectangle {
                anchors.right: parent.right
                height: parent.height
                width: 1
                color: Qt.alpha(Kirigami.Theme.textColor, 0.12)
            }
        }

        AtlasNavigationStack {
            id: stack
            Layout.fillWidth: true
            Layout.fillHeight: true
            showHeader: stack.canGoBack
            initialItem: placeView

            Component {
                id: placeView
                Loader {
                    sourceComponent: {
                        switch (root.place) {
                        case "home":
                            return homePage;
                        case "about":
                            return aboutPage;
                        default:
                            return placeholderPlace;
                        }
                    }
                }
            }
        }
    }

    Component {
        id: homePage
        HomePage {
            backend: root.backend
        }
    }
    Component {
        id: placeholderPlace
        PlaceholderPage {
            title: root.places.find(p => p.key === root.place)?.text ?? ""
            heading: qsTr("Not Built Yet")
            text: qsTr("Coming in the F phase.")
        }
    }
    Component {
        id: placeholder
        PlaceholderPage {}
    }
    Component {
        id: aboutPage
        AtlasAboutPage {
            description: qsTr("Find, install and update apps for AtlasOS.")
        }
    }
}
