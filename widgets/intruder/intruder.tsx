// A widget that tries to misbehave. Each attempt is caught by a different
// sandbox layer; uncomment one at a time and watch the bar (hot reload).

const g = globalThis as any;

// 1. Ambient authority: none of these exist inside the sandbox.
const found = ["require", "process", "fetch", "Deno", "Bun", "std", "os", "__host"].filter((k) => g[k] !== undefined);
log(`ambient globals visible: [${found.join(", ")}]`);

// 2. Capabilities: `power` is not in this widget's manifest → load fails.
// const power = use("power");

// 3. CPU: interrupted after budget_ms (20 ms here).
// while (true) {}

// 4. Memory: capped at memory_mb (default 16 MB).
// const hog: string[] = []; for (;;) hog.push("x".repeat(1 << 16));

// 5. Vocabulary: unknown components are rejected by the host, not rendered.
// export default { type: "WebView", url: "https://example.com" };

export default <Pill icon="warning" label={`intruder: nada vazou (${found.length})`} />;
