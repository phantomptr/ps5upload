import { describe, expect, it } from "vitest";
import { dataCodewords, encodeQr, qrPath, type Ecl } from "./qr";

/**
 * Known symbols, from another encoder: segno (Python, ISO 18004; v1.6) for the same text, level
 * and byte mode, no ECC boost, once per mask; each symbol hashed (rows of 0/1 joined by "\n").
 * segno's padding was patched to the standard's (7.4.10: zero bits up to the codeword boundary,
 * none when already on it; segno adds a whole 0x00 there, which decoders skip but which changes
 * every pad and ECC codeword after it). The cases cover versions 1–10 (one and several blocks,
 * short and long blocks, version information from 7, the 16-bit length at 10), UTF-8 and both
 * levels. `mask` is the one encodeQr picks (penalty scored with the format bits drawn, as Project
 * Nayuki's encoder does); segno scores before drawing them and picks the same mask in all but the
 * first case (segnoMask). Any mask is a valid symbol; the choice only tunes readability.
 */
const VECTORS: { text: string; ecl: Ecl; version: number; mask: number; segnoMask: number; masks: string[] }[] = [
  {
    text: "HELLO",
    ecl: "M",
    version: 1,
    mask: 4,
    segnoMask: 2,
    masks: [
      "b664c5ebcc2451fb6630e63f516abd72b3cdfbf16b9ceb0d92d226f6a6d0cde0",
      "aac7b8561561aa9ae4f5c8a20a12321e0a1ad77028376a765f63e49f263c210b",
      "1fff7fd781f0f59edd796e0f79b20d68de6ecef41b47571460c4bbfbac1afe97",
      "71bc9ab3ffcaa0d281e4873d98c839634768a360dd4020111d405a3c41752367",
      "c346c75add5698735afe3f7eb4f3e6c57ccefd9563f45f65c6d76aa518fb6f91",
      "ba746fc2cc99bfbc1a9f7ce2b807778e4e5bc573e306afe3e0e27d323ff0b83b",
      "533270fea1ec92a09839f69ab35d0555d27dff9a8d05ad876b51cd5805cd8a62",
      "c59a8159c247ab477a18292aa2f9bfbe4775ec9af09bc6556d3824289c8360e9",
    ],
  },
  {
    // Reference symbols from another encoder, for exactly this text.
    text: "http://192.168.1.20:9180/",
    ecl: "M",
    version: 2,
    mask: 5,
    segnoMask: 5,
    masks: [
      "d73364eec07ec8135fb4c1acadb44cb6a91b2f6c6a33ff0d17957c9727cad25a",
      "6022fca68f1e3f1947b31ef8007fb8b0c7263a8a14f8961ca8f5d2b851551c7b",
      "4eca1ae9e1e35601a26bcf743477488f2588a6947a8092967b7addd7c3b68b4a",
      "92129a2c2efdb6ddbaaf644ac4f0d81db0bd0fa3794f09a1f36ee5f27eb1a19b",
      "5ba05905f6722331fa051ddec476c8ec1c2c9de88093447193f49cdf76ba695a",
      "5585bd73e0949b48311011c07368b388361835d7a20a1e3486d78c6989c8eac4",
      "1215b370d633cb9c6964655154918205fd99350f0c7a070a5416b1f84f3ee8b6",
      "3a554667abdc09ccd971d44b879da8bf3c6a0c604878c1f5ddbad1ba95668920",
    ],
  },
  {
    text: "http://10.0.0.5:9180/",
    ecl: "L",
    version: 2,
    mask: 4,
    segnoMask: 4,
    masks: [
      "14903dfca5a315d59828c527fc731cecaa60cc8ab0737ba40d51a7e4660f03a8",
      "0ce213c069792bddcd7b3a5f1c7f66a9749dcce29f30e65619e1c2f0caeb4ea6",
      "ca464ae10e0fe6725e10cbcc7a36e2536f984712c8ead50dcbae42ab920b88d1",
      "c9396e68b6a285711603f2e0dd3c2e66c2a149306469cda1afb749f0d919acd9",
      "1bb68a0637f9f6c819963b6b8726bbf07642558413419d32a4b22192554b8c4c",
      "779dfae360f8a641f54990e8ea568d155a65ebde77b6fe7f4332fb34ec0b6e37",
      "e891994e2bf4c3bc221414c7b771e0c253d270ea73ec5f743da4375f1684c899",
      "91c514fdaeea869036c4406eb109ef78b2a74a3a2c40781ecb098ce85bfcba21",
    ],
  },
  {
    text: "http://192.168.100.200:9180/?x=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ecl: "M",
    version: 6,
    mask: 2,
    segnoMask: 2,
    masks: [
      "02216116764c964cb4585a49aa490f01cd6457f883d00e9c4bc1524d56264713",
      "82e566dd9289a1386ae287e7cc0925a79b528528b7046bd608e33b5300472d5c",
      "4ba4adef7aa35b1a988c91381b575b10c0e29e611c728fd97d7c14bc1ce0f33b",
      "9b4d492a184ae33b0f939ef33dc4c96cb522c31903757aa5ff2aa88ca53c2581",
      "f8d003d1b8576a8146cb73207765c2c40130576f69ec33db10abd248a4000baa",
      "99388a251735d617aeefb05a9ca33945f9c968c02c5369eaddaab2eca1e78755",
      "c71e09612a929dbe0d3fc161c7ca5a2334820ca6b22172b1e1a485f8fca41f9b",
      "9ceb72c1a6f49ce9cac2193550bf37659268ae90bb8604116a79a03b3c0a98de",
    ],
  },
  {
    text: "https://example.com/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ecl: "M",
    version: 8,
    mask: 2,
    segnoMask: 2,
    masks: [
      "0877803b1f3d4c3edcba07d209edc0403cc75b402d5deefba2152b8aa04dd7ea",
      "9a16397c3a54cc337b5c0335494590bf96d2e03918ddfb6b105a21ac7d5f5b12",
      "de2c22e2d9f9d7d2a4b260792a5baf32f6fedfbc47f1eac8e284c2fc2e25a2d9",
      "708972d60d727af9070a7641d64395756563459ea61a9bae1e9d178eb955ed6c",
      "8cb3b86b7b84c605aa31ffc536e787ec4737f59718c5d06618742c98b69febc5",
      "d980c91a957ddded5140af00a151e9dce37f62dfd9170c765ff2e2de391a5c56",
      "2b2b2f5e7b3d5606f11f9fba41013687d81c8c3c9119e4d44e94674e49f31964",
      "afd86522ff73c44b7d8d15a7d598d124888dbe1c2f83cd11554ad234acf0e2d1",
    ],
  },
  {
    text: "https://example.com/cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    ecl: "M",
    version: 10,
    mask: 2,
    segnoMask: 2,
    masks: [
      "d27c5047e526226b190ae82ee23788b295ae96f49cb452ec3f47a579f53137b7",
      "c1601e9ba0849fdcf1779f3c8845e622961cf027bbfa1d9746f74ae68b6fc0e2",
      "6f81bb63098d7e2d869b1e7a9ed709676df4feb9d6e6c92220436eb11047495e",
      "2038043527c1a5c7f6e8688447e64dc2170302b69b6fcfa265b770e96bc8b746",
      "acfec5a589a923092b72e525f15836b403c28105ba482b11a5ac70aa695d592a",
      "4092429426e1a0e80b3179aa746d8589586ba2667fa35bf72416fe9af61242e0",
      "c65d2ba46d698b1b8c168600b73e7d47c1647d5e38434b9984e4a3a92808e9d2",
      "3ad9cc18428fca4945e75e589fc5b6cfa4a03d91884b4e4e6d4fe22bbb591e1d",
    ],
  },
  {
    text: "Grüße aus 東京 — PS5Upload",
    ecl: "M",
    version: 3,
    mask: 3,
    segnoMask: 3,
    masks: [
      "e76244e75e7474dd5bdcc75d2b04cd54629cb2222478c4e094efac47c3400bdd",
      "ec78a085d9faf026f8a7e9e140277260afd7d29bd992751fc290ac356eb14895",
      "43e67d341c14ff29e2c442259774018962d9664b70ce4737576de0a782967169",
      "566d68565ee4064a9e3fe1f4e71d451301e7164599318bbaa01489422f287fd5",
      "7009b0bf4d2dc0940ec5b6f823bb6e36925fe756269370e13021cc2cb03cbbe4",
      "db9cd19a4661678f10c4373f2cac09f30999b78050bb89bf4b97f39c8967a61f",
      "f68b5e7b615da962432990eb792b0fd50217ac6475866e5761d15aa495ec221e",
      "6efe828e88e67dac751f7fe118a15d83dc7ba9b24fc44b279a6a8f998cabffa8",
    ],
  },
  {
    text: "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    ecl: "L",
    version: 9,
    mask: 0,
    segnoMask: 0,
    masks: [
      "71ddfbc79101aebe329e96f941cecd5e8440af18575c5b44b1700da69a5e03d3",
      "b48fa9361e96e0a2c1d85c50daa22e8e85361b50d6767b4273889544f9a7a8f6",
      "42c3e7947ec6891459e2b23afba7d4002c5530594ec7c0242463484f166f6317",
      "3610cdc901987a502b8c49a588fe648cecde665032f0d313ef9d49f2d5ffcc27",
      "c851a54ca6851a9cb303043c42ba81eef126fc3b0988e265e2e3bd4e6c360d6b",
      "8f9480c5e84832683bd12e715482b0db09f142115727e4c6a1315e6662291beb",
      "1b87fdc0b2042dc60424d3bc6656b9497bc6d88bf67a9e18ca4c0e0db7471cd9",
      "e709b05f425bd871e12fee96a0241edb6011b4afcf7dff7da79712c7f1fda709",
    ],
  },
];

