import { useEffect, useRef, useState } from "react";
import { Network, RefreshCw, Loader2 } from "lucide-react";
import { netInterfacesGet, type NetInterfaceList } from "../../api/ps5";
import { Button } from "../../components";
import { useTr } from "../../state/lang";

export default function NetworkPanel({ mgmtAddr }: { mgmtAddr: string }) {
  const tr = useTr();
  const [data, setData] = useState<NetInterfaceList | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Token-guard so a slow in-flight fetch that completes after the
  // addr changes (console switch) or the panel unmounts cannot write
  // stale data over fresh data, or setState on an unmounted component.
  // Each refresh call bumps the token; only the latest call's result
  // is applied.
  const reqIdRef = useRef(0);

  useEffect(() => {
    return () => {
      // eslint-disable-next-line react-hooks/exhaustive-deps
      reqIdRef.current++;
    };
  }, []);

  async function refresh() {
    const myId = ++reqIdRef.current;
    setLoading(true);
    setError(null);
    try {
      const d = await netInterfacesGet(mgmtAddr);
      if (myId !== reqIdRef.current) return;
      setData(d);
    } catch (e) {
      if (myId !== reqIdRef.current) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (myId === reqIdRef.current) setLoading(false);
    }
  }

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mgmtAddr]);

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center gap-2">
        <Network size={14} />
        <h3 className="flex-1 text-sm font-semibold">
          {tr("network_panel_title", undefined, "Network interfaces")}
        </h3>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={
            loading ? (
              <Loader2 size={11} className="animate-spin" />
            ) : (
              <RefreshCw size={11} />
            )
          }
          onClick={refresh}
          disabled={loading}
        >
          {tr("refresh", undefined, "Refresh")}
        </Button>
      </header>
      {error && (
        <div className="rounded-md border border-[var(--color-bad)] p-2 text-xs text-[var(--color-bad)]">
          {error}
        </div>
      )}
      {data && data.interfaces.length === 0 && (
        <div className="text-xs text-[var(--color-muted)]">
          {tr("network_panel_empty", undefined, "No interfaces reported.")}
        </div>
      )}
      {data && data.interfaces.length > 0 && (
        <div className="overflow-x-auto">
        <table className="w-full min-w-[420px] text-xs">
          <thead className="text-[var(--color-muted)]">
            <tr>
              <th className="px-1 py-0.5 text-left">
                {tr("network_panel_name", undefined, "Name")}
              </th>
              <th className="px-1 py-0.5 text-left">
                {tr("network_panel_ipv4", undefined, "IPv4")}
              </th>
              <th className="px-1 py-0.5 text-left">
                {tr("network_panel_mac", undefined, "MAC")}
              </th>
              <th className="px-1 py-0.5 text-right">
                {tr("network_panel_mtu", undefined, "MTU")}
              </th>
            </tr>
          </thead>
          <tbody>
            {data.interfaces.map((i) => (
              <tr
                key={i.name}
                className="border-t border-[var(--color-border)] font-mono"
              >
                <td className="px-1 py-0.5">{i.name}</td>
                <td className="px-1 py-0.5">{i.ipv4}</td>
                <td className="px-1 py-0.5 text-xs">{i.mac}</td>
                <td className="px-1 py-0.5 text-right tabular-nums">
                  {i.mtu}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      )}
    </section>
  );
}
