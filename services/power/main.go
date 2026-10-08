// power: battery state and power profile, in Go.
//
// Reads /sys/class/power_supply/BAT* when present; otherwise simulates a
// battery so the demo works in a VM or container. The profile is kept in
// memory here; a real service would call power-profiles-daemon over D-Bus.
package main

import (
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"marathon.dev/sdk/marathon"
)

var profiles = []string{"power-saver", "balanced", "performance"}

type battery struct {
	Percent   int    `json:"percent"`
	Charging  bool   `json:"charging"`
	Simulated bool   `json:"simulated"`
	Source    string `json:"source"`
}

func readSysfs() (battery, bool) {
	dirs, _ := filepath.Glob("/sys/class/power_supply/BAT*")
	for _, d := range dirs {
		capRaw, err := os.ReadFile(filepath.Join(d, "capacity"))
		if err != nil {
			continue
		}
		statusRaw, _ := os.ReadFile(filepath.Join(d, "status"))
		pct, err := strconv.Atoi(strings.TrimSpace(string(capRaw)))
		if err != nil {
			continue
		}
		status := strings.TrimSpace(string(statusRaw))
		return battery{Percent: pct, Charging: status == "Charging" || status == "Full", Source: filepath.Base(d)}, true
	}
	return battery{}, false
}

func main() {
	svc := marathon.New("power")
	b, real := readSysfs()
	if !real {
		b = battery{Percent: 64, Charging: false, Simulated: true, Source: "simulated"}
	}
	bat := svc.Topic("battery", b)
	profile := svc.Topic("profile", map[string]any{"name": "balanced", "options": profiles})

	svc.Method("set_profile", func(args json.RawMessage) (any, error) {
		var a struct {
			Profile string `json:"profile"`
		}
		if err := json.Unmarshal(args, &a); err != nil {
			return nil, err
		}
		for _, p := range profiles {
			if p == a.Profile {
				profile.Publish(map[string]any{"name": p, "options": profiles})
				log.Printf("profile -> %s", p)
				return p, nil
			}
		}
		return nil, fmt.Errorf("unknown profile %q", a.Profile)
	})
	svc.Method("cycle_profile", func(json.RawMessage) (any, error) {
		cur := profile.Value().(map[string]any)["name"].(string)
		next := profiles[0]
		for i, p := range profiles {
			if p == cur {
				next = profiles[(i+1)%len(profiles)]
			}
		}
		profile.Publish(map[string]any{"name": next, "options": profiles})
		log.Printf("profile -> %s", next)
		return next, nil
	})

	go func() {
		for range time.Tick(2 * time.Second) {
			if nb, ok := readSysfs(); ok {
				bat.Publish(nb)
				continue
			}
			// Simulation: drain to 15%, then charge to full, and repeat.
			cur := bat.Value().(battery)
			switch {
			case cur.Charging && cur.Percent >= 100:
				cur.Charging = false
			case !cur.Charging && cur.Percent <= 15:
				cur.Charging = true
			case cur.Charging:
				cur.Percent += 3
			default:
				cur.Percent--
			}
			cur.Percent = min(cur.Percent, 100)
			bat.Publish(cur)
		}
	}()

	if err := svc.Serve(); err != nil {
		log.Fatal(err)
	}
}
