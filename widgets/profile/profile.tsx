// Power profile pill, showing both kinds of event props:
//  - click is a declarative action: the host calls the Go `power` service
//    directly, no extension code runs;
//  - scroll is a handler: it picks the next profile, calls the service and
//    flashes a check mark for a moment using a timer.

type Profile = { name: string; options: string[] };

const ICONS: Record<string, string> = { "power-saver": "leaf", balanced: "gauge", performance: "bolt" };
const LABELS: Record<string, string> = { "power-saver": "economia", balanced: "equilíbrio", performance: "desempenho" };

export default function ProfilePill() {
  const power = use("power");
  const profile = power.subscribe<Profile>("profile");
  const flash = signal(false);
  const name = () => profile()?.name ?? "balanced";

  const step = (e: { delta: number }) => {
    const p = profile.peek();
    if (!p) return;
    const i = p.options.indexOf(p.name);
    const next = p.options[Math.max(0, Math.min(p.options.length - 1, i + e.delta))];
    if (next === p.name) return;
    power.call("set_profile", { profile: next }, (r) => {
      if (!r.ok) return log(`falhou: ${r.error}`);
      log(`perfil agora: ${r.value}`);
      flash.set(true);
      after(1200, () => flash.set(false));
    });
  };

  return (
    <Pill
      icon={() => (flash() ? "check" : ICONS[name()] ?? "gauge")}
      label={() => LABELS[name()] ?? name()}
      active={() => name() === "performance"}
      onClick={power.action("cycle_profile")}
      onScroll={step}
    />
  );
}
