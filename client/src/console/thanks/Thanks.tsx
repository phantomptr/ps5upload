import { useMemo, useState } from "react";
import { ArrowLeft, Heart } from "lucide-react";
import supporters from "../../data/supporters.json";
import { CRYPTO } from "../../data/donate";
import { COFFEE_URL } from "../../lib/supportLinks";
import { t } from "../strings";
import { encodeQr, qrPath } from "../ui/qr";

/**
 * Thanks & Support on the console (the integration design (e)): the same names as PS5Upload's
 * Thanks page (client/src/data/supporters.json, names only, most first, anonymous gifts only
 * counted) and the same ways to support (supportLinks.ts, donate.ts). Everything is in the page
 * when it is built, so it works with no network; the codes are drawn here (ui/qr.ts) for a phone
 * to scan: the TV's browser never leaves Minion to open a link.
 */

/** The site's Thanks section: the other ways to support. */
export const THANKS_PAGE_URL = "https://ps5upload.phantomptr.com/#thanks";

interface SupporterList {
  names: string[];
  anonymous: number;
}
const LIST = supporters as SupporterList;

function Qr({ text, label }: { text: string; label: string }) {
  const code = useMemo(() => {
    try {
      const q = encodeQr(text, "M");
      return { d: qrPath(q), n: q.size + 8 };
    } catch {
      return null;
    }
  }, [text]);
  if (!code) return null;
  return (
    <svg className="cw-thanks-qr" viewBox={`0 0 ${code.n} ${code.n}`} role="img" aria-label={label} shapeRendering="crispEdges">
      <rect width={code.n} height={code.n} fill="#fff" />
      <path d={code.d} fill="#000" />
    </svg>
  );
}

export function Thanks({ onBack, list = LIST }: { onBack?: () => void; list?: SupporterList }) {
  const [crypto, setCrypto] = useState(false);
  return (
    <div className="cw-settings cw-thanks" data-page="thanks">
      {onBack ? (
        <button type="button" className="cw-focusable cw-button cw-doc-back" data-action="back" onClick={onBack}>
          <ArrowLeft aria-hidden />
          <span>{t("cw_nav_settings")}</span>
        </button>
      ) : null}
      <h1 className="cw-h1">
        <Heart className="cw-thanks-heart" aria-hidden /> {t("cw_thanks_title")}
      </h1>
      <p>{t("cw_thanks_intro")}</p>
      <p className="cw-dim" data-thanks-counts>
        {t("cw_thanks_count_named", { n: list.names.length })}
        {list.anonymous > 0 ? ` · ${t("cw_thanks_count_anonymous", { n: list.anonymous })}` : ""}
      </p>

      <section className="cw-home-section">
        <h2 className="cw-h2">{t("cw_thanks_support_title")}</h2>
        <p className="cw-dim">{t("cw_thanks_scan")}</p>
        <div className="cw-thanks-codes">
          <figure>
            <Qr text={COFFEE_URL} label={t("cw_thanks_kofi")} />
            <figcaption>{t("cw_thanks_kofi")}</figcaption>
          </figure>
          <figure>
            <Qr text={THANKS_PAGE_URL} label={t("cw_thanks_more")} />
            <figcaption>{t("cw_thanks_more")}</figcaption>
          </figure>
        </div>
        <button
          type="button"
          className="cw-focusable cw-button"
          data-action="thanks-crypto"
          aria-expanded={crypto}
          onClick={() => setCrypto((v) => !v)}
        >
          <span>{t("cw_thanks_crypto_title")}</span>
        </button>
        {crypto ? (
          <ul className="cw-thanks-crypto">
            {CRYPTO.map((c) => (
              <li key={c.id} data-coin={c.id}>
                <Qr text={c.address} label={`${c.name} · ${t("cw_thanks_network_only", { coin: c.coin, network: c.network })}`} />
                <div className="cw-thanks-crypto-text">
                  <div className="cw-game-title">{c.name}</div>
                  <div className="cw-mono cw-thanks-address">{c.address}</div>
                  <div className="cw-dim">{t("cw_thanks_network_only", { coin: c.coin, network: c.network })}</div>
                  {c.id === "xrp" ? <div className="cw-dim">{t("cw_thanks_xrp_no_tag")}</div> : null}
                </div>
              </li>
            ))}
          </ul>
        ) : null}
        <p className="cw-dim cw-setting-note" data-thanks-voluntary="">
          {t("cw_thanks_voluntary")}
        </p>
      </section>

      <section className="cw-home-section">
        <h2 className="cw-h2">{t("cw_thanks_wall_title")}</h2>
        <ul className="cw-thanks-names" data-thanks-names>
          {list.names.map((name, i) => (
            <li key={`${i}:${name}`}>{name}</li>
          ))}
        </ul>
        {list.anonymous > 0 ? <p className="cw-dim">{t("cw_thanks_anonymous", { n: list.anonymous })}</p> : null}
      </section>
    </div>
  );
}
