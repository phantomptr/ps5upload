// Play Rooms: the 12 coarse regions a room may show (design 3.1), the same fixed list and codes as
// the hub (ps5upload-hub geo.rs `Region`), and the "about N ms" estimate between two of them
// (design 6.3, the hub's `est_ms`). A region is never a country and never an address: the host's
// computer picks one from its own time zone, and the host can change or hide it.

export type Region =
  | "na-west"
  | "na-east"
  | "central-am"
  | "south-am"
  | "eu-west"
  | "eu-east"
  | "middle-east"
  | "africa"
  | "asia-south"
  | "asia-east"
  | "asia-se"
  | "oceania";

export const REGIONS: readonly Region[] = [
  "na-west",
  "na-east",
  "central-am",
  "south-am",
  "eu-west",
  "eu-east",
  "middle-east",
  "africa",
  "asia-south",
  "asia-east",
  "asia-se",
  "oceania",
];

/** A region code from the hub, the engine or a saved setting; anything else is null. */
export function readRegion(v: unknown): Region | null {
  return typeof v === "string" && (REGIONS as readonly string[]).includes(v) ? (v as Region) : null;
}

const CONTINENT: Record<Region, number> = {
  "na-west": 0,
  "na-east": 0,
  "central-am": 0,
  "south-am": 1,
  "eu-west": 2,
  "eu-east": 2,
  "middle-east": 3,
  africa: 4,
  "asia-south": 5,
  "asia-east": 5,
  "asia-se": 5,
  oceania: 6,
};

/** Neighbouring regions (the hub's table, both ways). */
const NEIGHBOURS: ReadonlyArray<[Region, Region]> = [
  ["na-west", "na-east"],
  ["na-west", "central-am"],
  ["na-east", "central-am"],
  ["central-am", "south-am"],
  ["eu-west", "eu-east"],
  ["eu-east", "middle-east"],
  ["eu-west", "africa"],
  ["middle-east", "africa"],
  ["middle-east", "asia-south"],
  ["asia-south", "asia-se"],
  ["asia-se", "asia-east"],
  ["asia-se", "oceania"],
];

export function neighbours(a: Region, b: Region): boolean {
  return NEIGHBOURS.some(([x, y]) => (x === a && y === b) || (x === b && y === a));
}

/** "Near me": the same region or a neighbour. */
export function nearRegion(a: Region | null, b: Region | null): boolean {
  return !!a && !!b && (a === b || neighbours(a, b));
}

/** The round trip one can expect between two regions, in ms (an estimate, shown as "about"):
 *  same region 30, neighbours 70, same continent 100, else 160. Null when either is unknown. */
export function regionEstMs(a: Region | null, b: Region | null): number | null {
  if (!a || !b) return null;
  if (a === b) return 30;
  if (neighbours(a, b)) return 70;
  if (CONTINENT[a] === CONTINENT[b]) return 100;
  return 160;
}

// Zone names (the part after "America/") for the time-zone guess. Only what the offset can't tell.
const SOUTH_AM =
  /^(Sao_Paulo|Buenos_Aires|Argentina\/.*|Santiago|Lima|Bogota|Caracas|Montevideo|La_Paz|Asuncion|Guayaquil|Cayenne|Paramaribo|Guyana|Recife|Fortaleza|Belem|Manaus|Bahia|Maceio|Cuiaba|Campo_Grande|Porto_Velho|Boa_Vista|Rio_Branco|Araguaina|Santarem|Noronha|Punta_Arenas|Eirunepe)$/;
const CENTRAL_AM =
  /^(Mexico_City|Cancun|Merida|Monterrey|Matamoros|Chihuahua|Ciudad_Juarez|Ojinaga|Mazatlan|Bahia_Banderas|Hermosillo|Tijuana|Guatemala|Belize|El_Salvador|Tegucigalpa|Managua|Costa_Rica|Panama|Havana|Jamaica|Port-au-Prince|Santo_Domingo|Puerto_Rico|Nassau|Barbados|Port_of_Spain|Antigua|Dominica|Grenada|St_Kitts|St_Lucia|St_Vincent|Aruba|Curacao|Kralendijk|Lower_Princes|Cayman|Grand_Turk|Tortola|St_Thomas|Anguilla|Montserrat|Guadeloupe|Martinique|St_Barthelemy|Marigot)$/;
const EU_EAST =
  /^(Warsaw|Prague|Bratislava|Budapest|Ljubljana|Zagreb|Sarajevo|Belgrade|Podgorica|Skopje|Tirane|Bucharest|Sofia|Athens|Nicosia|Chisinau|Kiev|Kyiv|Minsk|Vilnius|Riga|Tallinn|Moscow|Istanbul|Kaliningrad|Samara|Volgograd|Kirov|Ulyanovsk|Astrakhan|Saratov|Simferopol|Uzhgorod|Zaporozhye)$/;
const MIDDLE_EAST =
  /^(Jerusalem|Tel_Aviv|Gaza|Hebron|Amman|Beirut|Damascus|Baghdad|Tehran|Kuwait|Riyadh|Bahrain|Qatar|Dubai|Muscat|Aden|Tbilisi|Yerevan|Baku)$/;
const ASIA_SOUTH =
  /^(Kolkata|Calcutta|Karachi|Dhaka|Colombo|Kathmandu|Katmandu|Thimphu|Kabul|Almaty|Qostanay|Aqtobe|Aqtau|Atyrau|Oral|Qyzylorda|Tashkent|Samarkand|Ashgabat|Bishkek|Dushanbe)$/;
