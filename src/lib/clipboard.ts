/**
 * Copy text, with the fallback the async Clipboard API needs.
 *
 * `navigator.clipboard` is gated on a secure context, and both origins this app
 * runs on qualify — `http://127.0.0.1:<port>` in release and
 * `http://localhost:1420` in dev are "potentially trustworthy" by the spec, the
 * same carve-out that makes loopback development work at all. What is *not*
 * guaranteed is the permission: WebKitGTK has shipped builds that reject
 * `writeText` outside a user gesture it recognises, and a rejected promise here
 * is a copy button that silently does nothing.
 *
 * So the deprecated `execCommand` path stays as the fallback. It is synchronous
 * and works from any click handler, at the cost of touching the selection —
 * which is why it runs second rather than first.
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return legacyCopy(text);
  }
}

function legacyCopy(text: string): boolean {
  const area = document.createElement("textarea");
  area.value = text;
  // Off-screen rather than `display: none`: a hidden element cannot be
  // selected, and an unselected one cannot be copied.
  area.style.position = "fixed";
  area.style.top = "-1000px";
  area.setAttribute("readonly", "");
  document.body.appendChild(area);
  try {
    area.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    document.body.removeChild(area);
  }
}
