// A region in words, apart from regions.ts so the guest page (which bundles the estimate table)
// doesn't carry the twelve names in 21 languages.

import type { Region } from "./regions";

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
