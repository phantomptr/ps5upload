/** True when an error is the app failing to reach its own engine, not the console. The
 *  desktop's command proxies say "error sending request for url (http://127.0.0.1:…)", which
 *  used to be reported as the PS5's helper not starting. */
export function isEngineUnreachable(error: string): boolean {
  return /error sending request for url \(https?:\/\/(127\.\d+\.\d+\.\d+|localhost|\[::1\])[:/]/i.test(
    error,
  );
}
