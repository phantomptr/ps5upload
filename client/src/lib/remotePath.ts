// A file on a saved server travels through the app as `remote://<connection-id>/<path>`. Only the
// engine reads it; the app passes it along like any path and shows it by the server's name.

const SCHEME = "remote://";

export function isRemotePath(p: string | null | undefined): p is string {
  return typeof p === "string" && p.startsWith(SCHEME);
}

export function parseRemotePath(p: string): { connectionId: string; path: string } | null {
  if (!isRemotePath(p)) return null;
  const rest = p.slice(SCHEME.length);
  const slash = rest.indexOf("/");
  const connectionId = slash < 0 ? rest : rest.slice(0, slash);
  if (!connectionId) return null;
  const path = slash < 0 ? "/" : rest.slice(slash) || "/";
  return { connectionId, path };
}

export function remotePath(connectionId: string, path: string): string {
  const inner = path.replace(/^\/+|\/+$/g, "");
  return inner ? `${SCHEME}${connectionId}/${inner}` : `${SCHEME}${connectionId}`;
}

/** "NAS › games/ps5/Minecraft.pkg"; a local path comes back unchanged. */
export function displayPath(p: string, nameOf: (id: string) => string | undefined): string {
  const parsed = parseRemotePath(p);
  if (!parsed) return p;
  const server = nameOf(parsed.connectionId) ?? parsed.connectionId;
  const inner = parsed.path.replace(/^\/+|\/+$/g, "");
  return inner ? `${server} › ${inner}` : server;
}
