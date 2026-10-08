// Clock pill. Click toggles seconds. Data comes from the Python `sys` service.
//
// Components run once (Solid-style). Anything reactive is a function prop:
// label={() => ...}. Passing a signal itself (active={seconds}) also binds.

type Clock = { hm: string; hms: string; date: string };

export default function ClockWidget() {
  const sys = use("sys");
  const clock = sys.subscribe<Clock>("clock");
  const seconds = signal(false);

  const label = () => {
    const c = clock();
    if (!c) return "--:--";
    return seconds() ? c.hms : c.hm;
  };

  return (
    <Row gap={4}>
      <Pill icon="clock" label={label} active={seconds} onClick={() => seconds.update((s) => !s)} />
      <Text tone="muted" text={() => clock()?.date ?? ""} />
    </Row>
  );
}
