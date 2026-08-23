/**
 * Drive and photograph the *real* Tauri webview, from outside, with no
 * dependencies and no second process.
 *
 *   node ui.mjs probe                        # is the app up? what is on screen?
 *   node ui.mjs shot /tmp/a.png [selector]   # screenshot viewport, or one element
 *   node ui.mjs click 'text=Agents'          # click by visible text
 *   node ui.mjs click '.chatrow'             # ...or by CSS selector
 *   node ui.mjs type 'textarea' 'hello'      # React-safe input
 *   node ui.mjs key 'textarea' Enter --ctrl
 *   node ui.mjs text '.statusbar'
 *   node ui.mjs eval 'location.hash'
 *   node ui.mjs await 'fetch("/api/agents").then(r=>r.status)'
 *   node ui.mjs wait 'document.querySelectorAll(".agentrow").length > 0' 5000
 *   node ui.mjs console 3000                 # collect console output for 3s
 *
 *   node ui.mjs do <<'EOF'                   # many steps, ONE connection
 *   click|text=Agents
 *   wait|document.title.length > 0
 *   shot|/tmp/agents.png
 *   EOF
 *
 * Requires the app launched with WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9224
 * (see SKILL.md). Node >= 22 — the global WebSocket is the only transport used.
 *
 * Prefer `do` over several invocations: it is one WebSocket and one process for
 * the whole sequence. Prefer `shot <path> <selector>` over a full-viewport
 * screenshot: a 5 KB strip costs a fraction of a 156 KB page in both wall clock
 * and the reading agent's context.
 */

const PORT = process.env.INSPECTOR_PORT || "9224";
const TIMEOUT_MS = Number(process.env.UI_TIMEOUT_MS || 30000);

// ---------------------------------------------------------------- transport

let sock, targetId = null, nextId = 1;
const pending = new Map();
const consoleLines = [];

function connect() {
  return new Promise((resolve, reject) => {
    sock = new WebSocket(`ws://127.0.0.1:${PORT}/socket/1/1/WebPage`);
    const timer = setTimeout(
      () => reject(new Error("timed out waiting for the inspector target")),
      TIMEOUT_MS
    );
    sock.addEventListener("error", () =>
      reject(
        new Error(
          `cannot reach the inspector on 127.0.0.1:${PORT}.\n` +
            "Launch the app with:\n" +
            "  WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9224 \\\n" +
            "    setsid nohup npm run app:alongside > /tmp/app.log 2>&1 < /dev/null &"
        )
      )
    );
    sock.addEventListener("message", (ev) => {
      const msg = JSON.parse(typeof ev.data === "string" ? ev.data : ev.data.toString());
      if (msg.method === "Target.targetCreated" && !targetId) {
        targetId = msg.params.targetInfo.targetId;
        clearTimeout(timer);
        resolve();
        return;
      }
      if (msg.method !== "Target.dispatchMessageFromTarget") return;
      const inner = JSON.parse(msg.params.message);
      if (inner.id && pending.has(inner.id)) {
        pending.get(inner.id)(inner.result ?? inner);
        pending.delete(inner.id);
      } else if (inner.method === "Console.messageAdded") {
        const m = inner.params.message;
        consoleLines.push(`[${m.level}] ${m.text}`);
      }
    });
  });
}

function send(method, params) {
  const id = nextId++;
  sock.send(
    JSON.stringify({
      id: nextId++,
      method: "Target.sendMessageToTarget",
      params: { targetId, message: JSON.stringify({ id, method, params }) },
    })
  );
  return new Promise((resolve) => pending.set(id, resolve));
}

/**
 * `awaitPromise: true` does NOT work here — WebKit answers
 * {type:"object", value:{}} with wasThrown:false, which is indistinguishable
 * from a call that returned nothing. Promises are parked on window and polled
 * (see `awaitExpr`); never "fix" this by passing awaitPromise.
 */
