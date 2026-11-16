import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import PeripheralPanel from "./PeripheralPanel";

// The front light's controls did nothing on a PS5 (#840): only the beeper is offered.
describe("PeripheralPanel", () => {
  it("offers the beeper and no front light controls", () => {
    const html = renderToStaticMarkup(<PeripheralPanel mgmtAddr="192.0.2.5:9114" model={null} />);
    expect(html).toContain("Beeper");
    expect(html).toContain(">Beep<");
    for (const gone of ["Front light", "Light off", "Light back to normal", "Brightness"]) {
      expect(html).not.toContain(gone);
    }
  });
});
