// A fetch Response whose body is never read keeps its buffer alive in Windows WebView2 (about
// 2 MB a call, off the JS heap), so a poll that returns early on a status leaks steadily. Every
// path that is done with a response without reading it cancels the body here.

/** Cancel a response body nobody will read. Never throws; a body already read or locked is left. */
export async function discardBody(res: Response | null | undefined): Promise<void> {
  const body = res?.body;
  if (!body || res.bodyUsed || body.locked) return;
  await body.cancel().catch(() => {});
}