async function evaluate(expression) {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true });
  if (r?.wasThrown) throw new Error("threw: " + JSON.stringify(r.result?.description ?? r.result));
  return r?.result?.value;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------- helpers

/** `text=Label` matches the smallest element whose trimmed text starts with Label. */
function resolverJs(selector) {
  if (!selector.startsWith("text=")) return JSON.stringify(selector);
  const label = selector.slice(5);
  return `(() => {
    const want = ${JSON.stringify(label)};
    const cand = [...document.querySelectorAll("button,a,[role=button],[role=tab],label,li,summary")]
      .filter(e => e.offsetParent !== null && e.textContent.trim().startsWith(want));
    return cand.sort((a,b) => a.textContent.length - b.textContent.length)[0] || null;
  })()`;
}

/** Resolve to an element expression usable inside an evaluate. */
function elJs(selector) {
  return selector.startsWith("text=")
    ? resolverJs(selector)
    : `document.querySelector(${JSON.stringify(selector)})`;
}

async function requireEl(selector) {
  const ok = await evaluate(`!!(${elJs(selector)})`);
  if (!ok) throw new Error(`no element for ${selector}`);
}

// ---------------------------------------------------------------- commands

async function cmdProbe() {
  const out = await evaluate(`JSON.stringify({
    url: location.href, title: document.title,
    view: (document.querySelector("header, .titlebar")?.textContent || "").trim().slice(0,80),
    viewport: innerWidth + "x" + innerHeight, dpr: devicePixelRatio,
    theme: document.documentElement.getAttribute("data-theme") || "system",
    errors: [...document.querySelectorAll("[class*=error]")].map(e=>e.textContent.trim()).slice(0,3)
  })`);
  return JSON.parse(out);
}

async function cmdShot(path, selector) {
  if (!path) throw new Error("shot needs an output path");
  let shot;
  if (selector) {
    const doc = await send("DOM.getDocument", {});
    const node = await send("DOM.querySelector", {
      nodeId: doc.root.nodeId,
      selector,
    });
    if (!node?.nodeId) throw new Error(`no element for ${selector}`);
    shot = await send("Page.snapshotNode", { nodeId: node.nodeId });
  } else {
    const size = JSON.parse(await evaluate(`JSON.stringify({w:innerWidth,h:innerHeight})`));
    shot = await send("Page.snapshotRect", {
      x: 0, y: 0, width: size.w, height: size.h, coordinateSystem: "Viewport",
    });
  }
  if (!shot?.dataURL) throw new Error("no dataURL: " + JSON.stringify(shot).slice(0, 300));
  const b64 = shot.dataURL.split(",")[1];
  const fs = await import("node:fs");
  fs.writeFileSync(path, Buffer.from(b64, "base64"));
  return `${path} (${Math.round((b64.length * 0.75) / 1024)} KB)`;
}

async function cmdClick(selector) {
  await requireEl(selector);
  return await evaluate(`(() => {
    const el = ${elJs(selector)};
    el.scrollIntoView({block:"center"});
    el.click();
    return "clicked " + el.tagName.toLowerCase() + (el.className ? "." + String(el.className).split(" ")[0] : "");
  })()`);
}

/** React ignores a plain `.value =`; the native setter plus an input event is the only thing it sees. */
async function cmdType(selector, value) {
  await requireEl(selector);
  return await evaluate(`(() => {
    const el = ${elJs(selector)};
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value").set.call(el, ${JSON.stringify(value)});
    el.dispatchEvent(new Event("input", { bubbles: true }));
    return "typed " + ${JSON.stringify(value)}.length + " chars";
  })()`);
}

