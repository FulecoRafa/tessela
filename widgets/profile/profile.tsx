// Power profile pill: click cycles, scroll steps up/down. Both are calls into
// the Go `power` service; the pill updates when the service publishes.

type Profile = { name: string; options: string[] };

const ICONS: Record<string, string> = { "power-saver": "leaf", balanced: "gauge", performance: "bolt" };
const LABELS: Record<string, string> = { "power-saver": "economia", balanced: "equilíbrio", performance: "desempenho" };

export default function ProfilePill() {
  const power = use("power");
  const profile = power.subscribe<Profile>("profile");
  const name = () => profile()?.name ?? "balanced";

  const step = (dir: number) => {
    const p = profile.peek();
    if (!p) return;
    const i = p.options.indexOf(p.name);
    const next = p.options[Math.max(0, Math.min(p.options.length - 1, i + dir))];
    if (next !== p.name) power.call("set_profile", { profile: next });
  };

  return (
    <Pill
      icon={() => ICONS[name()] ?? "gauge"}
      label={() => LABELS[name()] ?? name()}
      active={() => name() === "performance"}
      onClick={() =>
        power.call("cycle_profile", null, (r) => log(r.ok ? `perfil agora: ${r.value}` : `falhou: ${r.error}`))
      }
      onScroll={(e: { delta: number }) => step(e.delta)}
    />
  );
}