async function digest(modules: boolean[][]): Promise<string> {
  const s = modules.map((row) => row.map((d) => (d ? "1" : "0")).join("")).join("\n");
  const buf = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
  return Array.from(new Uint8Array(buf), (b) => b.toString(16).padStart(2, "0")).join("");
}

describe("QR encoder", () => {
  it.each(VECTORS)("$text ($ecl) is the reference symbol under every mask", async (v) => {
    for (let m = 0; m < 8; m++) {
      const q = encodeQr(v.text, v.ecl, m);
      expect(q.version).toBe(v.version);
      expect(q.size).toBe(v.version * 4 + 17);
      expect(await digest(q.modules), `mask ${m}`).toBe(v.masks[m]);
    }
  });

  it.each(VECTORS)("$text ($ecl) picks its mask by the penalty rules", async (v) => {
    const q = encodeQr(v.text, v.ecl);
    expect(q.mask).toBe(v.mask);
    expect(await digest(q.modules)).toBe(v.masks[v.mask]);
  });

  it("capacities match the standard's tables (byte mode: data codewords)", () => {
    expect(dataCodewords(1, "M")).toBe(16);
    expect(dataCodewords(2, "M")).toBe(28);
    expect(dataCodewords(5, "M")).toBe(86);
    expect(dataCodewords(10, "M")).toBe(216);
    expect(dataCodewords(10, "L")).toBe(274);
  });

  it("refuses text longer than version 10 holds", () => {
    expect(() => encodeQr("x".repeat(214), "M")).toThrow(/too long/);
  });

  it("draws one path square per dark module inside the quiet zone", () => {
    const q = encodeQr("HELLO");
    const dark = q.modules.flat().filter(Boolean).length;
    const path = qrPath(q);
    expect(path.split("z").length - 1).toBe(dark);
    expect(path.startsWith("M4 4h1v1h-1z")).toBe(true); // the finder's top-left corner
  });
});