async function cmdKey(selector, key, mods) {
  await requireEl(selector);
  const init = JSON.stringify({
    key, bubbles: true, cancelable: true,
    ctrlKey: mods.includes("--ctrl"), metaKey: mods.includes("--meta"),
    shiftKey: mods.includes("--shift"), altKey: mods.includes("--alt"),
  });
  return await evaluate(`(() => {
    const el = ${elJs(selector)};
    el.focus();
    for (const t of ["keydown","keyup"]) el.dispatchEvent(new KeyboardEvent(t, ${init}));
    return "sent ${key}";
  })()`);
}

async function cmdText(selector) {
  await requireEl(selector);
  return await evaluate(`(${elJs(selector)}).textContent.trim().slice(0, 2000)`);
}

/** Poll a predicate rather than parking the runtime: a long-running evaluate blocks the page. */
async function cmdWait(predicate, ms = 10000) {
  const deadline = Date.now() + Number(ms);
  for (;;) {
    if (await evaluate(`!!(${predicate})`)) return "true";
    if (Date.now() > deadline) throw new Error(`wait timed out after ${ms}ms: ${predicate}`);
    await sleep(150);
  }
}

async function awaitExpr(expr) {
  const slot = "__ui_" + Date.now();
  await evaluate(`
    window.${slot} = { state: "pending" };
    Promise.resolve().then(() => (${expr}))
      .then(v => { window.${slot} = { state: "resolved", value: String(v) }; })
      .catch(e => { window.${slot} = { state: "rejected", error: String(e) }; });
    "fired"`);
  for (let i = 0; i < 120; i++) {
    await sleep(250);
    const parsed = JSON.parse(await evaluate(`JSON.stringify(window.${slot})`));
    if (parsed.state !== "pending") {
      await evaluate(`delete window.${slot}`);
      if (parsed.state === "rejected") throw new Error(parsed.error);
      return parsed.value;
    }
  }
  throw new Error("promise never settled");
}

async function cmdConsole(ms = 3000) {
  await send("Console.enable", {});
  await sleep(Number(ms));
  return consoleLines.join("\n") || "(no console output)";
}

async function dispatch(cmd, args) {
  switch (cmd) {
    case "probe":   return await cmdProbe();
    case "shot":    return await cmdShot(args[0], args[1]);
    case "click":   return await cmdClick(args[0]);
    case "type":    return await cmdType(args[0], args.slice(1).join(" "));
    case "key":     return await cmdKey(args[0], args[1], args.slice(2));
    case "text":    return await cmdText(args[0]);
    case "eval":    return await evaluate(args.join(" "));
    case "await":   return await awaitExpr(args.join(" "));
    case "wait":    return await cmdWait(args[0], args[1]);
    case "console": return await cmdConsole(args[0]);
    default:
      throw new Error(`unknown command "${cmd}" — see the header of ui.mjs`);
  }
}

// ---------------------------------------------------------------- entry

const argv = process.argv.slice(2);
if (argv.length === 0) {
  console.error("usage: node ui.mjs <probe|shot|click|type|key|text|eval|await|wait|console|do> …");
  process.exit(64);
}

try {
  await connect();

  if (argv[0] === "do") {
    // One connection for the whole script: `cmd|arg|arg` per line on stdin.
    const script = await new Promise((r) => {
      let buf = "";
      process.stdin.setEncoding("utf8");
      process.stdin.on("data", (d) => (buf += d));
      process.stdin.on("end", () => r(buf));
    });
    const steps = script.split("\n").map((l) => l.trim()).filter((l) => l && !l.startsWith("#"));
    for (const line of steps) {
      const [cmd, ...args] = line.split("|");
      const out = await dispatch(cmd.trim(), args);
      console.log(`${cmd.trim()}: ${typeof out === "string" ? out : JSON.stringify(out)}`);
    }
  } else {
    const out = await dispatch(argv[0], argv.slice(1));
    console.log(typeof out === "string" ? out : JSON.stringify(out, null, 1));
  }
  process.exit(0);
} catch (e) {
  console.error("FAIL: " + (e?.message ?? String(e)));
  process.exit(1);
}
