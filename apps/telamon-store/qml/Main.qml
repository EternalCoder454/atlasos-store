import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// The Store's window: a sidebar of places, and a navigation stack per place
// for the pages opened from it (an app's page, a category).
TelamonWindow {
    id: root

    // The Rust backend (src/backend.rs); main.cpp sets it.
    required property var backend
    // The catalogs and the two lists of apps (src/catalog.rs): one for the
    // search page, one for a category page.
    required property var catalog
    required property var searchModel
    required property var browseModel
    // The Flatpak jobs and the installed list (src/jobs.rs).
    required property var jobs

    // The place shown: "home", "installed", "updates" or "sources".
    property string place: "home"

    title: TelamonApp.name
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

    // The Store's categories, in the order of Catalog.categoryKey.
    readonly property var categories: [
        { text: qsTr("Audio & Video"), symbol: Symbols.Movie },
        { text: qsTr("Development"), symbol: Symbols.Code },
        { text: qsTr("Education"), symbol: Symbols.School },
        { text: qsTr("Games"), symbol: Symbols.SportsEsports },
        { text: qsTr("Graphics"), symbol: Symbols.Brush },
        { text: qsTr("Network"), symbol: Symbols.Public },
        { text: qsTr("Office"), symbol: Symbols.Work },
        { text: qsTr("Science"), symbol: Symbols.Science },
        { text: qsTr("System"), symbol: Symbols.Settings },
        { text: qsTr("Utilities"), symbol: Symbols.Build }
    ]

    function openPlace(key) {
        root.place = key;
        stack.popToRoot();
    }

    // The search page, filled in; one search page at a time.
    function openSearch(text) {
        if (stack.currentItem && stack.currentItem.isSearch === true) {
            stack.currentItem.setQuery(text);
            return;
        }
        if (stack.currentItem && stack.currentItem.message === true) {
            stack.pop();
        }
        stack.push(searchPage, { query: text });
    }

    function openCategory(index) {
        const key = root.catalog.categoryKey(index);
        if (key.length === 0) {
            return;
        }
        stack.push(categoryPage, { categoryKey: key, title: root.categories[index].text });
    }

    // An app's page; not opened again when it already is the page shown.
    function openApp(id) {
        if (stack.currentItem && stack.currentItem.isApp === true && stack.currentItem.appId === id) {
            return;
        }
        if (stack.currentItem && stack.currentItem.message === true) {
            stack.pop();
        }
        stack.push(appPage, { appId: id });
    }

    // The Remove confirmation for an installed app.
    function askRemove(id, name, scope, ref) {
        // Every installation of the ID shares one data folder.
        const shared = (JSON.parse(root.jobs.appInfo(id)).installs ?? []).length > 1;
        removeDialog.show(id, name, scope, ref, shared);
    }

    // `--remove <id>`: the app's page with the confirmation open, once the
    // installed list is known (the user still confirms).
    property string pendingRemove: ""

    function openRemove(id) {
        root.openApp(id);
        if (root.jobs.installedReady) {
            root.askRemoveInstalled(id);
        } else {
            root.pendingRemove = id;
        }
    }

    function askRemoveInstalled(id) {
        // With several installations the user picks one on the page.
        const info = JSON.parse(root.jobs.appInfo(id));
        if ((info.installs ?? []).length !== 1) {
            return;
        }
        root.askRemove(id, info.name, info.installs[0].scope, info.installs[0].ref);
    }

    // What a launch asked for. Pages for apps, searches, files and links
    // come with the F phase; until then the request is shown, not lost, on
    // top of the place the window is on.
    readonly property var requestHeadings: ({
            app: qsTr("App Page"),
            remove: qsTr("Remove App"),
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
            if (kind === "search") {
                root.openPlace("home");
                root.openSearch(value);
                return;
            }
            if (kind === "app") {
                root.openApp(value);
                return;
            }
            if (kind === "remove") {
                root.openRemove(value);
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

    Connections {
        target: root.jobs
        function onPlanReady(appId) {
            installDialog.show();
        }
        function onRemoveBlocked(appId, fullRef) {
            const info = JSON.parse(root.jobs.appInfo(appId));
            const install = (info.installs ?? []).find(i => i.ref === fullRef);
            if (install) {
                removeDialog.showRunning(appId, info.name, install.scope, fullRef);
            }
        }
        function onUnusedReady(count) {
            if (count > 0) {
                unusedDialog.show();
            }
        }
        function onInstalledReadyChanged() {
            if (root.jobs.installedReady && root.pendingRemove.length > 0) {
                const id = root.pendingRemove;
                root.pendingRemove = "";
                root.askRemoveInstalled(id);
            }
        }
    }

    // Icons of installed apps come from the catalog: read the list again
    // when a new library arrives.
    Connections {
        target: root.catalog
        function onRevisionChanged() {
            root.jobs.refresh();
        }
    }

    InstallDialog {
        id: installDialog
        jobs: root.jobs
    }
    RemoveDialog {
        id: removeDialog
        jobs: root.jobs
    }
    UnusedDialog {
        id: unusedDialog
        jobs: root.jobs
    }

    RowLayout {
        anchors.fill: parent
        spacing: 0

        Item {
            Layout.fillHeight: true
            Layout.preferredWidth: root.sidebarCollapsed ? Kirigami.Units.gridUnit * 3.6 : Kirigami.Units.gridUnit * 12.5

            // Scrolls by itself when the window is too short for every place.
            TelamonSidebar {
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

            // About stays at the bottom, as in the other Telamon apps.
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

        TelamonNavigationStack {
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
                        case "installed":
                            return installedPage;
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
            catalog: root.catalog
            categories: root.categories
            onSearchRequested: text => root.openSearch(text)
            onCategoryRequested: index => root.openCategory(index)
            onOpenSources: root.openPlace("sources")
        }
    }
    Component {
        id: searchPage
        SearchPage {
            catalog: root.catalog
            model: root.searchModel
            onAppRequested: id => root.openApp(id)
            onOpenSources: root.openPlace("sources")
        }
    }
    Component {
        id: categoryPage
        CategoryPage {
            catalog: root.catalog
            model: root.browseModel
            onAppRequested: id => root.openApp(id)
            onOpenSources: root.openPlace("sources")
        }
    }
    Component {
        id: appPage
        AppPage {
            catalog: root.catalog
            jobs: root.jobs
            onRemoveRequested: (id, name, scope, ref) => root.askRemove(id, name, scope, ref)
        }
    }
    Component {
        id: installedPage
        InstalledPage {
            jobs: root.jobs
            onAppRequested: id => root.openApp(id)
            onRemoveRequested: (id, name, scope, ref) => root.askRemove(id, name, scope, ref)
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
        TelamonAboutPage {
            description: qsTr("Find, install and update apps for Telamon OS.")
        }
    }
}
