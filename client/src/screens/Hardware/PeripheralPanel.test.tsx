import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import PeripheralPanel from "./PeripheralPanel";

// The front light's controls are offered on every console (#840): they did nothing on a PS5 on
// FW 13.60 but worked for a user on FW 10.01, so they stay, with a note saying so.
describe("PeripheralPanel", () => {
  it("offers the beeper and the front light controls, with the note", () => {
    const html = renderToStaticMarkup(<PeripheralPanel mgmtAddr="192.0.2.5:9114" model={null} />);
    expect(html).toContain(">Beep<");
    for (const label of [
      "Front light and beeper",
      "Light off",
      "Light back to normal",
      "Brightness 1",
      "Brightness 2",
      "Brightness 3",
    ]) {
      expect(html).toContain(label);
    }
    expect(html).toContain("On some consoles the light doesn");
  });
});
