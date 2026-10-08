// The same bar as marathon's default config, written the idiomatic Quickshell
// way, reading the same Go/Python services over the same sockets.
// BENCH_STRESS=1 adds the high-frequency counter used by the stress run.
import QtQuick
import QtQuick.Layouts
import Quickshell

ShellRoot {
    id: shell

    readonly property var profileIcons: ({ "power-saver": "❦", "balanced": "◔", "performance": "⚡" })
    readonly property var profileLabels: ({ "power-saver": "economia", "balanced": "equilíbrio", "performance": "desempenho" })
    property bool showSeconds: false

    Service { id: sys; name: "sys"; subscriptions: ["clock", "load", "memory"] }
    Service { id: power; name: "power"; subscriptions: ["battery", "profile"] }
    Service { id: stress; name: "stress"; subscriptions: Quickshell.env("BENCH_STRESS") ? ["tick"] : [] }

    PanelWindow {
        anchors { top: true; left: true; right: true }
        implicitHeight: 32
        color: "#0b0e14"

        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 12
            spacing: 12

            Rectangle {
                implicitWidth: tag.implicitWidth + 16; implicitHeight: 22; radius: 4; color: "#e6b450"
                Text { id: tag; anchors.centerIn: parent; text: "marathon"; color: "#0b0e14"; font.pixelSize: 14 }
            }

            // clock
            RowLayout {
                spacing: 4
                Pill {
                    readonly property var c: sys.values.clock
                    icon: "◷"
                    label: c ? (shell.showSeconds ? c.hms : c.hm) : "--:--"
                    active: shell.showSeconds
                    onClicked: shell.showSeconds = !shell.showSeconds
                }
                Text { text: sys.values.clock?.date ?? ""; color: "#565b66"; font.pixelSize: 14 }
            }

            // sysload
            Meter {
                readonly property var l: sys.values.load
                icon: "◫"; label: l ? l.load1.toFixed(2) : "…"; value: l?.ratio ?? 0
            }
            Meter {
                readonly property var m: sys.values.memory
                icon: "▤"; label: m ? `${m.used_gb.toFixed(1)}/${m.total_gb.toFixed(0)}G` : "…"; value: m?.ratio ?? 0
            }

            // battery
            Pill {
                readonly property var b: power.values.battery
                icon: b ? (b.charging ? "ϟ" : "▮") : "!"
                label: b ? `${b.percent}%${b.simulated ? "~" : ""}` : "sem energia"
                active: b?.charging ?? false
            }

            // profile
            Pill {
                readonly property string n: power.values.profile?.name ?? "balanced"
                icon: shell.profileIcons[n] ?? "◔"
                label: shell.profileLabels[n] ?? n
                active: n === "performance"
                onClicked: power.call("cycle_profile", null)
            }

            Text {
                visible: !!Quickshell.env("BENCH_STRESS")
                text: "tick " + (stress.values.tick?.n ?? 0)
                color: "#bfbdb6"; font.pixelSize: 14
            }

            Item { Layout.fillWidth: true }
        }
    }
}
