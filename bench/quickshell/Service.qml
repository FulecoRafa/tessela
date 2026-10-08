// Speaks the marathon socket protocol from QML (Quickshell.Io.Socket), so the
// Quickshell bar reads exactly the same services as the GPUI bar.
import QtQuick
import Quickshell
import Quickshell.Io

Socket {
    id: root

    required property string name
    property list<string> subscriptions: []
    // path -> latest value; null while the service is down
    property var values: ({})

    property int _nextId: 1
    property var _paths: ({})

    path: (Quickshell.env("MARATHON_SERVICES_DIR") ?? "/tmp") + "/" + name + ".sock"
    connected: true

    function _send(msg) {
        msg.id = _nextId++;
        write(JSON.stringify(msg) + "\n");
        flush();
        return msg.id;
    }

    function call(method, args) {
        _send({ op: "call", method: method, args: args ?? null });
    }

    onConnectionStateChanged: {
        if (connected) {
            for (const p of subscriptions)
                _paths[_send({ op: "subscribe", path: p })] = p;
        } else {
            values = ({});
            retry.start();
        }
    }
    onError: retry.start()

    parser: SplitParser {
        onRead: line => {
            const m = JSON.parse(line);
            if (m.event === undefined)
                return;
            const p = root._paths[m.id];
            if (p === undefined)
                return;
            const next = Object.assign({}, root.values);
            next[p] = m.event;
            root.values = next;
        }
    }

    property Timer retry: Timer {
        interval: 1000
        onTriggered: if (!root.connected) root.connected = true
    }
}