const ASIA_SE =
  /^(Singapore|Kuala_Lumpur|Kuching|Jakarta|Pontianak|Makassar|Jayapura|Bangkok|Ho_Chi_Minh|Saigon|Manila|Phnom_Penh|Vientiane|Yangon|Rangoon|Brunei|Dili)$/;

/** The UTC offset of `tz` now, in minutes, or null where the runtime can't say. */
function offsetMinutes(tz: string, at: Date): number | null {
  try {
    const part = new Intl.DateTimeFormat("en-US", { timeZone: tz, timeZoneName: "shortOffset" })
      .formatToParts(at)
      .find((p) => p.type === "timeZoneName")?.value;
    if (!part) return null;
    if (part === "GMT" || part === "UTC") return 0;
    const m = /^(?:GMT|UTC)([+-])(\d{1,2})(?::(\d{2}))?$/.exec(part);
    if (!m) return null;
    const v = Number(m[2]) * 60 + Number(m[3] ?? 0);
    return m[1] === "-" ? -v : v;
  } catch {
    return null;
  }
}

/** A coarse region from an IANA time zone ("Europe/Berlin" → "eu-west"), or null when it can't
 *  be told (UTC, "Etc/…", an unknown name). Only the default: the host can pick another. */
export function regionFromTimeZone(tz: string | null | undefined, at: Date = new Date()): Region | null {
  if (!tz) return null;
  const [area, ...rest] = tz.split("/");
  const city = rest.join("/");
  switch (area) {
    case "America": {
      if (SOUTH_AM.test(city)) return "south-am";
      if (CENTRAL_AM.test(city)) return "central-am";
      // The US and Canada: Mountain time and west of it is the west (standard time, so summer
      // time changes nothing: the lower of January's and July's offsets).
      const y = at.getUTCFullYear();
      const jan = offsetMinutes(tz, new Date(Date.UTC(y, 0, 15)));
      const jul = offsetMinutes(tz, new Date(Date.UTC(y, 6, 15)));
      if (jan != null && jul != null) return Math.min(jan, jul) <= -7 * 60 ? "na-west" : "na-east";
      return /^(Los_Angeles|Vancouver|Denver|Phoenix|Edmonton|Boise|Anchorage|Juneau|Sitka|Nome|Yakutat|Metlakatla|Whitehorse|Dawson|Dawson_Creek|Fort_Nelson|Creston|Cambridge_Bay|Inuvik|Yellowknife)$/.test(city)
        ? "na-west"
        : "na-east";
    }
    case "US":
      return /^(Pacific|Mountain|Arizona|Alaska|Hawaii|Aleutian)$/.test(city) ? "na-west" : "na-east";
    case "Canada":
      return /^(Pacific|Mountain|Yukon)$/.test(city) ? "na-west" : "na-east";
    case "Europe":
      return EU_EAST.test(city) ? "eu-east" : "eu-west";
    case "Africa":
      return city === "Cairo" ? "middle-east" : "africa";
    case "Asia": {
      if (MIDDLE_EAST.test(city)) return "middle-east";
      if (ASIA_SOUTH.test(city)) return "asia-south";
      if (ASIA_SE.test(city)) return "asia-se";
      // Russia east of the Urals: the offset says (Yekaterinburg +5 is near South Asia's band).
      if (/^(Yekaterinburg|Omsk|Novosibirsk|Barnaul|Tomsk|Novokuznetsk|Krasnoyarsk)$/.test(city)) return "asia-south";
      return "asia-east";
    }
    case "Australia":
      return "oceania";
    case "Pacific":
      return city === "Honolulu" || city === "Johnston" ? "na-west" : "oceania";
    case "Atlantic":
      return /^(Canary|Madeira|Faroe|Faeroe|Reykjavik)$/.test(city) ? "eu-west" : city === "Bermuda" ? "central-am" : null;
    case "Indian":
      return /^(Maldives|Chagos)$/.test(city) ? "asia-south" : /^(Christmas|Cocos)$/.test(city) ? "asia-se" : "africa";
    default:
      return null;
  }
}

/** This device's region from its time zone, or null. */
export function ownRegion(): Region | null {
  try {
    return regionFromTimeZone(Intl.DateTimeFormat().resolvedOptions().timeZone);
  } catch {
    return null;
  }
}

type Tr = (key: string, vars: undefined, fallback: string) => string;

/** A region in words (the same 12 everywhere). */
export function regionLabel(tr: Tr, r: Region): string {
  switch (r) {
    case "na-west":
      return tr("room_region_na_west", undefined, "North America West");
    case "na-east":
      return tr("room_region_na_east", undefined, "North America East");
    case "central-am":
      return tr("room_region_central_am", undefined, "Central America & Caribbean");
    case "south-am":
      return tr("room_region_south_am", undefined, "South America");
    case "eu-west":
      return tr("room_region_eu_west", undefined, "Europe West");
    case "eu-east":
      return tr("room_region_eu_east", undefined, "Europe East");
    case "middle-east":
      return tr("room_region_middle_east", undefined, "Middle East");
    case "africa":
      return tr("room_region_africa", undefined, "Africa");
    case "asia-south":
      return tr("room_region_asia_south", undefined, "South Asia");
    case "asia-east":
      return tr("room_region_asia_east", undefined, "East Asia");
    case "asia-se":
      return tr("room_region_asia_se", undefined, "Southeast Asia");
    case "oceania":
      return tr("room_region_oceania", undefined, "Oceania");
  }
}
