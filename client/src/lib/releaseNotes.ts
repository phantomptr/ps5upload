// A release's notes from GitHub, for an update whose check came without them: the update notice's
// Details showed an empty dialog then. The release page's body is the same CHANGELOG.md section the
// update manifest carries.

const RELEASE_API = "https://api.github.com/repos/phantomptr/ps5upload/releases/tags/";

/** The notes GitHub has for `version` ("11.4.0" or "v11.4.0"). Throws when they cannot be read
 *  (offline, rate-limited, no such release). */
export async function fetchReleaseNotes(
  version: string,
  fetchImpl: typeof fetch = fetch,
  timeoutMs = 15_000,
): Promise<string> {
  const tag = `v${version.trim().replace(/^v/, "")}`;
  const res = await fetchImpl(`${RELEASE_API}${encodeURIComponent(tag)}`, {
    headers: { Accept: "application/vnd.github+json" },
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!res.ok) throw new Error(`GitHub HTTP ${res.status}`);
  const body = ((await res.json()) as { body?: unknown }).body;
  if (typeof body !== "string" || !body.trim()) throw new Error("the release has no notes");
  return body.replace(/\r\n/g, "\n");
}
