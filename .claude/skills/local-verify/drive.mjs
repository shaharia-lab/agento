/**
 * Drive the real Tauri webview through WebKit's remote inspector.
 *
 *   node drive.mjs '<js expression>'        # evaluate, print the value
 *   node drive.mjs --await '<js promise>'   # park the promise, poll, print settled value
 *   node drive.mjs --console                # stream console messages until Ctrl-C
 *
 * Requires the app launched with WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9224.
 * No dependencies: Node >= 22 has a global WebSocket. (Do NOT reach for `ws`
 * via playwright-core — e2e/node_modules is usually not installed.)
 */

const PORT = process.env.INSPECTOR_PORT || "9224";
const args = process.argv.slice(2);
const mode = args[0] === "--await" || args[0] === "--console" ? args[0] : "--eval";
const EXPR = mode === "--eval" ? args[0] : args[1];

const sock = new WebSocket(`ws://127.0.0.1:${PORT}/socket/1/1/WebPage`);
let targetId = null;
let nextId = 1;
const pending = new Map();

function send(method, params) {
  const id = nextId++;
  const inner = JSON.stringify({ id, method, params });
  sock.send(
    JSON.stringify({
      id: nextId++,
      method: "Target.sendMessageToTarget",
      params: { targetId, message: inner },
    })
  );
  return new Promise((resolve) => pending.set(id, resolve));
}

async function evaluate(expression) {
  // returnByValue only serializes plain values. `awaitPromise: true` does NOT
  // work here — WebKit returns {type:"object", value:{}} with wasThrown:false,
  // which reads exactly like success. Park promises and poll instead.
  const r = await send("Runtime.evaluate", { expression, returnByValue: true });
  if (r?.wasThrown) throw new Error("threw: " + JSON.stringify(r.result));
  return r?.result?.value;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function run() {
  if (mode === "--console") {
    await send("Console.enable");
    return; // messages stream via the handler below
  }

  if (mode === "--await") {
    const slot = "__probe_" + Date.now();
    await evaluate(`
      window.${slot} = { state: "pending" };
      Promise.resolve().then(() => (${EXPR}))
        .then((v) => { window.${slot} = { state: "resolved", value: String(v) }; })
        .catch((e) => { window.${slot} = { state: "rejected", error: String(e) }; });
      "fired"
    `);
    for (let i = 0; i < 40; i++) {
      await sleep(250);
      const out = await evaluate(`JSON.stringify(window.${slot})`);
      const parsed = JSON.parse(out);
      if (parsed.state !== "pending") {
        await evaluate(`delete window.${slot}`);
        console.log(JSON.stringify(parsed, null, 1));
        process.exit(parsed.state === "rejected" ? 3 : 0);
      }
    }
    console.error("TIMEOUT: promise never settled");
    process.exit(2);
  }

  console.log(JSON.stringify(await evaluate(EXPR), null, 1));
  process.exit(0);
}

sock.addEventListener("message", async (ev) => {
  const raw = typeof ev.data === "string" ? ev.data : ev.data.toString();
  const msg = JSON.parse(raw);

  if (msg.method === "Target.targetCreated" && !targetId) {
    targetId = msg.params.targetInfo.targetId;
    run().catch((e) => {
      console.error(String(e));
      process.exit(1);
    });
    return;
  }

  if (msg.method === "Target.dispatchMessageFromTarget") {
    const inner = JSON.parse(msg.params.message);
    if (inner.id && pending.has(inner.id)) {
      pending.get(inner.id)(inner.result ?? inner);
      pending.delete(inner.id);
    } else if (inner.method === "Console.messageAdded") {
      const m = inner.params.message;
      console.log(`[${m.level}] ${m.text}`);
    }
  }
});

sock.addEventListener("error", () => {
  console.error(`WS ERROR — is the app running with WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:${PORT}?`);
  process.exit(1);
});

if (mode !== "--console") {
  setTimeout(() => {
    console.error("TIMEOUT waiting for the inspector target");
    process.exit(2);
  }, 30000);
}
